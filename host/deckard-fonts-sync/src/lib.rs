use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
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
const MAX_XML: u64 = 16 * 1024 * 1024;
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

pub trait Whiteouts {
    fn create(&mut self, directory: &OwnedFd, name: &OsStr) -> io::Result<()>;
}

pub struct LinuxWhiteouts;

impl Whiteouts for LinuxWhiteouts {
    fn create(&mut self, directory: &OwnedFd, name: &OsStr) -> io::Result<()> {
        fs::mknodat(
            directory,
            name,
            FileType::CharacterDevice,
            Mode::empty(),
            fs::makedev(0, 0),
        )?;
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn component(name: &OsStr) -> io::Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        Err(invalid("invalid path component"))
    } else {
        Ok(())
    }
}

fn walk_absolute(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(invalid("path must be absolute"));
    }
    let mut directory = fs::open("/", DIRECTORY, Mode::empty())?;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = fs::openat(&directory, name, DIRECTORY, Mode::empty())?
            }
            _ => return Err(invalid("path contains non-normal component")),
        }
    }
    Ok(directory)
}

fn open_regular(directory: &OwnedFd, name: &OsStr) -> io::Result<OwnedFd> {
    component(name)?;
    let file = fs::openat(directory, name, INPUT, Mode::empty())?;
    if FileType::from_raw_mode(fs::fstat(&file)?.st_mode) != FileType::RegularFile {
        return Err(invalid("expected a regular file"));
    }
    Ok(file)
}

fn read_limited(file: OwnedFd, limit: u64) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::from(file).take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("input exceeds size limit"));
    }
    String::from_utf8(bytes).map_err(|_| invalid("input is not UTF-8"))
}

fn read_named(directory: &OwnedFd, name: &str, limit: u64) -> io::Result<String> {
    read_limited(open_regular(directory, OsStr::new(name))?, limit)
}

fn basename(value: &str) -> io::Result<&str> {
    component(OsStr::new(value))?;
    if value.contains('\n') || value.contains('\r') {
        return Err(invalid("invalid font name"));
    }
    Ok(value)
}

fn list(directory: &OwnedFd, name: &str, product_prefix: bool) -> io::Result<Vec<String>> {
    let text = read_named(directory, name, MAX_LIST)?;
    let mut values = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let value = if product_prefix && line.starts_with("product/") {
            basename(&line["product/".len()..])?;
            line
        } else {
            basename(line)?;
            line
        };
        if !seen.insert(value.to_owned()) {
            return Err(invalid("duplicate manifest entry"));
        }
        values.push(value.to_owned());
    }
    Ok(values)
}

fn state_path(line: &str) -> io::Result<(&str, &str)> {
    match line {
        "etc/fonts.xml" => Ok(("etc", "fonts.xml")),
        "product/etc/fonts_customization.xml" => Ok(("product/etc", "fonts_customization.xml")),
        _ => {
            if let Some(name) = line.strip_prefix("fonts/") {
                return Ok(("fonts", basename(name)?));
            }
            if let Some(name) = line.strip_prefix("product/fonts/") {
                return Ok(("product/fonts", basename(name)?));
            }
            Err(invalid("unsafe state entry"))
        }
    }
}

fn state_entries(directory: &OwnedFd, name: &OsStr) -> io::Result<Vec<String>> {
    let file = match open_regular(directory, name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let text = read_limited(file, MAX_LIST)?;
    let mut entries = Vec::new();
    for line in text.lines() {
        state_path(line)?;
        entries.push(line.to_owned());
    }
    Ok(entries)
}

fn child(directory: &OwnedFd, name: &str, create: bool) -> io::Result<OwnedFd> {
    match fs::openat(directory, name, DIRECTORY, Mode::empty()) {
        Ok(child) => Ok(child),
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            fs::mkdirat(directory, name, Mode::from_raw_mode(0o755))?;
            Ok(fs::openat(directory, name, DIRECTORY, Mode::empty())?)
        }
        Err(error) => Err(error.into()),
    }
}

fn destination(system: &OwnedFd, parent: &str, create: bool) -> io::Result<OwnedFd> {
    let mut directory = rustix::io::dup(system)?;
    for name in parent.split('/') {
        directory = child(&directory, name, create)?;
    }
    Ok(directory)
}

fn remove_entry(system: &OwnedFd, entry: &str) -> io::Result<()> {
    let (parent, name) = state_path(entry)?;
    let directory = match destination(system, parent, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match fs::unlinkat(&directory, name, AtFlags::empty()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn children(directory: &OwnedFd) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in fs::Dir::read_from(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            names.push(OsStr::from_bytes(name).to_owned());
        }
    }
    names.sort();
    Ok(names)
}

fn find_fonts(
    directory: &OwnedFd,
    wanted: &BTreeSet<String>,
    found: &mut BTreeMap<String, OwnedFd>,
) -> io::Result<()> {
    for name in children(directory)? {
        if let Ok(subdir) = fs::openat(directory, &name, DIRECTORY, Mode::empty()) {
            find_fonts(&subdir, wanted, found)?;
            continue;
        }
        if let Some(font) = name
            .to_str()
            .filter(|name| wanted.contains(*name) && !found.contains_key(*name))
        {
            match open_regular(directory, OsStr::new(font)) {
                Ok(file) => {
                    found.insert(font.to_owned(), file);
                }
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(40) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn scan_fonts(
    root: &Path,
    wanted: &BTreeSet<String>,
    found: &mut BTreeMap<String, OwnedFd>,
) -> io::Result<()> {
    match walk_absolute(root) {
        Ok(directory) => find_fonts(&directory, wanted, found),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn copy_file(source: &OwnedFd, system: &OwnedFd, entry: &str) -> io::Result<()> {
    let (parent, name) = state_path(entry)?;
    let directory = destination(system, parent, true)?;
    remove_entry(system, entry)?;
    let output = fs::openat(&directory, name, NEW_FILE, Mode::from_raw_mode(0o644))?;
    fs::fchmod(&output, Mode::from_raw_mode(0o644))?;
    io::copy(
        &mut File::from(rustix::io::dup(source)?),
        &mut File::from(output),
    )?;
    Ok(())
}

fn apply(paths: &Paths, whiteouts: &mut impl Whiteouts) -> io::Result<Outcome> {
    let system = walk_absolute(&paths.overlay_system)?;
    let parent = paths
        .state
        .parent()
        .ok_or_else(|| invalid("state has no parent"))?;
    let state_dir = walk_absolute(parent)?;
    fs::flock(&state_dir, FlockOperation::LockExclusive)?;
    let state_name = paths
        .state
        .file_name()
        .ok_or_else(|| invalid("state has no name"))?;
    component(state_name)?;
    let old = state_entries(&state_dir, state_name)?;

    let share = match walk_absolute(&paths.share) {
        Ok(share) => Some(share),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let manifest = match share.as_ref() {
        Some(share) => match open_regular(share, OsStr::new("fonts.xml")) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        },
        None => None,
    };
    let Some(manifest) = manifest else {
        for entry in &old {
            remove_entry(&system, entry)?;
        }
        remove_state(&state_dir, state_name)?;
        return Ok(Outcome::MissingManifest);
    };
    let share = share.ok_or_else(|| invalid("missing share directory"))?;

    let required = list(&share, "host-fonts.txt", false)?;
    let wanted: BTreeSet<String> = required.iter().cloned().collect();
    let mut found = BTreeMap::new();
    scan_fonts(&paths.host_fonts, &wanted, &mut found)?;
    let missing: Vec<_> = required
        .iter()
        .filter(|name| !found.contains_key(*name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        for entry in &old {
            remove_entry(&system, entry)?;
        }
        remove_state(&state_dir, state_name)?;
        return Ok(Outcome::MissingRequiredFonts(missing));
    }
    let optional = list(&share, "android-fonts.txt", false)?;
    let hidden = list(&share, "hidden-fonts.txt", true)?;
    let customization = open_regular(&share, OsStr::new("fonts_customization.xml"))?;
    if fs::fstat(&manifest)?.st_size > MAX_XML as i64
        || fs::fstat(&customization)?.st_size > MAX_XML as i64
    {
        return Err(invalid("XML exceeds size limit"));
    }
    let optional_wanted: BTreeSet<String> = optional.iter().cloned().collect();
    scan_fonts(&paths.host_fonts, &optional_wanted, &mut found)?;

    let mut entries = Vec::new();
    let mut unique = BTreeSet::new();
    for font in required
        .iter()
        .chain(optional.iter().filter(|name| found.contains_key(*name)))
    {
        let entry = format!("fonts/{font}");
        if unique.insert(entry.clone()) {
            entries.push(entry);
        }
    }
    for font in &hidden {
        let entry = if let Some(name) = font.strip_prefix("product/") {
            format!("product/fonts/{name}")
        } else {
            format!("fonts/{font}")
        };
        if !unique.insert(entry.clone()) {
            return Err(invalid("font is both copied and hidden"));
        }
        entries.push(entry);
    }
    entries.push("etc/fonts.xml".to_owned());
    entries.push("product/etc/fonts_customization.xml".to_owned());

    let (temp_name, temp) = loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = OsString::from(format!(
            ".deckard-fonts.{}.{}.new",
            std::process::id(),
            sequence
        ));
        match fs::openat(&state_dir, &name, NEW_FILE, Mode::from_raw_mode(0o600)) {
            Ok(file) => break (name, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let result = (|| -> io::Result<()> {
        for entry in &old {
            remove_entry(&system, entry)?;
        }
        for entry in &entries {
            if let Some(name) = entry.strip_prefix("fonts/")
                && let Some(source) = found.get(name)
            {
                copy_file(source, &system, entry)?;
                continue;
            }
            match entry.as_str() {
                "etc/fonts.xml" => copy_file(&manifest, &system, entry)?,
                "product/etc/fonts_customization.xml" => copy_file(&customization, &system, entry)?,
                _ => {
                    let (parent, name) = state_path(entry)?;
                    let directory = destination(&system, parent, true)?;
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
        Ok(())
    })();
    if let Err(error) = result {
        for entry in &entries {
            let _ = remove_entry(&system, entry);
        }
        let _ = fs::unlinkat(&state_dir, &temp_name, AtFlags::empty());
        return Err(error);
    }
    if let Err(error) = fs::renameat(&state_dir, &temp_name, &state_dir, state_name) {
        for entry in &entries {
            let _ = remove_entry(&system, entry);
        }
        let _ = fs::unlinkat(&state_dir, &temp_name, AtFlags::empty());
        return Err(error.into());
    }
    state_dir_sync(&state_dir)?;
    Ok(Outcome::Applied)
}

fn state_dir_sync(directory: &OwnedFd) -> io::Result<()> {
    File::from(rustix::io::dup(directory)?).sync_all()
}

fn remove_state(directory: &OwnedFd, name: &OsStr) -> io::Result<()> {
    match fs::unlinkat(directory, name, AtFlags::empty()) {
        Ok(()) => state_dir_sync(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn sync(paths: &Paths, whiteouts: &mut impl Whiteouts) -> io::Result<Outcome> {
    apply(paths, whiteouts)
}
