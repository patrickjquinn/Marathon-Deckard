//! Resets Android for one user: removes their Android data so the next start
//! sets Android up again. Images and Deckard's overlay stay.
//!
//! Runs as root through pkexec, by deckardd after Settings' confirmation. In
//! the data directory, `deckard/` (deckardd's sockets and the files it writes
//! for Android) is kept, minus the files that crossed (`xfer/`, `install/`).
//!
//! Every removal is relative to a directory descriptor opened without
//! following symlinks, so a link swapped in while the reset runs cannot send
//! it outside the user's data.

use std::ffi::{CStr, OsStr, OsString};
use std::fmt;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use rustix::fs::{AtFlags, FileType, Mode, OFlags};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// The container units, the current one first. Whichever is active owns the
/// container.
const UNITS: [&str; 2] = ["deckard-container.service", "waydroid-container.service"];

/// The container identities LXC may know: (name, configuration path).
const CONTAINERS: [(&str, &str); 2] = [
    ("deckard", "/var/lib/deckard/lxc"),
    ("waydroid", "/var/lib/waydroid/lxc"),
];

/// Where a user's Android data lives, under their home: Deckard's path, then
/// the one it is migrating from.
const DATA: [&str; 2] = [".local/share/deckard/data", ".local/share/waydroid/data"];

/// Why a reset did not happen, and the helper's exit status for it.
#[derive(Debug)]
pub enum Error {
    /// Status 2: called wrongly (arguments, no pkexec caller, root's data).
    Usage(String),
    /// Status 3: the user has no Android data.
    NoData(String),
    /// Status 1: anything else.
    Failed(String),
}

impl Error {
    #[must_use]
    pub fn status(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::NoData(_) => 3,
            Self::Failed(_) => 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) | Self::NoData(message) | Self::Failed(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for Error {}

fn failed(context: &str, error: &io::Error) -> Error {
    Error::Failed(format!("{context}: {error}"))
}

/// The environment the helper reads; tests point every external at fakes.
#[derive(Debug, Default, Clone)]
pub struct Environment {
    /// `PKEXEC_UID`, else `DECKARD_RESET_UID`.
    pub uid: Option<String>,
    /// A passwd file to read instead of the system's user database.
    pub passwd: Option<PathBuf>,
    pub systemctl: Option<PathBuf>,
    pub lxc_info: Option<PathBuf>,
    /// The data directory, instead of the user's own.
    pub data: Option<PathBuf>,
    /// The desktop entries' directory, instead of the user's own.
    pub applications: Option<PathBuf>,
}

impl Environment {
    #[must_use]
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Self {
            uid: std::env::var("PKEXEC_UID")
                .or_else(|_| std::env::var("DECKARD_RESET_UID"))
                .ok(),
            passwd: var("DECKARD_RESET_PASSWD").map(PathBuf::from),
            systemctl: var("DECKARD_RESET_SYSTEMCTL").map(PathBuf::from),
            lxc_info: var("DECKARD_RESET_LXC_INFO").map(PathBuf::from),
            data: var("DECKARD_RESET_DATA").map(PathBuf::from),
            applications: var("DECKARD_RESET_APPS").map(PathBuf::from),
        }
    }

    fn systemctl(&self) -> Command {
        Command::new(self.systemctl.as_deref().unwrap_or(Path::new("systemctl")))
    }
}

fn caller(environment: &Environment) -> Result<u32, Error> {
    let Some(uid) = environment.uid.as_deref().filter(|uid| !uid.is_empty()) else {
        return Err(Error::Usage("run through pkexec".into()));
    };
    let uid: u32 = uid
        .parse()
        .map_err(|_| Error::Usage("run through pkexec".into()))?;
    if uid == 0 {
        return Err(Error::Usage("not for root's data".into()));
    }
    Ok(uid)
}

fn home_of(uid: u32, passwd: Option<&Path>) -> Result<PathBuf, Error> {
    let home = match passwd {
        Some(path) => {
            let text =
                std::fs::read_to_string(path).map_err(|e| failed("cannot read passwd", &e))?;
            text.lines().find_map(|line| {
                let fields: Vec<&str> = line.split(':').collect();
                (fields.len() >= 6 && fields[2] == uid.to_string())
                    .then(|| PathBuf::from(fields[5]))
            })
        }
        None => nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
            .ok()
            .flatten()
            .map(|user| user.dir),
    };
    let home = home
        .filter(|home| home.is_absolute() && home != Path::new("/"))
        .ok_or_else(|| Error::Failed(format!("no home for uid {uid}")))?;
    // The home itself may be reached through links (/home on some systems);
    // everything under it is opened without following any.
    std::fs::canonicalize(&home).map_err(|e| failed(&format!("no home at {}", home.display()), &e))
}

/// Opens `path` one component at a time from `/`, never following a link.
fn open_absolute(path: &Path) -> io::Result<OwnedFd> {
    let mut current = rustix::fs::open("/", DIRECTORY, Mode::empty())?;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = rustix::fs::openat(&current, name, DIRECTORY, Mode::empty())?;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a plain path",
                ));
            }
        }
    }
    Ok(current)
}

/// The data directory, opened below the home without following links, and
/// the user's own.
fn open_data(uid: u32, home: &Path, data: Option<&Path>) -> Result<(PathBuf, OwnedFd), Error> {
    let candidates: Vec<PathBuf> = match data {
        Some(data) => vec![data.to_path_buf()],
        None => DATA.iter().map(|relative| home.join(relative)).collect(),
    };
    let Some(path) = candidates
        .into_iter()
        .find(|path| path.symlink_metadata().is_ok())
    else {
        return Err(Error::NoData(format!(
            "no Android data at {}",
            home.join(DATA[1]).display()
        )));
    };
    let inside = path
        .strip_prefix(home)
        .ok()
        .filter(|relative| relative.components().next().is_some());
    if inside.is_none() {
        return Err(Error::Failed(format!(
            "{} is outside {}",
            path.display(),
            home.display()
        )));
    }
    let directory = open_absolute(&path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => Error::NoData(format!("no Android data at {}", path.display())),
        _ => failed(
            &format!(
                "{} is not a plain directory in {}",
                path.display(),
                home.display()
            ),
            &e,
        ),
    })?;
    let owner = rustix::fs::fstat(&directory)
        .map_err(|e| failed("cannot inspect the data", &io::Error::from(e)))?
        .st_uid;
    if owner != uid {
        return Err(Error::Failed(format!(
            "{} does not belong to uid {uid}",
            path.display()
        )));
    }
    Ok((path, directory))
}

fn unit_active(environment: &Environment, unit: &str) -> bool {
    environment
        .systemctl()
        .args(["is-active", "--quiet", unit])
        .status()
        .is_ok_and(|status| status.success())
}

/// Stops whichever unit runs the container; the ones that were running.
fn stop_container(environment: &Environment) -> Result<Vec<&'static str>, Error> {
    let running: Vec<&'static str> = UNITS
        .into_iter()
        .filter(|unit| unit_active(environment, unit))
        .collect();
    for unit in &running {
        let stopped = environment.systemctl().args(["stop", unit]).status();
        if !stopped.is_ok_and(|status| status.success()) {
            return Err(Error::Failed(format!("cannot stop {unit}")));
        }
    }
    if let Some(unit) = UNITS
        .into_iter()
        .find(|unit| unit_active(environment, unit))
    {
        return Err(Error::Failed(format!("{unit} is still active")));
    }
    for (name, path) in CONTAINERS {
        if container_running(environment, name, path) {
            return Err(Error::Failed(format!("container {name} is still running")));
        }
    }
    Ok(running)
}

/// Running unless LXC reports it stopped or knows no such container.
fn container_running(environment: &Environment, name: &str, path: &str) -> bool {
    if environment.lxc_info.is_none() && !Path::new(path).exists() {
        return false;
    }
    let program = environment
        .lxc_info
        .as_deref()
        .unwrap_or(Path::new("lxc-info"));
    let output = Command::new(program)
        .args(["-n", name, "-P", path, "-s"])
        .output();
    let Ok(output) = output else { return true };
    if !output.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let stopped = text
        .lines()
        .filter_map(|line| line.strip_prefix("State:"))
        .any(|state| state.trim() == "STOPPED");
    !stopped
}

fn children(directory: &OwnedFd) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(directory)? {
        let entry = entry?;
        let name: &CStr = entry.file_name();
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(OsStr::from_bytes(name.to_bytes()).to_os_string());
        }
    }
    Ok(names)
}

/// Removes `name` below `parent`: a directory's contents first, nothing
/// through a link.
fn remove(parent: &OwnedFd, name: &OsStr) -> io::Result<()> {
    let stat = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Ok(rustix::fs::unlinkat(parent, name, AtFlags::empty())?);
    }
    let directory = rustix::fs::openat(parent, name, DIRECTORY, Mode::empty())?;
    clear(&directory)?;
    Ok(rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?)
}

/// Removes everything in `directory`, carrying on past failures; the first.
fn clear(directory: &OwnedFd) -> io::Result<()> {
    let mut first_error = None;
    for name in children(directory)? {
        if let Err(error) = remove(directory, &name) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn is_plain_directory(parent: &OwnedFd, name: &str) -> bool {
    rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
}

/// Android's data goes; `deckard/` stays without the files that crossed.
fn clear_data(data: &OwnedFd) -> bool {
    let Ok(names) = children(data) else {
        return false;
    };
    let mut complete = true;
    for name in names {
        if name == "deckard" && is_plain_directory(data, "deckard") {
            continue;
        }
        complete &= remove(data, &name).is_ok();
    }
    if is_plain_directory(data, "deckard") {
        let Ok(deckard) = rustix::fs::openat(data, "deckard", DIRECTORY, Mode::empty()) else {
            return false;
        };
        for crossed in ["xfer", "install"] {
            match remove(&deckard, OsStr::new(crossed)) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => complete = false,
                _ => {}
            }
        }
    }
    complete
}

/// The desktop entries of the apps that are gone, under either identity.
fn clear_entries(home: &Path, applications: Option<&Path>) {
    let path =
        applications.map_or_else(|| home.join(".local/share/applications"), Path::to_path_buf);
    let Ok(directory) = open_absolute(&path) else {
        return;
    };
    let Ok(names) = children(&directory) else {
        return;
    };
    for name in names {
        let bytes = name.as_bytes();
        let android = (bytes.starts_with(b"deckard.") || bytes.starts_with(b"waydroid."))
            && bytes.ends_with(b".desktop");
        let regular = rustix::fs::statat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile);
        if android && regular {
            let _ = rustix::fs::unlinkat(&directory, &name, AtFlags::empty());
        }
    }
}

/// Resets Android for the pkexec caller.
///
/// # Errors
///
/// [`Error::Usage`] when not called through pkexec for a non-root user,
/// [`Error::NoData`] when there is nothing to remove, and [`Error::Failed`]
/// for everything else, including a container that would not stop (nothing
/// is removed then).
pub fn run(arguments: &[OsString], environment: &Environment) -> Result<(), Error> {
    if !arguments.is_empty() {
        return Err(Error::Usage("usage: deckard-reset".into()));
    }
    let uid = caller(environment)?;
    let home = home_of(uid, environment.passwd.as_deref())?;
    let (path, data) = open_data(uid, &home, environment.data.as_deref())?;
    let running = stop_container(environment)?;
    let complete = clear_data(&data);
    clear_entries(&home, environment.applications.as_deref());
    for unit in running {
        let started = environment.systemctl().args(["start", unit]).status();
        if !started.is_ok_and(|status| status.success()) {
            return Err(Error::Failed(format!("cannot start {unit} again")));
        }
    }
    if !complete {
        return Err(Error::Failed(format!(
            "some of {} could not be removed",
            path.display()
        )));
    }
    Ok(())
}
