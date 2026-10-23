use crate::command::{CommandSpec, Runner};
use crate::{Error, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub struct MountLayout {
    pub work: PathBuf,
    pub images: PathBuf,
    pub overlays: bool,
}

impl MountLayout {
    pub fn rootfs(&self) -> PathBuf {
        self.work.join("rootfs")
    }

    pub fn preflight(&self) -> Result<()> {
        safe_dir(&self.work)?;
        safe_dir(&self.images)?;
        for name in ["system.img", "vendor.img"] {
            let path = self.images.join(name);
            safe_regular_file(&path)?;
        }
        safe_regular_file(&self.work.join("waydroid.prop"))?;
        if self.overlays {
            safe_dir(&self.work.join("overlay"))?;
            safe_dir(&self.work.join("overlay/vendor"))?;
        }
        Ok(())
    }

    fn prepare_directories(&self) -> Result<()> {
        for path in [
            self.rootfs(),
            self.rootfs().join("vendor"),
            self.work.join("overlay_rw/system"),
            self.work.join("overlay_work/system"),
            self.work.join("overlay_rw/vendor"),
            self.work.join("overlay_work/vendor"),
        ] {
            if path.starts_with(&self.work) {
                safe_create_dir(&path)?;
            }
        }
        Ok(())
    }

    pub fn commands(&self) -> Result<Vec<CommandSpec>> {
        self.preflight()?;
        let root = self.rootfs();
        let vendor = root.join("vendor");
        let mut commands = vec![loop_mount(&self.images.join("system.img"), &root)];
        if self.overlays {
            commands.push(overlay_mount(
                &[self.work.join("overlay"), root.clone()],
                &self.work.join("overlay_rw/system"),
                &self.work.join("overlay_work/system"),
                &root,
            )?);
        }
        commands.push(loop_mount(&self.images.join("vendor.img"), &vendor));
        if self.overlays {
            commands.push(overlay_mount(
                &[self.work.join("overlay/vendor"), vendor.clone()],
                &self.work.join("overlay_rw/vendor"),
                &self.work.join("overlay_work/vendor"),
                &vendor,
            )?);
        }
        commands.push(CommandSpec::new(
            "mount",
            [
                "--bind".into(),
                self.work.join("waydroid.prop").display().to_string(),
                vendor.join("waydroid.prop").display().to_string(),
            ],
        ));
        Ok(commands)
    }

    pub fn mount(&self, runner: &mut impl Runner) -> Result<()> {
        let commands = self.commands()?;
        self.prepare_directories()?;
        let root = self.rootfs();
        if !mountpoints_beneath(&root, &fs::read_to_string("/proc/self/mountinfo")?).is_empty() {
            return Err(Error::Invalid("rootfs already contains mounts".into()));
        }
        let mut completed = Vec::new();
        for command in commands {
            let Some(target) = command.args.last().cloned() else {
                return Err(Error::Invalid("mount target missing".into()));
            };
            if let Err(error) = runner.run(command) {
                for target in completed.into_iter().rev() {
                    let _ = runner.run(CommandSpec::new("umount", [target]));
                }
                return Err(error);
            }
            completed.push(target);
        }
        Ok(())
    }

    pub fn unmount(&self, runner: &mut impl Runner) -> Result<()> {
        safe_dir(&self.work)?;
        let root = self.rootfs();
        for mountpoint in mountpoints_beneath(&root, &fs::read_to_string("/proc/self/mountinfo")?) {
            runner.run(CommandSpec::new(
                "umount",
                [mountpoint.display().to_string()],
            ))?;
        }
        Ok(())
    }
}

fn loop_mount(image: &Path, target: &Path) -> CommandSpec {
    CommandSpec::new(
        "mount",
        [
            "-o".into(),
            "ro,loop".into(),
            image.display().to_string(),
            target.display().to_string(),
        ],
    )
}

fn overlay_mount(
    lower: &[PathBuf],
    upper: &Path,
    work: &Path,
    target: &Path,
) -> Result<CommandSpec> {
    for path in lower
        .iter()
        .map(PathBuf::as_path)
        .chain([upper, work, target])
    {
        validate_path_syntax(path)?;
    }
    let options = format!(
        "lowerdir={},upperdir={},workdir={},xino=off",
        lower
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(":"),
        upper.display(),
        work.display()
    );
    Ok(CommandSpec::new(
        "mount",
        [
            "-t".into(),
            "overlay".into(),
            "-o".into(),
            options,
            "overlay".into(),
            target.display().to_string(),
        ],
    ))
}

fn validate_path_syntax(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::Invalid(format!("unsafe path: {}", path.display())));
    }
    let value = path.to_string_lossy();
    if value.contains([':', ',', '\n', '\r', '\0', '\\']) {
        return Err(Error::Invalid(format!("unsafe mount option path: {value}")));
    }
    Ok(())
}

fn no_symlink_components(path: &Path) -> Result<()> {
    validate_path_syntax(path)?;
    let mut current = PathBuf::from("/");
    for part in path.components().skip(1) {
        current.push(part);
        match current.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::Invalid(format!(
                    "symlink in mount path: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn safe_dir(path: &Path) -> Result<()> {
    no_symlink_components(path)?;
    if !path.is_dir() {
        return Err(Error::Invalid(format!(
            "directory missing: {}",
            path.display()
        )));
    }
    Ok(())
}

fn safe_regular_file(path: &Path) -> Result<()> {
    no_symlink_components(path)?;
    let metadata = path.metadata()?;
    if !metadata.is_file() {
        return Err(Error::Invalid(format!(
            "image is not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn safe_create_dir(path: &Path) -> Result<()> {
    no_symlink_components(path)?;
    fs::create_dir_all(path)?;
    safe_dir(path)
}

pub fn mountpoints_beneath(root: &Path, mountinfo: &str) -> Vec<PathBuf> {
    let mut entries: Vec<(u64, PathBuf)> = mountinfo
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            let id = fields.first()?.parse().ok()?;
            let mountpoint = PathBuf::from(fields.get(4)?.replace("\\040", " "));
            (mountpoint == root || mountpoint.starts_with(root)).then_some((id, mountpoint))
        })
        .collect();
    entries.sort_by(|(id_a, path_a), (id_b, path_b)| {
        path_b
            .components()
            .count()
            .cmp(&path_a.components().count())
            .then(id_b.cmp(id_a))
    });
    entries.into_iter().map(|(_, path)| path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rejects_overlay_metacharacters_and_symlinks() -> Result<()> {
        assert!(validate_path_syntax(Path::new("/var/lib/foo:bar")).is_err());
        assert!(validate_path_syntax(Path::new("/var/lib/foo,bar")).is_err());
        assert!(validate_path_syntax(Path::new("/var/../lib/waydroid")).is_err());
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Invalid(e.to_string()))?
            .as_nanos();
        let base = std::env::temp_dir().join(format!("deckard-container-{id}"));
        fs::create_dir(&base)?;
        let link = base.join("link");
        std::os::unix::fs::symlink("/tmp", &link)?;
        assert!(no_symlink_components(&link.join("image.img")).is_err());
        fs::remove_file(link)?;
        fs::remove_dir(base)?;
        Ok(())
    }

    #[test]
    fn nested_and_stacked_mounts_unmount_in_safe_order() {
        let table = "1 0 0:1 / /var/lib/deckard/rootfs rw - ext4 x y\n2 1 0:2 / /var/lib/deckard/rootfs rw - overlay x y\n3 2 0:3 / /var/lib/deckard/rootfs/vendor rw - ext4 x y\n4 3 0:4 / /var/lib/deckard/rootfs/vendor rw - overlay x y\n5 0 0:5 / /var/lib/deckard/rootfs-extra rw - ext4 x y";
        let mounts = mountpoints_beneath(Path::new("/var/lib/deckard/rootfs"), table);
        assert_eq!(
            mounts,
            [
                PathBuf::from("/var/lib/deckard/rootfs/vendor"),
                PathBuf::from("/var/lib/deckard/rootfs/vendor"),
                PathBuf::from("/var/lib/deckard/rootfs"),
                PathBuf::from("/var/lib/deckard/rootfs"),
            ]
        );
    }

    #[test]
    fn system_vendor_mounts_are_read_only() {
        let command = loop_mount(Path::new("/images/system.img"), Path::new("/rootfs"));
        assert_eq!(command.args[1], "ro,loop");
        assert_eq!(command.program, "mount");
    }

    #[test]
    fn image_and_overlay_mount_order_matches_installed_runtime() -> Result<()> {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Invalid(e.to_string()))?
            .as_nanos();
        let base = std::env::temp_dir().join(format!("deckard-mount-plan-{id}"));
        let work = base.join("work");
        let images = base.join("images");
        fs::create_dir_all(work.join("overlay/vendor"))?;
        fs::create_dir(&images)?;
        fs::write(images.join("system.img"), b"system")?;
        fs::write(images.join("vendor.img"), b"vendor")?;
        fs::write(work.join("waydroid.prop"), b"ro.adb.secure=1\n")?;
        let layout = MountLayout {
            work,
            images,
            overlays: true,
        };
        let commands = layout.commands()?;
        assert_eq!(commands.len(), 5);
        assert!(
            commands[0]
                .args
                .iter()
                .any(|arg| arg.ends_with("system.img"))
        );
        assert!(
            commands[1]
                .args
                .iter()
                .any(|arg| arg.starts_with("lowerdir="))
        );
        assert!(
            commands[2]
                .args
                .iter()
                .any(|arg| arg.ends_with("vendor.img"))
        );
        assert!(
            commands[3]
                .args
                .iter()
                .any(|arg| arg.contains("overlay/vendor"))
        );
        assert_eq!(commands[4].program, "mount");
        assert_eq!(commands[4].args[0], "--bind");
        assert!(commands[4].args[2].ends_with("rootfs/vendor/waydroid.prop"));
        fs::remove_dir_all(base)?;
        Ok(())
    }
}
