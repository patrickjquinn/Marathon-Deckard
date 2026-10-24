//! Image, overlay and property mounts of the Android rootfs, run through
//! mount(8) by path. Every path must be controlled by root
//! ([`owner_controlled`]), so only root could swap one between the check and
//! the mount, and root already owns the host. Session-owned paths are bound
//! from descriptors instead (`session_config`).

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use deckard_host_tools::files;

use crate::Result;
use crate::command::{self, Command};

#[derive(Clone, Debug)]
pub struct MountLayout {
    pub work: PathBuf,
    pub images: PathBuf,
    /// Whether `overlay/` is mounted over the images (`mount_overlays`).
    pub overlays: bool,
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

impl MountLayout {
    #[must_use]
    pub fn rootfs(&self) -> PathBuf {
        self.work.join("rootfs")
    }

    /// # Errors
    /// A directory or image is missing or not controlled by root.
    pub fn preflight(&self) -> Result<()> {
        safe_dir(&self.work)?;
        safe_dir(&self.images)?;
        for path in [
            self.images.join("system.img"),
            self.images.join("vendor.img"),
            self.work.join("waydroid.prop"),
        ] {
            owner_controlled(&path)?;
            if !path.metadata()?.is_file() {
                return Err(format!("image is not a regular file: {}", path.display()).into());
            }
        }
        if self.overlays {
            safe_dir(&self.work.join("overlay"))?;
            safe_dir(&self.work.join("overlay/vendor"))?;
        }
        Ok(())
    }

    /// The mounts, in order, each with its target last.
    ///
    /// # Errors
    /// As for [`preflight`](Self::preflight), or a path is unusable in mount options.
    pub fn commands(&self) -> Result<Vec<Command>> {
        self.preflight()?;
        let root = self.rootfs();
        let mut commands = Vec::new();
        for (image, target, lower, overlay) in [
            (
                "system.img",
                root.clone(),
                self.work.join("overlay"),
                "system",
            ),
            (
                "vendor.img",
                root.join("vendor"),
                self.work.join("overlay/vendor"),
                "vendor",
            ),
        ] {
            let image = display(&self.images.join(image));
            commands.push(Command::new([
                "mount",
                "-o",
                "ro,loop",
                &image,
                &display(&target),
            ]));
            if self.overlays {
                let upper = self.work.join("overlay_rw").join(overlay);
                let work = self.work.join("overlay_work").join(overlay);
                for path in [&lower, &upper, &work, &target] {
                    validate_path_syntax(path)?;
                }
                let options = format!(
                    "lowerdir={}:{},upperdir={},workdir={},xino=off",
                    display(&lower),
                    display(&target),
                    display(&upper),
                    display(&work)
                );
                let args = [
                    "mount",
                    "-t",
                    "overlay",
                    "-o",
                    &options,
                    "overlay",
                    &display(&target),
                ];
                commands.push(Command::new(args));
            }
        }
        let prop = display(&self.work.join("waydroid.prop"));
        let target = display(&root.join("vendor/waydroid.prop"));
        commands.push(Command::new(["mount", "--bind", &prop, &target]));
        Ok(commands)
    }

    /// Mounts everything, unmounting what was mounted on failure.
    ///
    /// # Errors
    /// The layout is unsafe, the rootfs already holds mounts or a mount fails.
    pub fn mount(&self) -> Result<()> {
        let commands = self.commands()?;
        for path in [
            self.rootfs(),
            self.rootfs().join("vendor"),
            self.work.join("overlay_rw/system"),
            self.work.join("overlay_work/system"),
            self.work.join("overlay_rw/vendor"),
            self.work.join("overlay_work/vendor"),
        ] {
            owner_controlled(&path)?;
            fs::create_dir_all(&path)?;
            safe_dir(&path)?;
        }
        let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
        if !mountpoints_beneath(&self.rootfs(), &mountinfo).is_empty() {
            return Err("rootfs already contains mounts".into());
        }
        let mut completed = Vec::new();
        for command in commands {
            let target = command.args.last().cloned().ok_or("mount target missing")?;
            if let Err(error) = command::run(command) {
                for target in completed.into_iter().rev() {
                    let _ = command::run(Command::new(["umount".into(), target]));
                }
                return Err(error);
            }
            completed.push(target);
        }
        Ok(())
    }

    /// # Errors
    /// The work directory is unsafe or an unmount fails.
    pub fn unmount(&self) -> Result<()> {
        safe_dir(&self.work)?;
        let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
        for mountpoint in mountpoints_beneath(&self.rootfs(), &mountinfo) {
            command::run(Command::new(["umount".into(), display(&mountpoint)]))?;
        }
        Ok(())
    }
}

fn validate_path_syntax(path: &Path) -> Result<()> {
    let plain = path
        .components()
        .all(|part| matches!(part, Component::RootDir | Component::Normal(_)));
    if !path.is_absolute() || !plain {
        return Err(format!("unsafe path: {}", path.display()).into());
    }
    let value = path.to_string_lossy();
    if value.contains([':', ',', '\n', '\r', '\0', '\\']) {
        return Err(format!("unsafe mount option path: {value}").into());
    }
    Ok(())
}

/// Every existing component of `path` is a real file or directory owned by
/// root or this process and not writable by group or others, except sticky
/// directories, whose entries others cannot replace. Then only root can swap
/// a component between this check and a mount.
///
/// # Errors
/// A component is a link, foreign, or writable by others.
pub(crate) fn owner_controlled(path: &Path) -> Result<()> {
    validate_path_syntax(path)?;
    let owners = [0, rustix::process::geteuid().as_raw()];
    let mut current = PathBuf::from("/");
    let mut components = path.components().skip(1).peekable();
    loop {
        let Some(metadata) = files::found(current.symlink_metadata())? else {
            return Ok(());
        };
        let shared = metadata.mode() & 0o022 != 0;
        let sticky_dir = metadata.is_dir() && metadata.mode() & 0o1000 != 0;
        if metadata.is_symlink()
            || !owners.contains(&metadata.uid())
            || (shared && !(sticky_dir && components.peek().is_some()))
        {
            let current = current.display();
            return Err(format!("mount path is not controlled by root: {current}").into());
        }
        let Some(part) = components.next() else {
            return Ok(());
        };
        current.push(part);
    }
}

fn safe_dir(path: &Path) -> Result<()> {
    owner_controlled(path)?;
    if !path.is_dir() {
        return Err(format!("directory missing: {}", path.display()).into());
    }
    Ok(())
}

/// The mount points at or below `root`, deepest and newest first.
#[must_use]
pub fn mountpoints_beneath(root: &Path, mountinfo: &str) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(id), Some(point)) = (fields.first(), fields.get(4)) else {
            continue;
        };
        let point = PathBuf::from(point.replace("\\040", " "));
        if let Ok(id) = id.parse::<u64>()
            && point.starts_with(root)
        {
            entries.push((point.components().count(), id, point));
        }
    }
    entries.sort_by(|a, b| b.cmp(a));
    entries.into_iter().map(|(_, _, path)| path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_overlay_metacharacters_and_symlinks() -> Result<()> {
        assert!(validate_path_syntax(Path::new("/var/lib/foo:bar")).is_err());
        assert!(validate_path_syntax(Path::new("/var/lib/foo,bar")).is_err());
        assert!(validate_path_syntax(Path::new("/var/../lib/waydroid")).is_err());
        let base = tempfile::tempdir()?;
        let link = base.path().join("link");
        std::os::unix::fs::symlink("/tmp", &link)?;
        assert!(owner_controlled(&link.join("image.img")).is_err());
        Ok(())
    }

    #[test]
    fn nested_and_stacked_mounts_unmount_in_safe_order() {
        let table = "1 0 0:1 / /var/lib/deckard/rootfs rw - ext4 x y\n2 1 0:2 / /var/lib/deckard/rootfs rw - overlay x y\n3 2 0:3 / /var/lib/deckard/rootfs/vendor rw - ext4 x y\n4 3 0:4 / /var/lib/deckard/rootfs/vendor rw - overlay x y\n5 0 0:5 / /var/lib/deckard/rootfs-extra rw - ext4 x y";
        let mounts = mountpoints_beneath(Path::new("/var/lib/deckard/rootfs"), table);
        let vendor = PathBuf::from("/var/lib/deckard/rootfs/vendor");
        let root = PathBuf::from("/var/lib/deckard/rootfs");
        assert_eq!(mounts, [vendor.clone(), vendor, root.clone(), root]);
    }
}
