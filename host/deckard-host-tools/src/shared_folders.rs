use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, RenameFlags};
use rustix::process::{Gid, Uid};

pub const MEDIA_RW: u32 = 1023;
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FOLDERS: [(&str, &str, &str); 6] = [
    ("Documents", "XDG_DOCUMENTS_DIR", "Documents"),
    ("Download", "XDG_DOWNLOAD_DIR", "Downloads"),
    ("Pictures", "XDG_PICTURES_DIR", "Pictures"),
    ("Music", "XDG_MUSIC_DIR", "Music"),
    ("Movies", "XDG_VIDEOS_DIR", "Videos"),
    ("DCIM", "", "DCIM"),
];

pub trait Operations {
    fn chown(&mut self, descriptor: &OwnedFd, uid: u32, gid: u32) -> io::Result<()>;
    fn bind(&mut self, source: &OwnedFd, target: &OwnedFd) -> io::Result<()>;
}

pub struct Linux;

impl Operations for Linux {
    fn chown(&mut self, descriptor: &OwnedFd, uid: u32, gid: u32) -> io::Result<()> {
        if uid == u32::MAX || gid == u32::MAX {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid owner"));
        }
        Ok(fs::fchown(
            descriptor,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )?)
    }

    fn bind(&mut self, source: &OwnedFd, target: &OwnedFd) -> io::Result<()> {
        Ok(rustix::mount::mount_bind(
            format!("/proc/self/fd/{}", source.as_raw_fd()),
            format!("/proc/self/fd/{}", target.as_raw_fd()),
        )?)
    }
}

#[derive(Default, Debug)]
pub struct Report {
    pub shared: Vec<String>,
    pub warnings: Vec<String>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
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

fn open_directory(parent: impl AsFd, name: &OsStr) -> io::Result<OwnedFd> {
    component(name)?;
    Ok(fs::openat(parent, name, DIRECTORY, Mode::empty())?)
}

fn walk(parent: impl AsFd, path: &Path) -> io::Result<OwnedFd> {
    let mut current = rustix::io::dup(parent)?;
    for part in path.components() {
        match part {
            Component::Normal(name) => current = open_directory(&current, name)?,
            _ => return Err(invalid("path must contain only normal components")),
        }
    }
    Ok(current)
}

pub fn open_absolute(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(invalid("expected an absolute path"));
    }
    let root = fs::open("/", DIRECTORY, Mode::empty())?;
    walk(
        &root,
        path.strip_prefix("/")
            .map_err(|_| invalid("invalid absolute path"))?,
    )
}

fn create_directory(
    parent: &OwnedFd,
    name: &str,
    uid: u32,
    gid: u32,
    mode: u32,
    operations: &mut impl Operations,
) -> io::Result<OwnedFd> {
    match open_directory(parent, OsStr::new(name)) {
        Ok(directory) => return Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::mkdirat(parent, name, Mode::from_raw_mode(mode))?;
    let directory = open_directory(parent, OsStr::new(name))?;
    operations.chown(&directory, uid, gid)?;
    fs::fchmod(&directory, Mode::from_raw_mode(mode))?;
    Ok(directory)
}

fn read_small(descriptor: OwnedFd, limit: u64) -> io::Result<String> {
    let mut text = String::new();
    std::fs::File::from(descriptor)
        .take(limit)
        .read_to_string(&mut text)?;
    Ok(text)
}

pub fn session_data(configuration: &str) -> Option<&str> {
    configuration
        .lines()
        .filter(|line| line.starts_with("lxc.mount.entry"))
        .find_map(|line| {
            let (_, value) = line.split_once('=')?;
            let fields: Vec<_> = value.split_whitespace().collect();
            (fields.len() >= 4 && fields[1] == "data").then_some(fields[0])
        })
}

pub fn user_directories(configuration: &str) -> BTreeMap<String, String> {
    configuration
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            if !FOLDERS
                .iter()
                .any(|(_, candidate, _)| key == *candidate && !key.is_empty())
            {
                return None;
            }
            let name = value
                .trim_end()
                .strip_prefix("\"$HOME/")?
                .strip_suffix('"')?;
            component(OsStr::new(name)).ok()?;
            Some((key.to_owned(), name.to_owned()))
        })
        .collect()
}

fn directories_from_home(home: &OwnedFd) -> BTreeMap<String, String> {
    let read = || -> io::Result<String> {
        let config = open_directory(home, OsStr::new(".config"))?;
        let file = fs::openat(
            config,
            "user-dirs.dirs",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
        if FileType::from_raw_mode(fs::fstat(&file)?.st_mode) != FileType::RegularFile {
            return Err(invalid(
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
        open_absolute(home).map_or_else(|_| BTreeMap::new(), |fd| directories_from_home(&fd));
    let mut folders = Vec::with_capacity(FOLDERS.len());
    for (android, key, default) in FOLDERS {
        let name = names.get(key).map_or(default, String::as_str);
        folders.push((android, home.join(name)));
    }
    folders
}

fn children(directory: &OwnedFd) -> io::Result<Vec<CString>> {
    let mut names = Vec::new();
    for entry in fs::Dir::read_from(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

fn fix_tree(
    parent: &OwnedFd,
    name: &CString,
    uid: u32,
    depth: usize,
    operations: &mut impl Operations,
) -> io::Result<()> {
    if depth > 128 {
        return Err(invalid("shared folder nesting exceeds 128 levels"));
    }
    let metadata = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    let file_type = FileType::from_raw_mode(metadata.st_mode);
    if !matches!(file_type, FileType::Directory | FileType::RegularFile) {
        return Ok(());
    }
    let descriptor = fs::openat(
        parent,
        name,
        OFlags::RDONLY
            | OFlags::NOFOLLOW
            | OFlags::CLOEXEC
            | OFlags::NONBLOCK
            | if file_type == FileType::Directory {
                OFlags::DIRECTORY
            } else {
                OFlags::empty()
            },
        Mode::empty(),
    )?;
    let pinned = fs::fstat(&descriptor)?;
    if metadata.st_ino != pinned.st_ino || metadata.st_dev != pinned.st_dev {
        return Err(invalid("shared file changed while opening it"));
    }
    operations.chown(&descriptor, uid, MEDIA_RW)?;
    fs::fchmod(
        &descriptor,
        Mode::from_raw_mode(
            pinned.st_mode
                | if file_type == FileType::Directory {
                    0o770
                } else {
                    0o660
                },
        ),
    )?;
    if file_type == FileType::Directory {
        for child in children(&descriptor)? {
            fix_tree(&descriptor, &child, uid, depth + 1, operations)?;
        }
    }
    Ok(())
}

fn migrate(
    android: &OwnedFd,
    host: &OwnedFd,
    uid: u32,
    operations: &mut impl Operations,
    warnings: &mut Vec<String>,
) -> io::Result<()> {
    for name in children(android)? {
        match fs::renameat_with(android, &name, host, &name, RenameFlags::NOREPLACE) {
            Ok(()) => {
                if let Err(error) = fix_tree(host, &name, uid, 0, operations) {
                    warnings.push(format!(
                        "could not set migrated file ownership for {name:?}: {error}"
                    ));
                }
            }
            Err(error) => warnings.push(format!("kept {name:?} in Android: {error}")),
        }
    }
    Ok(())
}

pub fn share(
    rootfs: &Path,
    home: &Path,
    data: &Path,
    uid: u32,
    gid: u32,
    operations: &mut impl Operations,
) -> io::Result<Report> {
    let home = open_absolute(home)?;
    let home_stat = fs::fstat(&home)?;
    if home_stat.st_uid != uid {
        return Err(invalid("home directory has a different owner"));
    }
    let data = open_absolute(data)?;
    let data_stat = fs::fstat(&data)?;
    if data_stat.st_uid != uid {
        return Err(invalid("Android data has a different owner"));
    }
    if (data_stat.st_dev, data_stat.st_ino) == (home_stat.st_dev, home_stat.st_ino) {
        return Err(invalid("Android data cannot be the user's home"));
    }
    let root = open_absolute(rootfs)?;
    let media = create_directory(&data, "media", MEDIA_RW, MEDIA_RW, 0o770, operations)?;
    let user0 = create_directory(&media, "0", MEDIA_RW, MEDIA_RW, 0o770, operations)?;
    let mounted = walk(&root, Path::new("data/media/0"))?;
    let names = directories_from_home(&home);
    let mut report = Report::default();
    for (android_name, key, default) in FOLDERS {
        let name = names.get(key).map_or(default, String::as_str);
        let mut map_folder = || -> io::Result<()> {
            let host = create_directory(&home, name, uid, gid, 0o755, operations)?;
            if fs::fstat(&host)?.st_uid != uid {
                return Err(invalid("shared folder has a different owner"));
            }
            let android =
                create_directory(&user0, android_name, MEDIA_RW, MEDIA_RW, 0o770, operations)?;
            let target = open_directory(&mounted, OsStr::new(android_name))?;
            let original = fs::fstat(&android)?;
            let container = fs::fstat(&target)?;
            if original.st_ino != container.st_ino || original.st_dev != container.st_dev {
                return Err(invalid("container data does not match the user's data"));
            }
            migrate(&android, &host, uid, operations, &mut report.warnings)?;
            operations.bind(&host, &target)?;
            Ok(())
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

pub fn run_hook() -> Result<Report, Box<dyn std::error::Error>> {
    let Some(rootfs) = std::env::var_os("LXC_ROOTFS_MOUNT") else {
        return Ok(Report::default());
    };
    let Some(config) = std::env::var_os("LXC_CONFIG_FILE") else {
        return Ok(Report::default());
    };
    if !rustix::process::geteuid().is_root() {
        return Err("LXC shared-folder hook requires root".into());
    }
    let directory = Path::new(&config)
        .parent()
        .ok_or("invalid LXC configuration path")?;
    let configuration = read_small(
        fs::openat(
            open_absolute(directory)?,
            "config_session",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?,
        65536,
    )?;
    let data = Path::new(session_data(&configuration).ok_or("no Android session data bind")?);
    let owner = fs::fstat(open_absolute(data)?)?.st_uid;
    if owner == 0 {
        return Ok(Report::default());
    }
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(owner))?
        .ok_or("unknown session user")?;
    Ok(share(
        Path::new(&rootfs),
        &user.dir,
        data,
        owner,
        user.gid.as_raw(),
        &mut Linux,
    )?)
}
