//! The LXC mount hook that shares the user's Documents, Download, Pictures,
//! Music, Movies and DCIM folders with Android's shared storage.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags};
use rustix::process::{Gid, Uid};

use crate::files;

pub const MEDIA_RW: u32 = 1023;
const FILE: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const FOLDERS: [(&str, &str, &str); 6] = [
    ("Documents", "XDG_DOCUMENTS_DIR", "Documents"),
    ("Download", "XDG_DOWNLOAD_DIR", "Downloads"),
    ("Pictures", "XDG_PICTURES_DIR", "Pictures"),
    ("Music", "XDG_MUSIC_DIR", "Music"),
    ("Movies", "XDG_VIDEOS_DIR", "Videos"),
    ("DCIM", "", "DCIM"),
];

/// The privileged steps, so the hook can be tested unprivileged.
pub trait Operations {
    /// # Errors
    /// The owner cannot be changed.
    fn chown(&mut self, descriptor: &OwnedFd, uid: u32, gid: u32) -> io::Result<()>;
    /// # Errors
    /// The bind mount fails.
    fn bind(&mut self, source: &OwnedFd, target: &OwnedFd) -> io::Result<()>;
}

pub struct Linux;

impl Operations for Linux {
    fn chown(&mut self, descriptor: &OwnedFd, uid: u32, gid: u32) -> io::Result<()> {
        if uid == u32::MAX || gid == u32::MAX {
            return Err(files::invalid("invalid owner"));
        }
        Ok(rustix::fs::fchown(
            descriptor,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )?)
    }

    fn bind(&mut self, source: &OwnedFd, target: &OwnedFd) -> io::Result<()> {
        let path = |fd: &OwnedFd| format!("/proc/self/fd/{}", fd.as_raw_fd());
        Ok(rustix::mount::mount_bind(path(source), path(target))?)
    }
}

#[derive(Default, Debug)]
pub struct Report {
    pub shared: Vec<String>,
    pub warnings: Vec<String>,
}

fn open_directory(parent: impl AsFd, name: &OsStr) -> io::Result<OwnedFd> {
    if !files::is_component(name) {
        return Err(files::invalid("invalid path component"));
    }
    Ok(rustix::fs::openat(
        parent,
        name,
        files::DIRECTORY,
        Mode::empty(),
    )?)
}

fn create_directory(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
    mode: u32,
    operations: &mut impl Operations,
) -> io::Result<OwnedFd> {
    if let Some(directory) = files::found(open_directory(parent, OsStr::new(name)))? {
        return Ok(directory);
    }
    rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(mode))?;
    let directory = open_directory(parent, OsStr::new(name))?;
    operations.chown(&directory, owner.0, owner.1)?;
    rustix::fs::fchmod(&directory, Mode::from_raw_mode(mode))?;
    Ok(directory)
}

fn read_small(descriptor: OwnedFd, limit: u64) -> io::Result<String> {
    let mut text = String::new();
    std::fs::File::from(descriptor)
        .take(limit)
        .read_to_string(&mut text)?;
    Ok(text)
}

/// The host path LXC binds over Android's `/data`.
#[must_use]
pub fn session_data(configuration: &str) -> Option<&str> {
    let mut entries = configuration
        .lines()
        .filter(|line| line.starts_with("lxc.mount.entry"));
    entries.find_map(|line| {
        let fields: Vec<_> = line.split_once('=')?.1.split_whitespace().collect();
        (fields.len() >= 4 && fields[1] == "data").then_some(fields[0])
    })
}

/// The `XDG_*_DIR` folders of `user-dirs.dirs` that name a direct child of
/// the home.
#[must_use]
pub fn user_directories(configuration: &str) -> BTreeMap<String, String> {
    let mut directories = BTreeMap::new();
    for line in configuration.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let known = !key.is_empty() && FOLDERS.iter().any(|(_, candidate, _)| key == *candidate);
        let name = value
            .trim_end()
            .strip_prefix("\"$HOME/")
            .and_then(|name| name.strip_suffix('"'));
        if let Some(name) = name
            && known
            && files::is_component(OsStr::new(name))
        {
            directories.insert(key.to_owned(), name.to_owned());
        }
    }
    directories
}

fn directories_from_home(home: &OwnedFd) -> BTreeMap<String, String> {
    let read = || -> io::Result<String> {
        let config = open_directory(home, OsStr::new(".config"))?;
        let file = rustix::fs::openat(config, "user-dirs.dirs", FILE, Mode::empty())?;
        if FileType::from_raw_mode(rustix::fs::fstat(&file)?.st_mode) != FileType::RegularFile {
            return Err(files::invalid(
                "user directories configuration is not a regular file",
            ));
        }
        read_small(file, 16384)
    };
    read().map_or_else(|_| BTreeMap::new(), |text| user_directories(&text))
}

/// The home folder behind each shared Android folder, as the hook binds
/// them: (Android name, host path).
#[must_use]
pub fn host_folders(home: &Path) -> Vec<(&'static str, PathBuf)> {
    let names =
        files::open_dir(home).map_or_else(|_| BTreeMap::new(), |fd| directories_from_home(&fd));
    let mut folders = Vec::with_capacity(FOLDERS.len());
    for (android, key, default) in FOLDERS {
        folders.push((
            android,
            home.join(names.get(key).map_or(default, String::as_str)),
        ));
    }
    folders
}

/// Gives a migrated file or tree to `uid` and Android's media group.
fn fix_tree(
    parent: &OwnedFd,
    name: &OsStr,
    uid: u32,
    depth: usize,
    operations: &mut impl Operations,
) -> io::Result<()> {
    if depth > 128 {
        return Err(files::invalid("shared folder nesting exceeds 128 levels"));
    }
    let metadata = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    let (flags, bits) = match FileType::from_raw_mode(metadata.st_mode) {
        FileType::Directory => (FILE | OFlags::DIRECTORY, 0o770),
        FileType::RegularFile => (FILE, 0o660),
        _ => return Ok(()),
    };
    let descriptor = rustix::fs::openat(parent, name, flags, Mode::empty())?;
    let pinned = rustix::fs::fstat(&descriptor)?;
    if (metadata.st_ino, metadata.st_dev) != (pinned.st_ino, pinned.st_dev) {
        return Err(files::invalid("shared file changed while opening it"));
    }
    operations.chown(&descriptor, uid, MEDIA_RW)?;
    rustix::fs::fchmod(&descriptor, Mode::from_raw_mode(pinned.st_mode | bits))?;
    if bits == 0o770 {
        for child in files::children(&descriptor)? {
            fix_tree(&descriptor, &child, uid, depth + 1, operations)?;
        }
    }
    Ok(())
}

/// Moves what Android already stored into the host folder, keeping any
/// host file of the same name.
fn migrate(
    android: &OwnedFd,
    host: &OwnedFd,
    uid: u32,
    operations: &mut impl Operations,
    warnings: &mut Vec<String>,
) -> io::Result<()> {
    for name in files::children(android)? {
        if let Err(error) =
            rustix::fs::renameat_with(android, &name, host, &name, RenameFlags::NOREPLACE)
        {
            warnings.push(format!("kept {name:?} in Android: {error}"));
        } else if let Err(error) = fix_tree(host, &name, uid, 0, operations) {
            warnings.push(format!(
                "could not set migrated file ownership for {name:?}: {error}"
            ));
        }
    }
    Ok(())
}

/// Binds each home folder over its Android shared-storage folder in the
/// mounted rootfs, creating both as needed.
///
/// # Errors
/// The home, data or rootfs is foreign or unsafe; per-folder failures are
/// warnings in the report.
pub fn share(
    rootfs: &Path,
    home: &Path,
    data: &Path,
    uid: u32,
    gid: u32,
    operations: &mut impl Operations,
) -> io::Result<Report> {
    let home = files::open_dir(home)?;
    let home_stat = rustix::fs::fstat(&home)?;
    if home_stat.st_uid != uid {
        return Err(files::invalid("home directory has a different owner"));
    }
    let data = files::open_dir(data)?;
    let data_stat = rustix::fs::fstat(&data)?;
    if data_stat.st_uid != uid {
        return Err(files::invalid("Android data has a different owner"));
    }
    if (data_stat.st_dev, data_stat.st_ino) == (home_stat.st_dev, home_stat.st_ino) {
        return Err(files::invalid("Android data cannot be the user's home"));
    }
    let root = files::open_dir(rootfs)?;
    let media_rw = (MEDIA_RW, MEDIA_RW);
    let media = create_directory(&data, "media", media_rw, 0o770, operations)?;
    let user0 = create_directory(&media, "0", media_rw, 0o770, operations)?;
    let mounted = files::open_dir_at(&root, Path::new("data/media/0"))?;
    let names = directories_from_home(&home);
    let mut report = Report::default();
    for (android_name, key, default) in FOLDERS {
        let name = names.get(key).map_or(default, String::as_str);
        let mut map_folder = || -> io::Result<()> {
            let host = create_directory(&home, name, (uid, gid), 0o755, operations)?;
            if rustix::fs::fstat(&host)?.st_uid != uid {
                return Err(files::invalid("shared folder has a different owner"));
            }
            let android = create_directory(&user0, android_name, media_rw, 0o770, operations)?;
            let target = open_directory(&mounted, OsStr::new(android_name))?;
            let (original, container) = (rustix::fs::fstat(&android)?, rustix::fs::fstat(&target)?);
            if (original.st_ino, original.st_dev) != (container.st_ino, container.st_dev) {
                return Err(files::invalid(
                    "container data does not match the user's data",
                ));
            }
            migrate(&android, &host, uid, operations, &mut report.warnings)?;
            operations.bind(&host, &target)
        };
        match map_folder() {
            Ok(()) => report.shared.push(android_name.to_owned()),
            Err(error) => report
                .warnings
                .push(format!("{android_name} not shared: {error}")),
        }
    }
    Ok(report)
}

/// The hook's entry point: LXC names the rootfs and config in the
/// environment.
///
/// # Errors
/// Not root, or the session's data or user cannot be found.
pub fn run_hook() -> Result<Report, Box<dyn std::error::Error>> {
    let (Some(rootfs), Some(config)) = (
        std::env::var_os("LXC_ROOTFS_MOUNT"),
        std::env::var_os("LXC_CONFIG_FILE"),
    ) else {
        return Ok(Report::default());
    };
    if !rustix::process::geteuid().is_root() {
        return Err("LXC shared-folder hook requires root".into());
    }
    let directory = Path::new(&config)
        .parent()
        .ok_or("invalid LXC configuration path")?;
    let session = rustix::fs::openat(
        files::open_dir(directory)?,
        "config_session",
        FILE,
        Mode::empty(),
    )?;
    let configuration = read_small(session, 65536)?;
    let data = Path::new(session_data(&configuration).ok_or("no Android session data bind")?);
    let owner = rustix::fs::fstat(files::open_dir(data)?)?.st_uid;
    if owner == 0 {
        return Ok(Report::default());
    }
    let user = nix::unistd::User::from_uid(owner.into())?.ok_or("unknown session user")?;
    Ok(share(
        Path::new(&rootfs),
        &user.dir,
        data,
        owner,
        user.gid.as_raw(),
        &mut Linux,
    )?)
}
