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

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use deckard_host_tools::files;
use rustix::fd::OwnedFd;
use rustix::fs::{CWD, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;
use rustix::mount::{MoveMountFlags, OpenTreeFlags, UnmountFlags};

use crate::Result;
use crate::mounts;

/// Where the pinned session sources are mounted for LXC.
pub struct Stage {
    root: PathBuf,
}

impl Stage {
    /// The stage under the daemon's runtime directory.
    #[must_use]
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

    /// Unmounts whatever a previous session left on the stage.
    ///
    /// # Errors
    /// A stage mount cannot be removed.
    pub fn release(&self) -> Result<()> {
        for target in [self.wayland(), self.pulse(), self.data()] {
            // Bounded: a stage target holds at most a few stacked mounts.
            for _ in 0..8 {
                match rustix::mount::unmount(&target, UnmountFlags::DETACH | UnmountFlags::NOFOLLOW)
                {
                    Ok(()) => {}
                    Err(Errno::INVAL | Errno::NOENT) => break,
                    Err(error) => return Err(error.into()),
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
        mounts::owner_controlled(&self.root)?;
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
        for target in [self.wayland(), self.pulse(), self.data()] {
            mounts::owner_controlled(&target)?;
        }
        Ok(())
    }
}

fn field<'a>(fields: &'a HashMap<String, String>, key: &str) -> Result<&'a str> {
    let value = fields
        .get(key)
        .ok_or(format!("missing session field: {key}"))?;
    Ok(value)
}

fn safe_path(path: &Path) -> Result<()> {
    let value = path.to_string_lossy();
    let safe =
        path.is_absolute() && !value.chars().any(char::is_whitespace) && !value.contains('\0');
    crate::ensure(safe, format!("unsafe LXC mount path: {value}"))
}

/// `path` resolved once, then opened without following any link and checked
/// on the descriptor: owned by `uid` and of the `expected` type.
fn pin(path: &Path, uid: u32, expected: FileType) -> Result<(PathBuf, OwnedFd)> {
    safe_path(path)?;
    let canonical = fs::canonicalize(path)?;
    safe_path(&canonical)?;
    let fd = open_without_links(&canonical)?;
    let stat = rustix::fs::fstat(&fd)?;
    let kind = if expected == FileType::Socket {
        "socket"
    } else {
        "directory"
    };
    let matches = stat.st_uid == uid && FileType::from_raw_mode(stat.st_mode) == expected;
    crate::ensure(
        matches,
        format!(
            "session path is not a {kind} owned by UID {uid}: {}",
            path.display()
        ),
    )?;
    Ok((canonical, fd))
}

fn open_without_links(path: &Path) -> Result<OwnedFd> {
    let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let resolve = ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS;
    Ok(rustix::fs::openat2(
        CWD,
        path,
        flags,
        Mode::empty(),
        resolve,
    )?)
}

struct Sources {
    wayland: OwnedFd,
    pulse: Option<OwnedFd>,
    data: OwnedFd,
}

fn sources(fields: &HashMap<String, String>, uid: u32, runtime_root: &Path) -> Result<Sources> {
    let runtime = runtime_root.join(uid.to_string());
    safe_path(&runtime)?;
    let expected = Path::new(field(fields, "xdg_runtime_dir")?) == runtime;
    crate::ensure(expected, "unexpected XDG runtime directory")?;
    crate::ensure(
        runtime.metadata()?.uid() == uid,
        "XDG runtime directory has wrong owner",
    )?;
    let display = field(fields, "wayland_display")?;
    let plain =
        !display.is_empty() && !display.contains('/') && !display.chars().any(char::is_whitespace);
    crate::ensure(plain, "invalid Wayland display name")?;
    let (wayland_path, wayland) = pin(&runtime.join(display), uid, FileType::Socket)?;
    crate::ensure(
        wayland_path.starts_with(&runtime),
        "Wayland socket escapes runtime directory",
    )?;
    let pulse_path = Path::new(field(fields, "pulse_runtime_path")?).join("native");
    let pulse = if pulse_path.exists() {
        let (canonical, fd) = pin(&pulse_path, uid, FileType::Socket)?;
        crate::ensure(
            canonical.starts_with(&runtime),
            "Pulse socket escapes runtime directory",
        )?;
        Some(fd)
    } else {
        None
    };
    let data = Path::new(field(fields, "deckard_data")?);
    let (_, data) = pin(data, uid, FileType::Directory)?;
    Ok(Sources {
        wayland,
        pulse,
        data,
    })
}

/// The `config_session` text for a stage.
#[must_use]
pub fn render(stage: &Stage, pulse: bool) -> String {
    let mut config = format!(
        "lxc.mount.entry = tmpfs /run/xdg none create=dir 0 0\n\
         lxc.mount.entry = {} run/xdg/wayland-0 none rbind,create=file 0 0\n",
        stage.wayland().display()
    );
    if pulse {
        let pulse = stage.pulse();
        let pulse = pulse.display();
        let _ = writeln!(
            config,
            "lxc.mount.entry = {pulse} run/xdg/pulse/native none rbind,create=file 0 0"
        );
    }
    let data = stage.data();
    let _ = writeln!(
        config,
        "lxc.mount.entry = {} data none rbind 0 0",
        data.display()
    );
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
    let trusted = directory.is_dir()
        && directory.uid() == rustix::process::geteuid().as_raw()
        && directory.mode() & 0o022 == 0;
    crate::ensure(trusted, "untrusted LXC instance directory")?;
    stage.release()?;
    stage.prepare()?;
    let binds = [
        (
            Some(&sources.wayland),
            stage.wayland(),
            OpenTreeFlags::empty(),
        ),
        (
            sources.pulse.as_ref(),
            stage.pulse(),
            OpenTreeFlags::empty(),
        ),
        (
            Some(&sources.data),
            stage.data(),
            OpenTreeFlags::AT_RECURSIVE,
        ),
    ];
    for (source, target, recursive) in binds {
        if let Some(source) = source
            && let Err(error) = attach(source, &target, recursive)
        {
            let _ = stage.release();
            return Err(error);
        }
    }
    Ok(files::replace(
        &lxc_instance.join("config_session"),
        config.as_bytes(),
        0o644,
    )?)
}

fn attach(source: &OwnedFd, target: &Path, recursive: OpenTreeFlags) -> Result<()> {
    let flags = OpenTreeFlags::OPEN_TREE_CLONE
        | OpenTreeFlags::OPEN_TREE_CLOEXEC
        | OpenTreeFlags::AT_EMPTY_PATH
        | recursive;
    let tree = rustix::mount::open_tree(source, "", flags)?;
    rustix::mount::move_mount(
        &tree,
        "",
        CWD,
        target,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )?;
    Ok(())
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
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    fn session(root: &Path) -> Result<HashMap<String, String>> {
        let uid = rustix::process::geteuid().as_raw();
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
                "deckard_data".into(),
                root.join("data").display().to_string(),
            ),
        ]))
    }

    #[test]
    fn config_names_only_the_root_owned_stage() {
        let stage = Stage::new(Path::new("/run/deckard-lxc"));
        assert_eq!(
            render(&stage, true),
            "lxc.mount.entry = tmpfs /run/xdg none create=dir 0 0\n\
             lxc.mount.entry = /run/deckard-lxc/session/wayland-0 run/xdg/wayland-0 none rbind,create=file 0 0\n\
             lxc.mount.entry = /run/deckard-lxc/session/pulse-native run/xdg/pulse/native none rbind,create=file 0 0\n\
             lxc.mount.entry = /run/deckard-lxc/session/data data none rbind 0 0\n"
        );
        assert!(!render(&stage, false).contains("pulse"));
    }

    #[test]
    fn sources_are_pinned_without_following_links() -> Result<()> {
        let root = tempfile::tempdir()?;
        let uid = rustix::process::geteuid().as_raw();
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
        let (canonical, _) = pin(&root.path().join("data"), uid, FileType::Directory)?;
        assert_eq!(canonical, root.path().join("elsewhere"));
        fs::create_dir(root.path().join("elsewhere/inner"))?;
        assert!(open_without_links(&root.path().join("data/inner")).is_err());
        assert!(open_without_links(&root.path().join("elsewhere/inner")).is_ok());
        assert!(pin(&root.path().join("elsewhere"), uid, FileType::Socket).is_err());
        let socket = run.join(uid.to_string()).join("marathon-wayland-0");
        assert!(pin(&socket, uid, FileType::Directory).is_err());
        Ok(())
    }
}
