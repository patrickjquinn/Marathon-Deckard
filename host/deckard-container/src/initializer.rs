//! Offline, local-image portion of Waydroid's initializer. Nothing here stops
//! a container, downloads images, or modifies an existing LXC instance.

use crate::{Error, Result};
use rustix::fs::{CWD, RenameFlags, renameat_with};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Inputs for the preinstalled/local-image branch of `waydroid init`.
/// The output is always `work/lxc/name` and must not already exist.
pub struct LocalImageInit {
    pub work: PathBuf,
    pub images: PathBuf,
    pub templates: PathBuf,
    pub name: String,
    pub bridge: String,
    pub arch: String,
    pub lxc_major: u32,
    pub binder: String,
    pub vndbinder: String,
    pub hwbinder: String,
}

/// Host device nodes that existed when static config was generated.
#[derive(Default)]
pub struct NodeInventory {
    paths: BTreeSet<String>,
}

impl NodeInventory {
    pub fn from_host() -> Result<Self> {
        let mut inventory = Self::default();
        for path in [
            "/dev/zero",
            "/dev/null",
            "/dev/full",
            "/dev/ashmem",
            "/dev/fuse",
            "/dev/ion",
            "/dev/tty",
            "/dev/char",
            "/dev/kgsl-3d0",
            "/dev/mali0",
            "/dev/pvr_sync",
            "/dev/pmsg0",
            "/dev/dxg",
            "/dev/uhid",
            "/dev/net/tun",
            "/dev/sw_sync",
            "/dev/Vcodec",
            "/dev/MTK_SMI",
            "/dev/mdp_sync",
            "/dev/mtk_cmdq",
            "/sys/module/lowmemorykiller",
            "/sys/kernel/debug",
            "/sys/class/leds/vibrator",
            "/sys/devices/virtual/timed_output/vibrator",
            "/mnt/wslg",
            "/system/etc/libnfc-nci.conf",
        ] {
            if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_symlink()) {
                inventory.paths.insert(path.to_owned());
            }
        }
        for dir in ["/dev/dri", "/dev", "/dev/graphics", "/dev/dma_heap"] {
            if !Path::new(dir).is_dir() {
                continue;
            }
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let supported = match dir {
                    "/dev/dri" => name.starts_with("renderD"),
                    "/dev" => name.starts_with("fb") || name.starts_with("video"),
                    "/dev/graphics" => name.starts_with("fb"),
                    _ => true,
                };
                if supported && entry.file_type()?.is_char_device() {
                    let path = entry.path().to_string_lossy().into_owned();
                    validate_token(&path)?;
                    inventory.paths.insert(path);
                }
            }
        }
        Ok(inventory)
    }

    pub fn from_paths(paths: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut inventory = Self::default();
        for path in paths {
            validate_token(&path)?;
            if !path.starts_with('/') {
                return Err(Error::Invalid("device path must be absolute".into()));
            }
            inventory.paths.insert(path);
        }
        Ok(inventory)
    }

    fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }
}

struct Rendered {
    config: String,
    nodes: String,
    seccomp: Vec<u8>,
}

impl LocalImageInit {
    fn validate(&self) -> Result<()> {
        for path in [&self.work, &self.images, &self.templates] {
            if !path.is_absolute() {
                return Err(Error::Invalid("initializer paths must be absolute".into()));
            }
            if path
                .components()
                .any(|component| component == Component::ParentDir)
            {
                return Err(Error::Invalid(
                    "initializer paths must not contain '..'".into(),
                ));
            }
            validate_token(&path.to_string_lossy())?;
        }
        for token in [
            &self.name,
            &self.bridge,
            &self.arch,
            &self.binder,
            &self.vndbinder,
            &self.hwbinder,
        ] {
            validate_identifier(token)?;
        }
        if self.lxc_major < 3 {
            return Err(Error::Invalid("only LXC 3 or newer is supported".into()));
        }
        for image in ["system.img", "vendor.img"] {
            validate_ext_image(&self.images.join(image))?;
        }
        Ok(())
    }

    fn render(&self, inventory: &NodeInventory) -> Result<Rendered> {
        self.validate()?;
        let base = read_regular(&self.templates.join("config_base"))?;
        let version3 = read_regular(&self.templates.join("config_3"))?;
        let mut config = String::from_utf8(base)
            .map_err(|_| Error::Invalid("non-UTF-8 LXC base template".into()))?;
        config.push_str(
            &String::from_utf8(version3)
                .map_err(|_| Error::Invalid("non-UTF-8 LXC v3 template".into()))?,
        );
        if self.lxc_major >= 4 && self.templates.join("config_4").exists() {
            config.push_str(
                &String::from_utf8(read_regular(&self.templates.join("config_4"))?)
                    .map_err(|_| Error::Invalid("non-UTF-8 LXC v4 template".into()))?,
            );
        }
        for required in [
            "lxc.rootfs.path = /var/lib/waydroid/rootfs",
            "lxc.include = /var/lib/waydroid/lxc/waydroid/config_nodes",
            "lxc.include = /var/lib/waydroid/lxc/waydroid/config_session",
            "lxc.seccomp.profile = /var/lib/waydroid/lxc/waydroid/waydroid.seccomp",
            "lxc.net.0.link = waydroid0",
            "lxc.uts.name = waydroid",
            "lxc.arch = LXCARCH",
        ] {
            if !config.contains(required) {
                return Err(Error::Invalid(format!("LXC template missing {required}")));
            }
        }
        let work = self.work.to_string_lossy();
        config = config.replace(
            "/var/lib/waydroid/lxc/waydroid/",
            &format!("{work}/lxc/{}/", self.name),
        );
        config = config.replace("/var/lib/waydroid/", &format!("{work}/"));
        config = config.replace(
            "lxc.net.0.link = waydroid0",
            &format!("lxc.net.0.link = {}", self.bridge),
        );
        config = config.replace(
            "lxc.uts.name = waydroid",
            &format!("lxc.uts.name = {}", self.name),
        );
        config = config.replace("LXCARCH", &self.arch);
        if config.contains("/var/lib/waydroid") || config.contains("LXCARCH") {
            return Err(Error::Invalid("unresolved LXC template token".into()));
        }
        let seccomp = read_regular(&self.templates.join("waydroid.seccomp"))?;
        if seccomp.is_empty() {
            return Err(Error::Invalid("empty LXC seccomp profile".into()));
        }
        Ok(Rendered {
            config,
            nodes: self.render_nodes(inventory)?,
            seccomp,
        })
    }

    /// Publish a new, offline LXC config directory. A failed write is removed;
    /// `RENAME_NOREPLACE` prevents replacing a concurrent or existing instance.
    pub fn prepare_local(&self, inventory: &NodeInventory) -> Result<PathBuf> {
        self.prepare_local_cancellable(inventory, &AtomicBool::new(false), None)
    }

    /// Cancellation and deadline are checked before each write and publication.
    /// They cannot interrupt a filesystem operation already in progress.
    pub fn prepare_local_cancellable(
        &self,
        inventory: &NodeInventory,
        cancelled: &AtomicBool,
        deadline: Option<std::time::Instant>,
    ) -> Result<PathBuf> {
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                return Err(Error::Invalid("local initialization cancelled".into()));
            }
            if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
                return Err(Error::Timeout("local initialization timed out".into()));
            }
            Ok(())
        };
        check()?;
        let rendered = self.render(inventory)?;
        check()?;
        let parent = self.work.join("lxc");
        let destination = parent.join(&self.name);
        let metadata = fs::symlink_metadata(&parent)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::Invalid("LXC parent is not a directory".into()));
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::Invalid(error.to_string()))?
            .as_nanos();
        let staging = parent.join(format!(
            ".{}-{}-{nonce}.staging",
            self.name,
            std::process::id()
        ));
        fs::create_dir(&staging)?;
        let result = (|| -> Result<()> {
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))?;
            check()?;
            write_new(&staging.join("config"), rendered.config.as_bytes())?;
            check()?;
            write_new(&staging.join("config_nodes"), rendered.nodes.as_bytes())?;
            check()?;
            write_new(&staging.join("config_session"), b"")?;
            check()?;
            write_new(&staging.join("waydroid.seccomp"), &rendered.seccomp)?;
            File::open(&staging)?.sync_all()?;
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))?;
            check()?;
            renameat_with(CWD, &staging, CWD, &destination, RenameFlags::NOREPLACE)
                .map_err(std::io::Error::from)?;
            File::open(&parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result.map(|()| destination)
    }

    fn render_nodes(&self, inventory: &NodeInventory) -> Result<String> {
        let mut nodes = String::from("lxc.mount.entry = tmpfs dev tmpfs nosuid 0 0\n");
        for path in [
            "/dev/zero",
            "/dev/null",
            "/dev/full",
            "/dev/ashmem",
            "/dev/fuse",
            "/dev/ion",
            "/dev/tty",
            "/dev/char",
            "/dev/kgsl-3d0",
            "/dev/mali0",
            "/dev/pvr_sync",
            "/dev/pmsg0",
            "/dev/dxg",
        ] {
            if inventory.contains(path) {
                let options = if path == "/dev/char" {
                    "bind,create=dir,optional"
                } else {
                    "bind,create=file,optional"
                };
                push_node(&mut nodes, path, &path[1..], "none", options);
            }
        }
        for path in &inventory.paths {
            if path.starts_with("/dev/dri/renderD")
                || path.starts_with("/dev/fb")
                || path.starts_with("/dev/graphics/fb")
                || path.starts_with("/dev/video")
                || path.starts_with("/dev/dma_heap/")
            {
                push_node(
                    &mut nodes,
                    path,
                    &path[1..],
                    "none",
                    "bind,create=file,optional",
                );
            }
        }
        for (source, target) in [
            (&self.binder, "dev/binder"),
            (&self.vndbinder, "dev/vndbinder"),
            (&self.hwbinder, "dev/hwbinder"),
        ] {
            push_node(
                &mut nodes,
                &format!("/dev/{source}"),
                target,
                "none",
                "bind,create=file,optional",
            );
        }
        push_node(
            &mut nodes,
            "none",
            "dev/pts",
            "devpts",
            "defaults,mode=644,ptmxmode=666,create=dir",
        );
        for path in [
            "/dev/uhid",
            "/dev/net/tun",
            "/dev/sw_sync",
            "/dev/Vcodec",
            "/dev/MTK_SMI",
            "/dev/mdp_sync",
            "/dev/mtk_cmdq",
        ] {
            if inventory.contains(path) {
                let target = if path == "/dev/net/tun" {
                    "dev/tun"
                } else {
                    &path[1..]
                };
                push_node(
                    &mut nodes,
                    path,
                    target,
                    "none",
                    "bind,create=file,optional",
                );
            }
        }
        let host_perms = self.work.join("host-permissions");
        if host_perms.exists() {
            push_node(
                &mut nodes,
                &host_perms.to_string_lossy(),
                "vendor/etc/host-permissions",
                "none",
                "bind,optional",
            );
        }
        for path in [
            "/sys/module/lowmemorykiller",
            "/sys/kernel/debug",
            "/sys/class/leds/vibrator",
            "/sys/devices/virtual/timed_output/vibrator",
            "/mnt/wslg",
            "/system/etc/libnfc-nci.conf",
        ] {
            if inventory.contains(path) {
                let (target, options) = match path {
                    "/sys/kernel/debug" => (&path[1..], "rbind,create=dir,optional"),
                    "/mnt/wslg" => ("mnt_extra/wslg", "rbind,create=dir,optional"),
                    "/system/etc/libnfc-nci.conf" => (&path[1..], "bind,optional"),
                    _ => (&path[1..], "bind,create=dir,optional"),
                };
                push_node(&mut nodes, path, target, "none", options);
            }
        }
        push_node(&mut nodes, "tmpfs", "mnt_extra", "tmpfs", "nodev");
        for path in ["tmp", "var", "run"] {
            push_node(&mut nodes, "tmpfs", path, "tmpfs", "nodev");
        }
        Ok(nodes)
    }
}

fn push_node(output: &mut String, source: &str, target: &str, kind: &str, options: &str) {
    output.push_str(&format!(
        "lxc.mount.entry = {source} {target} {kind} {options} 0 0\n"
    ));
}

fn validate_token(token: &str) -> Result<()> {
    if token.is_empty()
        || token
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0 || byte == b'\\')
    {
        return Err(Error::Invalid("unsafe LXC token".into()));
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<()> {
    validate_token(value)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(Error::Invalid("invalid LXC identifier".into()));
    }
    Ok(())
}

fn read_regular(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(Error::Invalid(format!(
            "not a regular template: {}",
            path.display()
        )));
    }
    if metadata.len() > 1024 * 1024 {
        return Err(Error::Invalid(format!(
            "oversized LXC template: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::Invalid(format!(
            "oversized LXC template: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

fn validate_ext_image(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !(metadata.file_type().is_file() || metadata.file_type().is_block_device())
    {
        return Err(Error::Invalid(format!(
            "not a local image: {}",
            path.display()
        )));
    }
    let mut image = File::open(path)?;
    image.seek(SeekFrom::Start(1024 + 56))?;
    let mut magic = [0; 2];
    image.read_exact(&mut magic)?;
    if magic != [0x53, 0xef] {
        return Err(Error::Invalid(format!(
            "not an ext image: {}",
            path.display()
        )));
    }
    Ok(())
}

fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| Error::Invalid(error.to_string()))?
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("deckard-init-{}-{nonce}", std::process::id()));
            fs::create_dir(&root)?;
            for path in ["work/lxc", "images", "templates"] {
                fs::create_dir_all(root.join(path))?;
            }
            for image in ["system.img", "vendor.img"] {
                let mut bytes = vec![0; 1082];
                bytes[1080..1082].copy_from_slice(&[0x53, 0xef]);
                fs::write(root.join("images").join(image), bytes)?;
            }
            fs::write(
                root.join("templates/config_base"),
                b"lxc.rootfs.path = /var/lib/waydroid/rootfs\nlxc.arch = LXCARCH\nlxc.include = /var/lib/waydroid/lxc/waydroid/config_nodes\nlxc.include = /var/lib/waydroid/lxc/waydroid/config_session\nlxc.hook.mount = /usr/libexec/deckard-shared-folders\n",
            )?;
            fs::write(
                root.join("templates/config_3"),
                b"lxc.uts.name = waydroid\nlxc.seccomp.profile = /var/lib/waydroid/lxc/waydroid/waydroid.seccomp\nlxc.net.0.link = waydroid0\n",
            )?;
            fs::write(
                root.join("templates/config_4"),
                b"lxc.seccomp.allow_nesting = 1\n",
            )?;
            fs::write(root.join("templates/waydroid.seccomp"), b"2\nblacklist\n")?;
            Ok(Self { root })
        }

        fn init(&self) -> LocalImageInit {
            LocalImageInit {
                work: self.root.join("work"),
                images: self.root.join("images"),
                templates: self.root.join("templates"),
                name: "deckard".into(),
                bridge: "deckard0".into(),
                arch: "aarch64".into(),
                lxc_major: 7,
                binder: "binder".into(),
                vndbinder: "vndbinder".into(),
                hwbinder: "hwbinder".into(),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn prepares_static_config_from_local_images_without_overwriting() -> Result<()> {
        let fixture = Fixture::new()?;
        let init = fixture.init();
        let inventory = NodeInventory::from_paths([
            "/dev/zero".into(),
            "/dev/dri/renderD128".into(),
            "/dev/dma_heap/system".into(),
            "/dev/net/tun".into(),
        ])?;
        let output = init.prepare_local(&inventory)?;
        let config = fs::read_to_string(output.join("config"))?;
        let nodes = fs::read_to_string(output.join("config_nodes"))?;
        assert!(config.contains(&format!("lxc.rootfs.path = {}/rootfs", init.work.display())));
        assert!(config.contains(&format!(
            "lxc.include = {}/lxc/deckard/config_nodes",
            init.work.display()
        )));
        assert!(config.contains("lxc.arch = aarch64"));
        assert!(config.contains("lxc.uts.name = deckard"));
        assert!(config.contains("lxc.net.0.link = deckard0"));
        assert!(config.contains("lxc.seccomp.allow_nesting = 1"));
        assert!(config.contains("lxc.hook.mount = /usr/libexec/deckard-shared-folders"));
        assert!(nodes.contains("/dev/dri/renderD128 dev/dri/renderD128"));
        assert!(nodes.contains("/dev/net/tun dev/tun"));
        assert!(nodes.contains("/dev/binder dev/binder"));
        assert!(!nodes.contains("/dev/video"));
        assert_eq!(fs::read(output.join("config_session"))?, b"");
        assert_eq!(
            fs::read(output.join("waydroid.seccomp"))?,
            b"2\nblacklist\n"
        );
        assert!(init.prepare_local(&inventory).is_err());
        assert_eq!(fs::read_to_string(output.join("config"))?, config);
        assert_eq!(fs::read_dir(init.work.join("lxc"))?.count(), 1);
        Ok(())
    }

    #[test]
    fn rejects_invalid_and_symlinked_images_before_writing() -> Result<()> {
        let fixture = Fixture::new()?;
        let init = fixture.init();
        let inventory = NodeInventory::default();
        fs::write(init.images.join("vendor.img"), b"not an image")?;
        assert!(init.prepare_local(&inventory).is_err());
        fs::remove_file(init.images.join("vendor.img"))?;
        symlink(
            init.images.join("system.img"),
            init.images.join("vendor.img"),
        )?;
        assert!(init.prepare_local(&inventory).is_err());
        assert_eq!(fs::read_dir(init.work.join("lxc"))?.count(), 0);
        Ok(())
    }

    #[test]
    fn rejects_changed_templates_and_unsafe_identifiers() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut init = fixture.init();
        init.bridge = "deckard0\nlxc.hook.mount=/tmp/evil".into();
        assert!(init.prepare_local(&NodeInventory::default()).is_err());
        init.bridge = "deckard0".into();
        fs::write(init.templates.join("config_base"), b"lxc.arch = LXCARCH\n")?;
        assert!(init.prepare_local(&NodeInventory::default()).is_err());
        assert_eq!(fs::read_dir(init.work.join("lxc"))?.count(), 0);
        Ok(())
    }

    #[test]
    fn cancelled_local_init_never_publishes() -> Result<()> {
        let fixture = Fixture::new()?;
        let cancelled = AtomicBool::new(true);
        let init = fixture.init();
        assert!(
            init.prepare_local_cancellable(&NodeInventory::default(), &cancelled, None)
                .is_err()
        );
        assert_eq!(fs::read_dir(init.work.join("lxc"))?.count(), 0);
        Ok(())
    }

    #[test]
    fn expired_local_init_never_publishes() -> Result<()> {
        let fixture = Fixture::new()?;
        let cancelled = AtomicBool::new(false);
        let init = fixture.init();
        assert!(
            init.prepare_local_cancellable(
                &NodeInventory::default(),
                &cancelled,
                Some(std::time::Instant::now()),
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(init.work.join("lxc"))?.count(), 0);
        Ok(())
    }

    #[test]
    fn lxc_three_does_not_include_v4_snippet() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut init = fixture.init();
        init.lxc_major = 3;
        let output = init.prepare_local(&NodeInventory::default())?;
        assert!(!fs::read_to_string(output.join("config"))?.contains("allow_nesting"));
        Ok(())
    }
}
