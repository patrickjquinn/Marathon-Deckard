//! Reconcile Deckard's packaged Android overlay with Waydroid's persistent state.

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{
    AtFlags, FileType, Mode, Timespec, Timestamps, chownat, makedev, mknodat, utimensat,
};
use rustix::process::geteuid;
use tempfile::NamedTempFile;

const GUARDS: &[(&str, &str, &str)] = &[
    (
        "vendor_datetime",
        "1790486210",
        "vendor/lib64/hw/hwcomposer.waydroid.so",
    ),
    (
        "system_datetime",
        "1790477549",
        "system/apex/com.android.permission/priv-app/PermissionController@TQ3A.230901.001/PermissionController.apk",
    ),
    (
        "system_datetime",
        "1790477549",
        "system/etc/init/hw/init.rc",
    ),
    ("system_datetime", "1790477549", "system/etc/init/atrace.rc"),
    (
        "system_datetime",
        "1790477549",
        "system/etc/init/audioserver.rc",
    ),
    (
        "system_datetime",
        "1790477549",
        "system/etc/init/bpfloader.rc",
    ),
    ("system_datetime", "1790477549", "system/etc/init/dmesgd.rc"),
    ("system_datetime", "1790477549", "system/etc/init/llkd.rc"),
    (
        "system_datetime",
        "1790477549",
        "system/etc/init/perfetto.rc",
    ),
];

#[derive(Debug, Clone)]
pub struct Paths {
    pub share: PathBuf,
    pub waydroid: PathBuf,
}

impl Paths {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            share: env::var_os("DECKARD_SHARE_DIR")
                .map_or_else(|| PathBuf::from("/usr/share/deckard"), PathBuf::from),
            waydroid: env::var_os("DECKARD_WAYDROID_DIR")
                .map_or_else(|| PathBuf::from("/var/lib/waydroid"), PathBuf::from),
        }
    }
}

pub trait Whiteouts {
    fn is_whiteout(&self, path: &Path) -> io::Result<bool>;
    fn create(&self, path: &Path) -> io::Result<()>;
}

pub struct DeviceWhiteouts;

impl Whiteouts for DeviceWhiteouts {
    fn is_whiteout(&self, path: &Path) -> io::Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(meta) => Ok(meta.file_type().is_char_device() && meta.rdev() == makedev(0, 0)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn create(&self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("whiteout has no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| invalid("whiteout has no name"))?;
        let dir = File::open(parent)?;
        mknodat(
            &dir,
            name,
            FileType::CharacterDevice,
            Mode::empty(),
            makedev(0, 0),
        )
        .map_err(io::Error::from)
    }
}

pub fn sync(paths: &Paths) -> io::Result<()> {
    sync_with(paths, &DeviceWhiteouts, &mut io::stderr())
}

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
    if fs::symlink_metadata(&dst)?.file_type().is_symlink() {
        return Err(invalid("overlay root is a symlink"));
    }

    let old_files = read_lines(&paths.waydroid.join("deckard-overlay.list"))?;
    for old in old_files {
        let rel = relative(&old)?;
        if fs::symlink_metadata(src.join(&rel)).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        {
            remove_file_if_present(&safe_target(&dst, &rel)?)?;
        }
    }
    let mut files = Vec::new();
    let mut hardlinks = HashMap::new();
    copy_tree(&src, &dst, Path::new(""), &mut files, &mut hardlinks)?;
    write_lines_atomic(&paths.waydroid.join("deckard-overlay.list"), &files)?;

    let cfg = paths.waydroid.join("waydroid.cfg");
    let config = read_optional(&cfg)?;
    for &(key, expected, path) in GUARDS {
        let have = config_value(&config, key);
        if have != expected {
            remove_file_if_present(&safe_target(&dst, Path::new(path))?)?;
            writeln!(
                log,
                "deckard-overlay-sync: {path} is for {key} {expected}, not '{have}'; left out"
            )?;
        }
    }

    reconcile_whiteouts(paths, &dst, whiteouts, log)?;
    reconcile_properties(paths)?;
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

fn relative(value: &str) -> io::Result<PathBuf> {
    let path = Path::new(value);
    let mut result = PathBuf::new();
    for component in path.components() {
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

fn safe_target(dst: &Path, relative: &Path) -> io::Result<PathBuf> {
    let target = dst.join(relative);
    let mut parent = dst.to_path_buf();
    if let Some(components) = relative.parent() {
        for part in components.components() {
            parent.push(part.as_os_str());
            match fs::symlink_metadata(&parent) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(invalid("overlay path traverses a symlink"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(target)
}

fn read_optional(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

fn read_lines(path: &Path) -> io::Result<Vec<String>> {
    Ok(read_optional(path)?.lines().map(str::to_owned).collect())
}

fn write_lines_atomic(path: &Path, lines: &[String]) -> io::Result<()> {
    let mut output =
        NamedTempFile::new_in(path.parent().ok_or_else(|| invalid("missing parent"))?)?;
    for line in lines {
        writeln!(output, "{line}")?;
    }
    output.as_file().sync_all()?;
    output.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let mut output =
        NamedTempFile::new_in(path.parent().ok_or_else(|| invalid("missing parent"))?)?;
    output.write_all(content.as_bytes())?;
    preserve_metadata(output.path(), &fs::metadata(path)?, false)?;
    output.as_file().sync_all()?;
    output.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn exists_no_follow(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn copy_tree(
    src: &Path,
    dst: &Path,
    rel: &Path,
    files: &mut Vec<String>,
    hardlinks: &mut HashMap<(u64, u64), PathBuf>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(src.join(rel))?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let child = rel.join(entry.file_name());
        let source = src.join(&child);
        let target = safe_target(dst, &child)?;
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.is_dir() {
            if exists_no_follow(&target)? && !fs::symlink_metadata(&target)?.is_dir() {
                remove_file_if_present(&target)?;
            }
            fs::create_dir_all(&target)?;
            copy_tree(src, dst, &child, files, hardlinks)?;
            preserve_metadata(&target, &metadata, false)?;
        } else if metadata.is_file() {
            remove_file_if_present(&target)?;
            if let Some(first) = hardlinks.get(&(metadata.dev(), metadata.ino())) {
                fs::hard_link(first, &target)?;
            } else {
                fs::copy(&source, &target)?;
                preserve_metadata(&target, &metadata, false)?;
                if metadata.nlink() > 1 {
                    hardlinks.insert((metadata.dev(), metadata.ino()), target);
                }
            }
            let name = child
                .to_str()
                .ok_or_else(|| invalid("overlay path is not UTF-8"))?;
            files.push(format!("./{name}"));
        } else if metadata.file_type().is_symlink() {
            remove_file_if_present(&target)?;
            symlink(fs::read_link(&source)?, &target)?;
            preserve_metadata(&target, &metadata, true)?;
        } else {
            return Err(invalid("unsupported overlay file type"));
        }
    }
    Ok(())
}

fn preserve_metadata(path: &Path, metadata: &Metadata, symlink: bool) -> io::Result<()> {
    if geteuid().as_raw() == 0 {
        let parent = File::open(path.parent().ok_or_else(|| invalid("missing parent"))?)?;
        chownat(
            &parent,
            path.file_name()
                .ok_or_else(|| invalid("missing file name"))?,
            Some(rustix::process::Uid::from_raw(metadata.uid())),
            Some(rustix::process::Gid::from_raw(metadata.gid())),
            if symlink {
                AtFlags::SYMLINK_NOFOLLOW
            } else {
                AtFlags::empty()
            },
        )
        .map_err(io::Error::from)?;
    }
    if !symlink {
        fs::set_permissions(path, fs::Permissions::from_mode(metadata.mode()))?;
    }
    let parent = File::open(path.parent().ok_or_else(|| invalid("missing parent"))?)?;
    let times = Timestamps {
        last_access: Timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        last_modification: Timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    };
    utimensat(
        &parent,
        path.file_name()
            .ok_or_else(|| invalid("missing file name"))?,
        &times,
        if symlink {
            AtFlags::SYMLINK_NOFOLLOW
        } else {
            AtFlags::empty()
        },
    )
    .map_err(io::Error::from)?;
    Ok(())
}

fn config_value(config: &str, key: &str) -> String {
    config
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim_end() == key).then(|| value.trim_start().to_owned())
        })
        .unwrap_or_default()
}

fn reconcile_whiteouts(
    paths: &Paths,
    dst: &Path,
    whiteouts: &impl Whiteouts,
    log: &mut impl Write,
) -> io::Result<()> {
    let wanted: Vec<String> = read_lines(&paths.share.join("whiteouts"))?
        .into_iter()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect();
    let old = read_lines(&paths.waydroid.join("deckard-whiteouts.list"))?;
    for line in old {
        let rel = relative(&line)?;
        let path = safe_target(dst, &rel)?;
        if !wanted.contains(&line) && whiteouts.is_whiteout(&path)? {
            remove_file_if_present(&path)?;
        }
    }
    let mut installed = Vec::new();
    for line in wanted {
        let Ok(rel) = relative(&line) else { continue };
        let path = safe_target(dst, &rel)?;
        if !whiteouts.is_whiteout(&path)? {
            if exists_no_follow(&path)? {
                writeln!(
                    log,
                    "deckard-overlay-sync: {line} exists in the overlay; not hiding it"
                )?;
                continue;
            }
            fs::create_dir_all(path.parent().ok_or_else(|| invalid("missing parent"))?)?;
            whiteouts.create(&path)?;
        }
        installed.push(line);
    }
    write_lines_atomic(&paths.waydroid.join("deckard-whiteouts.list"), &installed)
}

fn reconcile_properties(paths: &Paths) -> io::Result<()> {
    let new: Vec<String> = read_lines(&paths.share.join("waydroid.prop"))?
        .into_iter()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect();
    let list = paths.waydroid.join("deckard-props.list");
    let mut keys: BTreeSet<String> = read_lines(&list)?.into_iter().collect();
    for line in &new {
        if let Some((key, _)) = line.split_once('=') {
            keys.insert(key.to_owned());
        }
    }
    let cfg = paths.waydroid.join("waydroid.cfg");
    let base = paths.waydroid.join("waydroid_base.prop");
    for path in [&cfg, &base] {
        if !path.is_file() {
            continue;
        }
        let content = fs::read_to_string(path)?;
        let mut lines: Vec<String> = content
            .lines()
            .filter(|line| {
                !keys.iter().any(|key| {
                    !key.is_empty()
                        && line.starts_with(key)
                        && line[key.len()..].trim_start().starts_with('=')
                })
            })
            .map(str::to_owned)
            .collect();
        for prop in &new {
            if prop.is_empty() {
                continue;
            }
            if path == &cfg {
                if let Some((key, value)) = prop.split_once('=') {
                    let mut index = 0;
                    while index < lines.len() {
                        if lines[index] == "[properties]" {
                            lines.insert(index + 1, format!("{key} = {value}"));
                            index += 1;
                        }
                        index += 1;
                    }
                }
            } else if let Some((key, value)) = prop.split_once('=') {
                lines.push(format!("{key}={value}"));
            }
        }
        let mut result = lines.join("\n");
        if !result.is_empty() {
            result.push('\n');
        }
        write_atomic(path, &result)?;
    }
    let names: Vec<String> = new
        .iter()
        .filter_map(|line| line.split_once('=').map(|(key, _)| key.to_owned()))
        .collect();
    write_lines_atomic(&list, &names)
}
