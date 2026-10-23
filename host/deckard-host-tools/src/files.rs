//! File helpers the Deckard host crates share.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Permissions};
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};

use rustix::fs::{CWD, Mode, OFlags, ResolveFlags};

/// Opens a directory without following a final link.
pub const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Opens the directory `path` below `base` without following any link on
/// the way; `path` has only plain components.
///
/// # Errors
/// The path has `..` or `.`, a component is a link, or it is missing.
pub fn open_dir_at(base: impl AsFd, path: &Path) -> io::Result<OwnedFd> {
    let plain = path
        .components()
        .all(|part| matches!(part, Component::RootDir | Component::Normal(_)));
    if !plain {
        return Err(invalid("expected a plain path"));
    }
    let resolve = ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS;
    Ok(rustix::fs::openat2(
        base,
        path,
        DIRECTORY,
        Mode::empty(),
        resolve,
    )?)
}

/// [`open_dir_at`] for an absolute path.
///
/// # Errors
/// As for [`open_dir_at`], or the path is relative.
pub fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(invalid("expected an absolute path"));
    }
    open_dir_at(CWD, path)
}

/// The names in `directory` but `.` and `..`, sorted.
///
/// # Errors
/// The directory cannot be read.
pub fn children(directory: impl AsFd) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            names.push(OsStr::from_bytes(name).to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Whether `name` is one plain path component.
#[must_use]
pub fn is_component(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes != b"."
        && bytes != b".."
        && !bytes.contains(&b'/')
        && !bytes.contains(&0)
}

/// `result` with "not found" as `None`.
///
/// # Errors
/// Any other error of `result`.
pub fn found<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        result => result.map(Some),
    }
}

/// The file's text, or empty when it does not exist.
///
/// # Errors
/// The file exists but cannot be read as UTF-8.
pub fn read_optional(path: &Path) -> io::Result<String> {
    Ok(found(fs::read_to_string(path))?.unwrap_or_default())
}

/// Replaces `path` atomically with `content` in mode `mode`, then syncs its
/// directory.
///
/// # Errors
/// The temporary file cannot be written, renamed or synced.
pub fn replace(path: &Path, content: &[u8], mode: u32) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| invalid("path has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(content)?;
    temporary
        .as_file()
        .set_permissions(Permissions::from_mode(mode))?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    File::open(directory)?.sync_all()
}
