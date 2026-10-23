//! Session-owned binds for the LXC instance (upstream
//! `generate_session_lxc_config`): the user's Wayland socket, PulseAudio
//! socket and Android data directory.
//!
//! These paths belong to the session user, who could swap one for a symlink
//! between a check and LXC's bind and so hand Android (root in the container)
//! a host path. Each source is therefore opened once without following links,
//! checked on the descriptor, and bind-mounted from that descriptor onto a
//! root-owned stage under the daemon's runtime directory; `config_session`
//! names only the stage.

use crate::mounts::owner_controlled;
use crate::{Error, Result};
use rustix::fd::OwnedFd;
use rustix::fs::{CWD, FileType, Mode, OFlags, ResolveFlags, fstat, openat2};
use rustix::mount::{MoveMountFlags, OpenTreeFlags, UnmountFlags, move_mount, open_tree, unmount};
use rustix::process::geteuid;
use std::collections::HashMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the pinned session sources are mounted for LXC.
pub struct Stage {
    root: PathBuf,
}

impl Stage {
    /// The stage under the daemon's runtime directory.
    pub fn new(runtime: &Path) -> Self {
        Self {
            root: runtime.join("session"),
        }
    }

    fn wayland(&self) -> PathBuf {
        self.root.join("wayland-0")
    }

    fn pulse(&self) -> PathBuf {
        self.root.join("pulse-native")
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn targets(&self) -> [PathBuf; 3] {
        [self.wayland(), self.pulse(), self.data()]
    }

    /// Unmounts whatever a previous session left on the stage.
    ///
    /// # Errors
    /// A stage mount cannot be removed.
    pub fn release(&self) -> Result<()> {
        for target in self.targets() {
            // Bounded: a stage target holds at most a few stacked mounts.
            for _ in 0..8 {
                match unmount(&target, UnmountFlags::DETACH | UnmountFlags::NOFOLLOW) {
                    Ok(()) => continue,
                    Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOENT) => break,
                    Err(error) => return Err(std::io::Error::from(error).into()),
                }
            }
        }
        Ok(())
    }

    fn prepare(&self) -> Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)?;
        owner_controlled(&self.root)?;
        if fs::symlink_metadata(self.data()).is_err() {
            DirBuilder::new().mode(0o700).create(self.data())?;
        }
        for socket in [self.wayland(), self.pulse()] {
            if fs::symlink_metadata(&socket).is_err() {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&socket)?;
            }
        }
        for target in self.targets() {
            owner_controlled(&target)?;
        }
        Ok(())
    }
}

fn field<'a>(fields: &'a HashMap<String, String>, key: &str) -> Result<&'a str> {
    fields
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| Error::Invalid(format!("missing session field: {key}")))
}

fn safe_path(path: &Path) -> Result<()> {
    let value = path.to_string_lossy();
    if !path.is_absolute() || value.chars().any(char::is_whitespace) || value.contains('\0') {
        return Err(Error::Invalid(format!("unsafe LXC mount path: {value}")));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Socket,
    Directory,
}

/// `path` resolved once, then opened without following any link and checked
/// on the descriptor: owned by `uid` and of the expected kind.
fn pin(path: &Path, uid: u32, kind: Kind) -> Result<(PathBuf, OwnedFd)> {
    safe_path(path)?;
    let canonical = fs::canonicalize(path)?;
    safe_path(&canonical)?;
    let fd = open_without_links(&canonical)?;
    let stat = fstat(&fd).map_err(std::io::Error::from)?;
    let actual = FileType::from_raw_mode(stat.st_mode);
    let expected = match kind {
        Kind::Socket => FileType::Socket,
        Kind::Directory => FileType::Directory,
    };
    if stat.st_uid != uid || actual != expected {
        return Err(Error::Invalid(format!(
            "session path is not a {} owned by UID {uid}: {}",
            if kind == Kind::Socket {
                "socket"
            } else {
                "directory"
            },
            path.display()
        )));
    }
    Ok((canonical, fd))
}

fn open_without_links(path: &Path) -> Result<OwnedFd> {
    let fd = openat2(
        CWD,
        path,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .map_err(std::io::Error::from)?;
    Ok(fd)
}

struct Sources {
    wayland: OwnedFd,
    pulse: Option<OwnedFd>,
    data: OwnedFd,
}

fn sources(fields: &HashMap<String, String>, uid: u32, runtime_root: &Path) -> Result<Sources> {
    let runtime = runtime_root.join(uid.to_string());
    safe_path(&runtime)?;
    if Path::new(field(fields, "xdg_runtime_dir")?) != runtime {
        return Err(Error::Invalid("unexpected XDG runtime directory".into()));
    }
    if runtime.metadata()?.uid() != uid {
        return Err(Error::Invalid(
            "XDG runtime directory has wrong owner".into(),
        ));
    }
    let display = field(fields, "wayland_display")?;
    if display.is_empty() || display.contains('/') || display.chars().any(char::is_whitespace) {
        return Err(Error::Invalid("invalid Wayland display name".into()));
    }
    let (wayland_path, wayland) = pin(&runtime.join(display), uid, Kind::Socket)?;
    if !wayland_path.starts_with(&runtime) {
        return Err(Error::Invalid(
            "Wayland socket escapes runtime directory".into(),
        ));
    }
    let pulse_path = Path::new(field(fields, "pulse_runtime_path")?).join("native");
    let pulse = if pulse_path.exists() {
        let (canonical, fd) = pin(&pulse_path, uid, Kind::Socket)?;
        if !canonical.starts_with(&runtime) {
            return Err(Error::Invalid(
                "Pulse socket escapes runtime directory".into(),
            ));
        }
        Some(fd)
    } else {
        None
    };
    let (_, data) = pin(
        Path::new(field(fields, "waydroid_data")?),
        uid,
        Kind::Directory,
    )?;
    Ok(Sources {
        wayland,
        pulse,
        data,
    })
}

/// The `config_session` text for a stage.
pub fn render(stage: &Stage, pulse: bool) -> String {
    let mut config = format!(
        "lxc.mount.entry = tmpfs /run/xdg none create=dir 0 0\n\
         lxc.mount.entry = {} run/xdg/wayland-0 none rbind,create=file 0 0\n",
        stage.wayland().display()
    );
    if pulse {
        config.push_str(&format!(
            "lxc.mount.entry = {} run/xdg/pulse/native none rbind,create=file 0 0\n",
            stage.pulse().display()
        ));
    }
    config.push_str(&format!(
        "lxc.mount.entry = {} data none rbind 0 0\n",
        stage.data().display()
    ));
    config
}

/// Checks the session's sources, pins them and writes `config_session` that
/// names them only through the stage.
///
/// # Errors
/// A source is missing, foreign or of the wrong kind; or the stage or the
/// instance directory is untrusted; or a bind or write fails.
pub fn write(
    lxc_instance: &Path,
    fields: &HashMap<String, String>,
    uid: u32,
    runtime_root: &Path,
    stage: &Stage,
) -> Result<()> {
    let sources = sources(fields, uid, runtime_root)?;
    let config = render(stage, sources.pulse.is_some());
    let directory = fs::symlink_metadata(lxc_instance)?;
    if !directory.is_dir() || directory.uid() != geteuid().as_raw() || directory.mode() & 0o022 != 0
    {
        return Err(Error::Invalid("untrusted LXC instance directory".into()));
    }
    stage.release()?;
    stage.prepare()?;
    let binds = [
        (Some(&sources.wayland), stage.wayland(), false),
        (sources.pulse.as_ref(), stage.pulse(), false),
        (Some(&sources.data), stage.data(), true),
    ];
    for (source, target, recursive) in binds {
        if let Some(source) = source
            && let Err(error) = attach(source, &target, recursive)
        {
            let _ = stage.release();
            return Err(error);
        }
    }
    publish(lxc_instance, &config)
}

fn attach(source: &OwnedFd, target: &Path, recursive: bool) -> Result<()> {
    let mut flags = OpenTreeFlags::OPEN_TREE_CLONE
        | OpenTreeFlags::OPEN_TREE_CLOEXEC
        | OpenTreeFlags::AT_EMPTY_PATH;
    if recursive {
        flags |= OpenTreeFlags::AT_RECURSIVE;
    }
    let tree = open_tree(source, "", flags).map_err(std::io::Error::from)?;
    move_mount(
        &tree,
        "",
        CWD,
        target,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )
    .map_err(std::io::Error::from)?;
    Ok(())
}

fn publish(lxc_instance: &Path, config: &str) -> Result<()> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Invalid(error.to_string()))?
        .as_nanos();
    let temporary = lxc_instance.join(format!(".config_session.{}-{nonce}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temporary)?;
    let result: Result<()> = (|| {
        file.write_all(config.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, lxc_instance.join("config_session"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Whether the session's sources pass the checks `write` applies, and the
/// Pulse socket exists; for the read-only audit.
///
/// # Errors
/// As for [`write`], before anything is mounted.
pub fn check(fields: &HashMap<String, String>, uid: u32, runtime_root: &Path) -> Result<bool> {
    Ok(sources(fields, uid, runtime_root)?.pulse.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as StdError;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    fn session(root: &Path) -> std::result::Result<HashMap<String, String>, Box<dyn StdError>> {
        let uid = geteuid().as_raw();
        let runtime = root.join("run").join(uid.to_string());
        fs::create_dir_all(runtime.join("pulse"))?;
        fs::create_dir_all(root.join("data"))?;
        UnixListener::bind(runtime.join("marathon-wayland-0"))?;
        UnixListener::bind(runtime.join("pulse/native"))?;
        Ok(HashMap::from([
            ("xdg_runtime_dir".into(), runtime.display().to_string()),
            ("wayland_display".into(), "marathon-wayland-0".into()),
            (
                "pulse_runtime_path".into(),
                runtime.join("pulse").display().to_string(),
            ),
            (
                "waydroid_data".into(),
                root.join("data").display().to_string(),
            ),
        ]))
    }

    #[test]
    fn config_names_only_the_root_owned_stage() {
        let stage = Stage::new(Path::new("/run/waydroid-lxc"));
        assert_eq!(
            render(&stage, true),
            "lxc.mount.entry = tmpfs /run/xdg none create=dir 0 0\n\
             lxc.mount.entry = /run/waydroid-lxc/session/wayland-0 run/xdg/wayland-0 none rbind,create=file 0 0\n\
             lxc.mount.entry = /run/waydroid-lxc/session/pulse-native run/xdg/pulse/native none rbind,create=file 0 0\n\
             lxc.mount.entry = /run/waydroid-lxc/session/data data none rbind 0 0\n"
        );
        assert!(!render(&stage, false).contains("pulse"));
    }

    #[test]
    fn sources_are_pinned_without_following_links() -> std::result::Result<(), Box<dyn StdError>> {
        let root = tempfile::tempdir()?;
        let uid = geteuid().as_raw();
        let mut fields = session(root.path())?;
        let run = root.path().join("run");
        assert!(check(&fields, uid, &run)?);
        assert!(check(&fields, uid + 1, &run).is_err());
        fields.insert("wayland_display".into(), "wayland-0\nmount".into());
        assert!(check(&fields, uid, &run).is_err());
        fields.insert("wayland_display".into(), "marathon-wayland-0".into());
        fs::create_dir(root.path().join("elsewhere"))?;
        fs::remove_dir(root.path().join("data"))?;
        symlink(root.path().join("elsewhere"), root.path().join("data"))?;
        let (canonical, _) = pin(&root.path().join("data"), uid, Kind::Directory)?;
        assert_eq!(canonical, root.path().join("elsewhere"));
        fs::create_dir(root.path().join("elsewhere/inner"))?;
        assert!(open_without_links(&root.path().join("data/inner")).is_err());
        assert!(open_without_links(&root.path().join("elsewhere/inner")).is_ok());
        assert!(pin(&root.path().join("elsewhere"), uid, Kind::Socket).is_err());
        let socket = run.join(uid.to_string()).join("marathon-wayland-0");
        assert!(pin(&socket, uid, Kind::Directory).is_err());
        Ok(())
    }
}
