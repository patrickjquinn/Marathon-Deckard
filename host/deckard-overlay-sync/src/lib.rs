//! Reconcile Deckard's packaged Android overlay with Waydroid's persistent state.

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs::{self, File, Metadata, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use deckard_host_tools::{cfg, files};
use rustix::fs::{AtFlags, FileType, Mode, Timespec, Timestamps};
use rustix::process::{Gid, Uid};

const SYSTEM: &str = "1790477549";
/// Overlay files built for one image: (`waydroid.cfg` key, its value, file).
const GUARDS: &[(&str, &str, &str)] = &[
    (
        "vendor_datetime",
        "1790486210",
        "vendor/lib64/hw/hwcomposer.waydroid.so",
    ),
    (
        "system_datetime",
        SYSTEM,
        "system/apex/com.android.permission/priv-app/PermissionController@TQ3A.230901.001/PermissionController.apk",
    ),
    ("system_datetime", SYSTEM, "system/etc/init/hw/init.rc"),
    ("system_datetime", SYSTEM, "system/etc/init/atrace.rc"),
    ("system_datetime", SYSTEM, "system/etc/init/audioserver.rc"),
    ("system_datetime", SYSTEM, "system/etc/init/bpfloader.rc"),
    ("system_datetime", SYSTEM, "system/etc/init/dmesgd.rc"),
    ("system_datetime", SYSTEM, "system/etc/init/llkd.rc"),
    ("system_datetime", SYSTEM, "system/etc/init/perfetto.rc"),
];

#[derive(Debug, Clone)]
pub struct Paths {
    pub share: PathBuf,
    pub waydroid: PathBuf,
}

impl Paths {
    #[must_use]
    pub fn from_env() -> Self {
        let path = |name: &str, default: &str| {
            env::var_os(name).map_or_else(|| PathBuf::from(default), PathBuf::from)
        };
        Self {
            share: path("DECKARD_SHARE_DIR", "/usr/share/deckard"),
            waydroid: path("DECKARD_WAYDROID_DIR", "/var/lib/waydroid"),
        }
    }
}

/// Overlayfs whiteouts, so tests can run unprivileged.
pub trait Whiteouts {
    /// # Errors
    /// The path cannot be inspected.
    fn is_whiteout(&self, path: &Path) -> io::Result<bool>;
    /// # Errors
    /// The whiteout cannot be created.
    fn create(&self, path: &Path) -> io::Result<()>;
}

pub struct DeviceWhiteouts;

impl Whiteouts for DeviceWhiteouts {
    fn is_whiteout(&self, path: &Path) -> io::Result<bool> {
        let whiteout = |meta: Metadata| {
            meta.file_type().is_char_device() && meta.rdev() == rustix::fs::makedev(0, 0)
        };
        Ok(present(fs::symlink_metadata(path))?.is_some_and(whiteout))
    }

    fn create(&self, path: &Path) -> io::Result<()> {
        let (parent, name) = split(path)?;
        let device = rustix::fs::makedev(0, 0);
        Ok(rustix::fs::mknodat(
            &File::open(parent)?,
            name,
            FileType::CharacterDevice,
            Mode::empty(),
            device,
        )?)
    }
}

/// Syncs the device's overlay, logging to stderr.
///
/// # Errors
/// As for [`sync_with`].
pub fn sync(paths: &Paths) -> io::Result<()> {
    sync_with(paths, &DeviceWhiteouts, &mut io::stderr())
}

/// Copies the packaged overlay over Waydroid's, drops what the previous
/// package installed and this one lacks, leaves out files built for another
/// image, and reconciles whiteouts and properties.
///
/// # Errors
/// A path is unsafe (a symlinked directory or a list escaping the overlay)
/// or a file operation fails.
pub fn sync_with(
    paths: &Paths,
    whiteouts: &impl Whiteouts,
    log: &mut impl Write,
) -> io::Result<()> {
    let src = paths.share.join("overlay");
    if !src.is_dir() {
        return Ok(());
    }
    let dst = paths.waydroid.join("overlay");
    fs::create_dir_all(&dst)?;
    if fs::symlink_metadata(&dst)?.is_symlink() {
        return Err(invalid("overlay root is a symlink"));
    }
    let list = paths.waydroid.join("deckard-overlay.list");
    for old in files::read_optional(&list)?.lines() {
        let rel = relative(old)?;
        if fs::symlink_metadata(src.join(&rel)).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        {
            remove_file(&safe_target(&dst, &rel)?)?;
        }
    }
    let mut copied = Vec::new();
    copy_tree(&src, &dst, Path::new(""), &mut copied, &mut HashMap::new())?;
    write_lines(&list, &copied)?;
    let config = files::read_optional(&paths.waydroid.join("waydroid.cfg"))?;
    for &(key, expected, path) in GUARDS {
        let have = cfg::get(&config, key).unwrap_or_default();
        if have != expected {
            remove_file(&safe_target(&dst, Path::new(path))?)?;
            writeln!(
                log,
                "deckard-overlay-sync: {path} is for {key} {expected}, not '{have}'; left out"
            )?;
        }
    }
    reconcile_whiteouts(paths, &dst, whiteouts, log)?;
    reconcile_properties(paths)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

fn present<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        result => result.map(Some),
    }
}

fn split(path: &Path) -> io::Result<(&Path, &std::ffi::OsStr)> {
    let parent = path.parent().ok_or_else(|| invalid("missing parent"))?;
    Ok((
        parent,
        path.file_name()
            .ok_or_else(|| invalid("missing file name"))?,
    ))
}

/// A list entry as a plain relative path.
fn relative(value: &str) -> io::Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            _ => return Err(invalid("list contains an unsafe path")),
        }
    }
    if result.as_os_str().is_empty() {
        return Err(invalid("list contains an empty path"));
    }
    Ok(result)
}

/// `dst/relative`, refusing a symlinked directory on the way.
fn safe_target(dst: &Path, relative: &Path) -> io::Result<PathBuf> {
    let mut parent = dst.to_path_buf();
    let parts = relative.parent().map(Path::components);
    for part in parts.into_iter().flatten() {
        parent.push(part);
        if present(fs::symlink_metadata(&parent))?.is_some_and(|meta| meta.is_symlink()) {
            return Err(invalid("overlay path traverses a symlink"));
        }
    }
    Ok(dst.join(relative))
}

fn remove_file(path: &Path) -> io::Result<()> {
    present(fs::remove_file(path)).map(drop)
}

/// Replaces `path` with `lines`, mode 0600.
fn write_lines(path: &Path, lines: &[String]) -> io::Result<()> {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    files::replace(path, text.as_bytes(), 0o600)
}

fn copy_tree(
    src: &Path,
    dst: &Path,
    rel: &Path,
    files: &mut Vec<String>,
    hardlinks: &mut HashMap<(u64, u64), PathBuf>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(src.join(rel))?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let child = rel.join(entry.file_name());
        let source = src.join(&child);
        let target = safe_target(dst, &child)?;
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.is_dir() {
            if present(fs::symlink_metadata(&target))?.is_some_and(|meta| !meta.is_dir()) {
                remove_file(&target)?;
            }
            fs::create_dir_all(&target)?;
            copy_tree(src, dst, &child, files, hardlinks)?;
            preserve_metadata(&target, &metadata)?;
        } else if metadata.is_file() {
            remove_file(&target)?;
            match hardlinks.get(&(metadata.dev(), metadata.ino())) {
                Some(first) => fs::hard_link(first, &target)?,
                None => {
                    fs::copy(&source, &target)?;
                    preserve_metadata(&target, &metadata)?;
                    if metadata.nlink() > 1 {
                        hardlinks.insert((metadata.dev(), metadata.ino()), target);
                    }
                }
            }
            let name = child
                .to_str()
                .ok_or_else(|| invalid("overlay path is not UTF-8"))?;
            files.push(format!("./{name}"));
        } else if metadata.is_symlink() {
            remove_file(&target)?;
            std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
            preserve_metadata(&target, &metadata)?;
        } else {
            return Err(invalid("unsupported overlay file type"));
        }
    }
    Ok(())
}

/// Copies `metadata`'s owner (as root), mode (not for links) and times onto `path`.
fn preserve_metadata(path: &Path, metadata: &Metadata) -> io::Result<()> {
    let (parent, name) = split(path)?;
    let parent = File::open(parent)?;
    let flags = if metadata.is_symlink() {
        AtFlags::SYMLINK_NOFOLLOW
    } else {
        AtFlags::empty()
    };
    if rustix::process::geteuid().is_root() {
        let (uid, gid) = (Uid::from_raw(metadata.uid()), Gid::from_raw(metadata.gid()));
        rustix::fs::chownat(&parent, name, Some(uid), Some(gid), flags)?;
    }
    if !metadata.is_symlink() {
        fs::set_permissions(path, Permissions::from_mode(metadata.mode()))?;
    }
    let time = |tv_sec, tv_nsec| Timespec { tv_sec, tv_nsec };
    let times = Timestamps {
        last_access: time(metadata.atime(), metadata.atime_nsec()),
        last_modification: time(metadata.mtime(), metadata.mtime_nsec()),
    };
    Ok(rustix::fs::utimensat(&parent, name, &times, flags)?)
}

/// The non-comment lines of a share list.
fn wanted(path: &Path) -> io::Result<Vec<String>> {
    let text = files::read_optional(path)?;
    let lines = text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'));
    Ok(lines.map(str::to_owned).collect())
}

fn reconcile_whiteouts(
    paths: &Paths,
    dst: &Path,
    whiteouts: &impl Whiteouts,
    log: &mut impl Write,
) -> io::Result<()> {
    let wanted = wanted(&paths.share.join("whiteouts"))?;
    let list = paths.waydroid.join("deckard-whiteouts.list");
    for line in files::read_optional(&list)?.lines() {
        let path = safe_target(dst, &relative(line)?)?;
        if !wanted.iter().any(|wanted| wanted == line) && whiteouts.is_whiteout(&path)? {
            remove_file(&path)?;
        }
    }
    let mut installed = Vec::new();
    for line in wanted {
        let Ok(rel) = relative(&line) else { continue };
        let path = safe_target(dst, &rel)?;
        if !whiteouts.is_whiteout(&path)? {
            if present(fs::symlink_metadata(&path))?.is_some() {
                writeln!(
                    log,
                    "deckard-overlay-sync: {line} exists in the overlay; not hiding it"
                )?;
                continue;
            }
            fs::create_dir_all(split(&path)?.0)?;
            whiteouts.create(&path)?;
        }
        installed.push(line);
    }
    write_lines(&list, &installed)
}

/// Rewrites the shipped properties into `waydroid.cfg`'s `[properties]`
/// and `waydroid_base.prop`, dropping the ones a previous package shipped.
fn reconcile_properties(paths: &Paths) -> io::Result<()> {
    let new = wanted(&paths.share.join("waydroid.prop"))?;
    let list = paths.waydroid.join("deckard-props.list");
    let mut keys: BTreeSet<String> = files::read_optional(&list)?
        .lines()
        .map(str::to_owned)
        .collect();
    let pairs: Vec<(&str, &str)> = new.iter().filter_map(|line| line.split_once('=')).collect();
    keys.extend(pairs.iter().map(|(key, _)| (*key).to_owned()));
    let shipped = |line: &&str| {
        let named = |key: &String| {
            !key.is_empty()
                && line
                    .strip_prefix(key.as_str())
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
        };
        keys.iter().any(named)
    };
    let cfg = paths.waydroid.join("waydroid.cfg");
    for path in [&cfg, &paths.waydroid.join("waydroid_base.prop")] {
        if !path.is_file() {
            continue;
        }
        let content = fs::read_to_string(path)?;
        let mut lines: Vec<String> = content
            .lines()
            .filter(|line| !shipped(line))
            .map(str::to_owned)
            .collect();
        for (key, value) in &pairs {
            if path != &cfg {
                lines.push(format!("{key}={value}"));
                continue;
            }
            let mut index = 0;
            while index < lines.len() {
                if lines[index] == "[properties]" {
                    lines.insert(index + 1, format!("{key} = {value}"));
                    index += 1;
                }
                index += 1;
            }
        }
        let mut result = lines.join("\n");
        if !result.is_empty() {
            result.push('\n');
        }
        write_preserving(path, &result)?;
    }
    let names: Vec<String> = pairs.iter().map(|(key, _)| (*key).to_owned()).collect();
    write_lines(&list, &names)
}

/// Replaces `path` atomically, keeping its owner, mode and times.
fn write_preserving(path: &Path, content: &str) -> io::Result<()> {
    let mut output = tempfile::NamedTempFile::new_in(split(path)?.0)?;
    output.write_all(content.as_bytes())?;
    preserve_metadata(output.path(), &fs::metadata(path)?)?;
    output.as_file().sync_all()?;
    output.persist(path).map_err(|error| error.error)?;
    Ok(())
}
