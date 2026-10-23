//! Installs Marathon's fonts into Android's system overlay: the generated
//! `fonts.xml`, the host font files it names, and whiteouts for the
//! Android fonts it drops. A state file lists what was installed, so the
//! next run removes exactly that. Every path is opened without following
//! links.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use deckard_host_tools::files;
use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags};
use rustix::io::Errno;

const INPUT: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const NEW_FILE: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const MAX_LIST: u64 = 1024 * 1024;
const MAX_XML: i64 = 16 * 1024 * 1024;
const MANIFEST: &str = "etc/fonts.xml";
const CUSTOMIZATION: &str = "product/etc/fonts_customization.xml";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct Paths {
    pub share: PathBuf,
    pub host_fonts: PathBuf,
    pub overlay_system: PathBuf,
    pub state: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    MissingManifest,
    MissingRequiredFonts(Vec<String>),
}

/// Overlayfs whiteouts, so tests can run unprivileged.
pub trait Whiteouts {
    /// # Errors
    /// The whiteout cannot be created.
    fn create(&mut self, directory: &OwnedFd, name: &OsStr) -> io::Result<()>;
}

pub struct LinuxWhiteouts;

impl Whiteouts for LinuxWhiteouts {
    fn create(&mut self, directory: &OwnedFd, name: &OsStr) -> io::Result<()> {
        let device = rustix::fs::makedev(0, 0);
        Ok(rustix::fs::mknodat(
            directory,
            name,
            FileType::CharacterDevice,
            Mode::empty(),
            device,
        )?)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn open_regular(directory: &OwnedFd, name: &OsStr) -> io::Result<OwnedFd> {
    if !files::is_component(name) {
        return Err(invalid("invalid path component"));
    }
    let file = rustix::fs::openat(directory, name, INPUT, Mode::empty())?;
    if FileType::from_raw_mode(rustix::fs::fstat(&file)?.st_mode) != FileType::RegularFile {
        return Err(invalid("expected a regular file"));
    }
    Ok(file)
}

fn read_limited(file: OwnedFd) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::from(file)
        .take(MAX_LIST + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LIST {
        return Err(invalid("input exceeds size limit"));
    }
    String::from_utf8(bytes).map_err(|_| invalid("input is not UTF-8"))
}

fn basename(value: &str) -> io::Result<&str> {
    if !files::is_component(OsStr::new(value)) || value.contains(['\n', '\r']) {
        return Err(invalid("invalid font name"));
    }
    Ok(value)
}

/// The font names of a share list, each optionally under `prefix`.
fn list(share: &OwnedFd, name: &str, prefix: Option<&str>) -> io::Result<Vec<String>> {
    let text = read_limited(open_regular(share, OsStr::new(name))?)?;
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        basename(
            prefix
                .and_then(|prefix| line.strip_prefix(prefix))
                .unwrap_or(line),
        )?;
        if !seen.insert(line) {
            return Err(invalid("duplicate manifest entry"));
        }
    }
    Ok(text.lines().map(str::to_owned).collect())
}

/// An installed entry's directory below the overlay and its file name.
fn state_path(line: &str) -> io::Result<(&str, &str)> {
    match line {
        MANIFEST => Ok(("etc", "fonts.xml")),
        CUSTOMIZATION => Ok(("product/etc", "fonts_customization.xml")),
        _ => match (
            line.strip_prefix("fonts/"),
            line.strip_prefix("product/fonts/"),
        ) {
            (Some(name), _) => Ok(("fonts", basename(name)?)),
            (None, Some(name)) => Ok(("product/fonts", basename(name)?)),
            (None, None) => Err(invalid("unsafe state entry")),
        },
    }
}

fn state_entries(directory: &OwnedFd, name: &OsStr) -> io::Result<Vec<String>> {
    let Some(file) = files::found(open_regular(directory, name))? else {
        return Ok(Vec::new());
    };
    let text = read_limited(file)?;
    for line in text.lines() {
        state_path(line)?;
    }
    Ok(text.lines().map(str::to_owned).collect())
}

/// `parent` below the overlay, created as needed when `create`.
fn destination(system: &OwnedFd, parent: &str, create: Create) -> io::Result<OwnedFd> {
    let mut directory = rustix::io::dup(system)?;
    for name in parent.split('/') {
        let child = rustix::fs::openat(&directory, name, files::DIRECTORY, Mode::empty());
        directory = match child {
            Err(Errno::NOENT) if create == Create::Yes => {
                rustix::fs::mkdirat(&directory, name, Mode::from_raw_mode(0o755))?;
                rustix::fs::openat(&directory, name, files::DIRECTORY, Mode::empty())?
            }
            child => child?,
        };
    }
    Ok(directory)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Create {
    Yes,
    No,
}

fn remove_entry(system: &OwnedFd, entry: &str) -> io::Result<()> {
    let (parent, name) = state_path(entry)?;
    let Some(directory) = files::found(destination(system, parent, Create::No))? else {
        return Ok(());
    };
    files::found(rustix::fs::unlinkat(&directory, name, AtFlags::empty()).map_err(Into::into))
        .map(drop)
}

/// The first regular file for each wanted name, searching `directory`'s
/// tree in name order.
fn find_fonts(
    directory: &OwnedFd,
    wanted: &BTreeSet<String>,
    found: &mut BTreeMap<String, OwnedFd>,
) -> io::Result<()> {
    for name in files::children(directory)? {
        if let Ok(subdirectory) =
            rustix::fs::openat(directory, &name, files::DIRECTORY, Mode::empty())
        {
            find_fonts(&subdirectory, wanted, found)?;
            continue;
        }
        let Some(font) = name
            .to_str()
            .filter(|name| wanted.contains(*name) && !found.contains_key(*name))
        else {
            continue;
        };
        match open_regular(directory, OsStr::new(font)) {
            Ok(file) => {
                found.insert(font.to_owned(), file);
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(Errno::LOOP.raw_os_error()) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn scan_fonts(
    root: &Path,
    wanted: &[String],
    found: &mut BTreeMap<String, OwnedFd>,
) -> io::Result<()> {
    let wanted = wanted.iter().cloned().collect();
    match files::found(files::open_dir(root))? {
        Some(directory) => find_fonts(&directory, &wanted, found),
        None => Ok(()),
    }
}

fn copy_file(source: &OwnedFd, system: &OwnedFd, entry: &str) -> io::Result<()> {
    let (parent, name) = state_path(entry)?;
    let directory = destination(system, parent, Create::Yes)?;
    remove_entry(system, entry)?;
    let output = rustix::fs::openat(&directory, name, NEW_FILE, Mode::from_raw_mode(0o644))?;
    rustix::fs::fchmod(&output, Mode::from_raw_mode(0o644))?;
    io::copy(
        &mut File::from(rustix::io::dup(source)?),
        &mut File::from(output),
    )?;
    Ok(())
}

fn sync_dir(directory: &OwnedFd) -> io::Result<()> {
    File::from(rustix::io::dup(directory)?).sync_all()
}

fn remove_state(directory: &OwnedFd, name: &OsStr) -> io::Result<()> {
    if files::found(rustix::fs::unlinkat(directory, name, AtFlags::empty()).map_err(Into::into))?
        .is_some()
    {
        sync_dir(directory)?;
    }
    Ok(())
}

/// The entries to install, in order: copied fonts, whiteouts, then the XML.
fn plan(
    required: &[String],
    optional: &[String],
    hidden: &[String],
    found: &BTreeMap<String, OwnedFd>,
) -> io::Result<Vec<String>> {
    let mut entries = Vec::new();
    let mut unique = BTreeSet::new();
    let copied = optional.iter().filter(|name| found.contains_key(*name));
    for font in required.iter().chain(copied) {
        let entry = format!("fonts/{font}");
        if unique.insert(entry.clone()) {
            entries.push(entry);
        }
    }
    for font in hidden {
        let entry = match font.strip_prefix("product/") {
            Some(name) => format!("product/fonts/{name}"),
            None => format!("fonts/{font}"),
        };
        if !unique.insert(entry.clone()) {
            return Err(invalid("font is both copied and hidden"));
        }
        entries.push(entry);
    }
    entries.extend([MANIFEST.to_owned(), CUSTOMIZATION.to_owned()]);
    Ok(entries)
}

/// Reconciles the overlay with the share and the host's fonts.
///
/// # Errors
/// A path is unsafe or a list is malformed (nothing is removed then), or a
/// write fails (what this run installed is removed again).
pub fn sync(paths: &Paths, whiteouts: &mut impl Whiteouts) -> io::Result<Outcome> {
    let system = files::open_dir(&paths.overlay_system)?;
    let state_dir = files::open_dir(
        paths
            .state
            .parent()
            .ok_or_else(|| invalid("state has no parent"))?,
    )?;
    rustix::fs::flock(&state_dir, FlockOperation::LockExclusive)?;
    let state_name = paths
        .state
        .file_name()
        .ok_or_else(|| invalid("state has no name"))?;
    if !files::is_component(state_name) {
        return Err(invalid("invalid path component"));
    }
    let old = state_entries(&state_dir, state_name)?;
    let uninstall = |outcome: Outcome| -> io::Result<Outcome> {
        for entry in &old {
            remove_entry(&system, entry)?;
        }
        remove_state(&state_dir, state_name)?;
        Ok(outcome)
    };
    let share = files::found(files::open_dir(&paths.share))?;
    let manifest = match &share {
        Some(share) => files::found(open_regular(share, OsStr::new("fonts.xml")))?,
        None => None,
    };
    let (Some(share), Some(manifest)) = (share, manifest) else {
        return uninstall(Outcome::MissingManifest);
    };
    let required = list(&share, "host-fonts.txt", None)?;
    let mut found = BTreeMap::new();
    scan_fonts(&paths.host_fonts, &required, &mut found)?;
    let missing: Vec<String> = required
        .iter()
        .filter(|name| !found.contains_key(*name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return uninstall(Outcome::MissingRequiredFonts(missing));
    }
    let optional = list(&share, "android-fonts.txt", None)?;
    let hidden = list(&share, "hidden-fonts.txt", Some("product/"))?;
    let customization = open_regular(&share, OsStr::new("fonts_customization.xml"))?;
    if rustix::fs::fstat(&manifest)?.st_size > MAX_XML
        || rustix::fs::fstat(&customization)?.st_size > MAX_XML
    {
        return Err(invalid("XML exceeds size limit"));
    }
    scan_fonts(&paths.host_fonts, &optional, &mut found)?;
    let entries = plan(&required, &optional, &hidden, &found)?;

    let (temp_name, temp) = loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = OsString::from(format!(
            ".deckard-fonts.{}.{sequence}.new",
            std::process::id()
        ));
        match rustix::fs::openat(&state_dir, &name, NEW_FILE, Mode::from_raw_mode(0o600)) {
            Err(Errno::EXIST) => {}
            file => break (name, file?),
        }
    };
    let install = || -> io::Result<()> {
        for entry in &old {
            remove_entry(&system, entry)?;
        }
        for entry in &entries {
            let font = entry
                .strip_prefix("fonts/")
                .and_then(|name| found.get(name));
            match (font, entry.as_str()) {
                (Some(source), _) => copy_file(source, &system, entry)?,
                (None, MANIFEST) => copy_file(&manifest, &system, entry)?,
                (None, CUSTOMIZATION) => copy_file(&customization, &system, entry)?,
                (None, _) => {
                    let (parent, name) = state_path(entry)?;
                    let directory = destination(&system, parent, Create::Yes)?;
                    remove_entry(&system, entry)?;
                    whiteouts.create(&directory, OsStr::new(name))?;
                }
            }
        }
        let mut state = File::from(temp);
        for entry in &entries {
            writeln!(state, "{entry}")?;
        }
        state.sync_all()?;
        Ok(rustix::fs::renameat(
            &state_dir, &temp_name, &state_dir, state_name,
        )?)
    };
    if let Err(error) = install() {
        for entry in &entries {
            let _ = remove_entry(&system, entry);
        }
        let _ = rustix::fs::unlinkat(&state_dir, &temp_name, AtFlags::empty());
        return Err(error);
    }
    sync_dir(&state_dir)?;
    Ok(Outcome::Applied)
}
