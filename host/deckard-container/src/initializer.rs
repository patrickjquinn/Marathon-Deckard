//! Offline, local-image portion of Waydroid's initializer. Nothing here stops
//! a container, downloads images, or modifies an existing LXC instance.

use crate::{Error, Result};
use rustix::fs::{CWD, RenameFlags, renameat_with};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
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

/// The installed, offline Waydroid initialization layout. The caller must
/// establish that LXC is stopped before entering this transaction.
pub struct ProductionInit {
    pub work: PathBuf,
    pub images: PathBuf,
    pub templates: PathBuf,
    pub arch: String,
    pub lxc_major: u32,
    pub binder: String,
    pub vndbinder: String,
    pub hwbinder: String,
    pub ota: Option<OtaConfig>,
}

pub struct OtaConfig {
    pub system_url: String,
    pub vendor_url: String,
    pub system_datetime: u64,
    pub vendor_datetime: u64,
}

impl ProductionInit {
    pub fn initialize_stopped(
        &self,
        inventory: &NodeInventory,
        cancelled: &AtomicBool,
        deadline: Option<std::time::Instant>,
    ) -> Result<()> {
        self.initialize_stopped_transactional(inventory, cancelled, deadline)?
            .commit()
    }

    pub fn initialize_stopped_transactional(
        &self,
        inventory: &NodeInventory,
        cancelled: &AtomicBool,
        deadline: Option<std::time::Instant>,
    ) -> Result<PublishedInit> {
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                return Err(Error::Invalid("initialization cancelled".into()));
            }
            if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
                return Err(Error::Timeout("initialization timed out".into()));
            }
            Ok(())
        };
        check()?;
        for path in [&self.work, &self.images, &self.templates] {
            trusted_dir(path)?;
        }
        for image in ["system.img", "vendor.img"] {
            read_image_metadata(&self.images.join(image))?;
        }
        for template in ["config_base", "config_3", "waydroid.seccomp"] {
            read_optional_trusted(&self.templates.join(template))?
                .ok_or_else(|| Error::Invalid(format!("missing template: {template}")))?;
        }
        if self.lxc_major >= 4 && self.templates.join("config_4").exists() {
            read_optional_trusted(&self.templates.join("config_4"))?;
        }
        if !self.images.starts_with(&self.work)
            && !self.images.starts_with("/etc/waydroid-extra/images")
            && !self.images.starts_with("/usr/share/waydroid-extra/images")
        {
            return Err(Error::Invalid(
                "images are outside installed locations".into(),
            ));
        }
        let lxc_parent = self.work.join("lxc");
        let local = LocalImageInit {
            work: self.work.clone(),
            images: self.images.clone(),
            templates: self.templates.clone(),
            name: "waydroid".into(),
            bridge: "waydroid0".into(),
            arch: self.arch.clone(),
            lxc_major: self.lxc_major,
            binder: self.binder.clone(),
            vndbinder: self.vndbinder.clone(),
            hwbinder: self.hwbinder.clone(),
        };
        let mut rendered = local.render(inventory)?;
        let post_stop = Path::new("/usr/lib/waydroid/data/scripts/waydroid-post-stop.sh");
        if rendered.config.contains("LXCPOSTSTOP") {
            if !post_stop.is_file() {
                return Err(Error::Invalid("missing LXC post-stop hook".into()));
            }
            rendered.config = rendered
                .config
                .replace("LXCPOSTSTOP", &post_stop.to_string_lossy());
        }
        let cfg_path = self.work.join("waydroid.cfg");
        let old_cfg = read_optional_trusted(&cfg_path)?.unwrap_or_default();
        let cfg = render_config(&old_cfg, self)?;
        let prop_path = self.work.join("waydroid_base.prop");
        let old_prop = read_optional_trusted(&prop_path)?;
        let props = render_base_properties(old_prop.as_deref(), &cfg)?;
        check()?;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::Invalid(error.to_string()))?
            .as_nanos();
        let staging = self
            .work
            .join(format!(".deckard-init-{}-{nonce}", std::process::id()));
        let directories = [
            lxc_parent.clone(),
            self.work.join("rootfs"),
            self.work.join("overlay"),
            self.work.join("overlay/vendor"),
            self.work.join("overlay_rw"),
            self.work.join("overlay_rw/system"),
            self.work.join("overlay_rw/vendor"),
        ];
        let mut created = Vec::new();
        let result = (|| -> Result<()> {
            for directory in &directories {
                if fs::symlink_metadata(directory).is_err() {
                    fs::create_dir(directory)?;
                    created.push(directory.clone());
                    fs::set_permissions(directory, fs::Permissions::from_mode(0o755))?;
                }
                trusted_dir(directory)?;
            }
            fs::create_dir(&staging)?;
            Ok(())
        })();
        if let Err(error) = result {
            for directory in created.iter().rev() {
                let _ = fs::remove_dir(directory);
            }
            return Err(error);
        }
        let result = (|| -> Result<Vec<PublishedEntry>> {
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))?;
            let lxc_staging = staging.join("waydroid");
            fs::create_dir(&lxc_staging)?;
            write_new(&lxc_staging.join("config"), rendered.config.as_bytes())?;
            check()?;
            write_new(&lxc_staging.join("config_nodes"), rendered.nodes.as_bytes())?;
            let existing = lxc_parent.join("waydroid");
            let session = existing.join("config_session");
            if fs::symlink_metadata(&existing).is_ok() {
                trusted_dir(&existing)?;
            }
            let session_bytes = read_optional_trusted(&session)?.unwrap_or_default();
            write_new(&lxc_staging.join("config_session"), &session_bytes)?;
            write_new(&lxc_staging.join("waydroid.seccomp"), &rendered.seccomp)?;
            fs::set_permissions(&lxc_staging, fs::Permissions::from_mode(0o755))?;
            write_new(&staging.join("waydroid.cfg"), cfg.as_bytes())?;
            write_new(&staging.join("waydroid_base.prop"), props.as_bytes())?;
            check()?;
            // Files are fully rendered before the first production rename. If a
            // later rename fails, restore every published predecessor.
            let mut outputs = Vec::new();
            if existing.exists() {
                for name in [
                    "config",
                    "config_nodes",
                    "config_session",
                    "waydroid.seccomp",
                ] {
                    outputs.push((lxc_staging.join(name), existing.join(name)));
                }
            } else {
                outputs.push((lxc_staging, existing));
            }
            outputs.push((staging.join("waydroid.cfg"), cfg_path));
            outputs.push((staging.join("waydroid_base.prop"), prop_path));
            publish_with_rollback(&outputs, &staging)
        })();
        match result {
            Ok(published) => Ok(PublishedInit {
                staging,
                published,
                created,
            }),
            Err(error) => {
                let has_backup = fs::read_dir(&staging).is_ok_and(|mut entries| {
                    entries.any(|entry| {
                        entry.ok().is_some_and(|entry| {
                            entry.file_name().to_string_lossy().starts_with("backup-")
                        })
                    })
                });
                if !has_backup {
                    let _ = fs::remove_dir_all(&staging);
                }
                for directory in created.iter().rev() {
                    let _ = fs::remove_dir(directory);
                }
                Err(error)
            }
        }
    }
}

type PublishedEntry = (PathBuf, PathBuf, bool);

pub struct PublishedInit {
    staging: PathBuf,
    published: Vec<PublishedEntry>,
    created: Vec<PathBuf>,
}

impl PublishedInit {
    pub fn commit(self) -> Result<()> {
        fs::remove_dir_all(&self.staging)?;
        Ok(())
    }

    pub fn rollback(self) -> Result<()> {
        restore_outputs(&self.published)?;
        fs::remove_dir_all(&self.staging)?;
        for directory in self.created.iter().rev() {
            let _ = fs::remove_dir(directory);
        }
        Ok(())
    }
}

fn trusted_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::Invalid(format!(
            "untrusted directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn read_image_metadata(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !(metadata.is_file() || metadata.file_type().is_block_device())
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::Invalid(format!(
            "untrusted image: {}",
            path.display()
        )));
    }
    Ok(())
}

fn read_optional_trusted(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.mode() & 0o022 != 0
            {
                return Err(Error::Invalid(format!(
                    "untrusted file: {}",
                    path.display()
                )));
            }
            read_regular(path).map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn publish_with_rollback(
    outputs: &[(PathBuf, PathBuf)],
    staging: &Path,
) -> Result<Vec<PublishedEntry>> {
    let mut published = Vec::new();
    for (index, (source, destination)) in outputs.iter().enumerate() {
        let backup = staging.join(format!("backup-{index}"));
        let had_previous = fs::symlink_metadata(destination).is_ok();
        let step = (|| -> Result<()> {
            if had_previous {
                if source.is_dir() {
                    trusted_dir(destination)?;
                } else if source
                    .extension()
                    .is_some_and(|extension| extension == "img")
                {
                    read_image_metadata(destination)?;
                } else {
                    read_optional_trusted(destination)?;
                }
                fs::rename(destination, &backup)?;
                published.push((destination.clone(), backup.clone(), true));
            }
            fs::rename(source, destination)?;
            if !had_previous {
                published.push((destination.clone(), backup, false));
            }
            File::open(
                destination
                    .parent()
                    .ok_or_else(|| Error::Invalid("missing output parent".into()))?,
            )?
            .sync_all()?;
            Ok(())
        })();
        if let Err(error) = step {
            restore_outputs(&published)?;
            return Err(error);
        }
    }
    Ok(published)
}

fn restore_outputs(published: &[PublishedEntry]) -> Result<()> {
    for (target, old, existed) in published.iter().rev() {
        if let Ok(metadata) = fs::symlink_metadata(target) {
            if metadata.is_dir() {
                fs::remove_dir_all(target)?;
            } else {
                fs::remove_file(target)?;
            }
        }
        if *existed {
            fs::rename(old, target)?;
        }
        File::open(
            target
                .parent()
                .ok_or_else(|| Error::Invalid("missing output parent".into()))?,
        )?
        .sync_all()?;
    }
    Ok(())
}

fn render_config(existing: &[u8], init: &ProductionInit) -> Result<String> {
    let text = std::str::from_utf8(existing)
        .map_err(|_| Error::Invalid("invalid waydroid.cfg encoding".into()))?;
    if text.contains('\0') || text.contains('\r') || text.len() > 1024 * 1024 {
        return Err(Error::Invalid("invalid waydroid.cfg".into()));
    }
    let (system_url, vendor_url, system_datetime, vendor_datetime) = if let Some(ota) = &init.ota {
        (
            ota.system_url.as_str(),
            ota.vendor_url.as_str(),
            ota.system_datetime.to_string(),
            ota.vendor_datetime.to_string(),
        )
    } else {
        ("None", "None", "0".into(), "0".into())
    };
    for url in [system_url, vendor_url] {
        if url.contains(['\n', '\r', '\0']) || url.len() > 4096 {
            return Err(Error::Invalid("invalid OTA URL".into()));
        }
    }
    let updates = [
        ("arch", init.arch.as_str()),
        (
            "images_path",
            init.images
                .to_str()
                .ok_or_else(|| Error::Invalid("invalid images path".into()))?,
        ),
        ("vendor_type", "MAINLINE"),
        ("binder", init.binder.as_str()),
        ("vndbinder", init.vndbinder.as_str()),
        ("hwbinder", init.hwbinder.as_str()),
        ("system_ota", system_url),
        ("vendor_ota", vendor_url),
        ("system_datetime", system_datetime.as_str()),
        ("vendor_datetime", vendor_datetime.as_str()),
    ];
    let mut result = String::new();
    let mut in_waydroid = false;
    let mut found_section = false;
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_waydroid {
                append_missing(&mut result, &updates, &seen);
            }
            in_waydroid = trimmed.eq_ignore_ascii_case("[waydroid]");
            if in_waydroid && found_section {
                return Err(Error::Invalid("duplicate [waydroid] section".into()));
            }
            found_section |= in_waydroid;
            seen.clear();
            result.push_str(line);
            result.push('\n');
            continue;
        }
        if in_waydroid && let Some((key, _)) = trimmed.split_once('=') {
            let key = key.trim().to_ascii_lowercase();
            if let Some((_, value)) = updates.iter().find(|(candidate, _)| *candidate == key) {
                if !seen.insert(key.clone()) {
                    return Err(Error::Invalid(format!("duplicate config key: {key}")));
                }
                result.push_str(&format!("{key} = {value}\n"));
                continue;
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    if in_waydroid {
        append_missing(&mut result, &updates, &seen);
    } else if !found_section {
        result.push_str("[waydroid]\n");
        append_missing(&mut result, &updates, &BTreeSet::new());
    }
    Ok(result)
}

fn append_missing(output: &mut String, updates: &[(&str, &str)], seen: &BTreeSet<String>) {
    for (key, value) in updates {
        if !seen.contains(*key) {
            output.push_str(&format!("{key} = {value}\n"));
        }
    }
}

fn render_base_properties(previous: Option<&[u8]>, config: &str) -> Result<String> {
    let system_ota = config_value(config, "system_ota").unwrap_or("None");
    let vendor_ota = config_value(config, "vendor_ota").unwrap_or("None");
    let local_images = system_ota == "None" && vendor_ota == "None";
    let mut props = if let Some(previous) = previous {
        String::from_utf8(previous.to_vec())
            .map_err(|_| Error::Invalid("invalid base properties encoding".into()))?
    } else {
        let mut baseline = String::from(
            "ro.adb.secure=1\nro.debuggable=0\nwaydroid.updater.disabled=true\nro.vndk.lite=true\n",
        );
        if !Path::new("/dev/ashmem").exists() {
            baseline.push_str("sys.use_memfd=true\n");
        }
        let render_node = fs::read_dir("/dev/dri").ok().and_then(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .find_map(|entry| {
                    let name = entry.file_name();
                    if name.to_string_lossy().starts_with("renderD")
                        && entry.file_type().is_ok_and(|kind| kind.is_char_device())
                    {
                        Some(entry.path())
                    } else {
                        None
                    }
                })
        });
        if let Some(node) = render_node {
            baseline.push_str(&format!(
                "ro.hardware.gralloc=gbm\nro.hardware.egl=mesa\ngralloc.gbm.device={}\n",
                node.display()
            ));
        } else {
            baseline.push_str("ro.hardware.gralloc=default\nro.hardware.egl=swiftshader\n");
        }
        baseline.push_str(
            "ro.opengles.version=196610\ndebug.stagefright.ccodec=0\nro.hardware.camera=v4l2\n",
        );
        baseline
    };
    if props.contains('\0') || props.contains('\r') {
        return Err(Error::Invalid("invalid base properties".into()));
    }
    props = props
        .lines()
        .filter(|line| {
            !line.starts_with("waydroid.system_ota=")
                && !line.starts_with("waydroid.vendor_ota=")
                && !line.starts_with("waydroid.updater.disabled=")
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !props.is_empty() {
        props.push('\n');
    }
    if local_images {
        props.push_str("waydroid.updater.disabled=true\n");
    } else {
        props.push_str(&format!(
            "waydroid.system_ota={system_ota}\nwaydroid.vendor_ota={vendor_ota}\n"
        ));
    }
    let mut in_properties = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_properties = trimmed.eq_ignore_ascii_case("[properties]");
        } else if in_properties && !trimmed.is_empty() && !trimmed.starts_with(['#', ';']) {
            let (key, value) = trimmed
                .split_once('=')
                .ok_or_else(|| Error::Invalid("invalid property override".into()))?;
            let key = key.trim();
            let value = value.trim();
            crate::lxc::validate_property_key(key)?;
            crate::lxc::validate_property_value(value)?;
            props = props
                .lines()
                .filter(|entry| !entry.starts_with(&format!("{key}=")))
                .collect::<Vec<_>>()
                .join("\n");
            if !props.is_empty() {
                props.push('\n');
            }
            props.push_str(&format!("{key}={value}\n"));
        }
    }
    if !props.ends_with('\n') {
        props.push('\n');
    }
    Ok(props)
}

fn config_value<'a>(config: &'a str, key: &str) -> Option<&'a str> {
    let mut in_waydroid = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_waydroid = line.eq_ignore_ascii_case("[waydroid]");
        } else if in_waydroid
            && let Some((candidate, value)) = line.split_once('=')
            && candidate.trim().eq_ignore_ascii_case(key)
        {
            return Some(value.trim());
        }
    }
    None
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

    fn production(fixture: &Fixture) -> Result<ProductionInit> {
        let images = fixture.root.join("work/images");
        fs::create_dir(&images)?;
        for image in ["system.img", "vendor.img"] {
            fs::copy(fixture.root.join("images").join(image), images.join(image))?;
        }
        Ok(ProductionInit {
            work: fixture.root.join("work"),
            images,
            templates: fixture.root.join("templates"),
            arch: "aarch64".into(),
            lxc_major: 4,
            binder: "binder".into(),
            vndbinder: "vndbinder".into(),
            hwbinder: "hwbinder".into(),
            ota: None,
        })
    }

    #[test]
    fn production_init_preserves_existing_session_and_properties() -> Result<()> {
        let fixture = Fixture::new()?;
        let init = production(&fixture)?;
        fs::create_dir(init.work.join("lxc/waydroid"))?;
        fs::write(init.work.join("lxc/waydroid/config"), "old config\n")?;
        fs::write(
            init.work.join("lxc/waydroid/config_session"),
            "session bind\n",
        )?;
        fs::write(init.work.join("lxc/waydroid/custom.conf"), "keep me\n")?;
        fs::write(
            init.work.join("waydroid.cfg"),
            "[waydroid]\nmount_overlays = True\nimages_path = /old\n[properties]\nro.sf.lcd_density = 254\n",
        )?;
        fs::write(
            init.work.join("waydroid_base.prop"),
            "ro.hardware.egl=mesa\n",
        )?;
        init.initialize_stopped(&NodeInventory::default(), &AtomicBool::new(false), None)?;
        let cfg = fs::read_to_string(init.work.join("waydroid.cfg"))?;
        assert!(cfg.contains("mount_overlays = True"));
        assert!(cfg.contains(&format!("images_path = {}", init.images.display())));
        assert!(cfg.contains("ro.sf.lcd_density = 254"));
        assert_eq!(
            fs::read(init.work.join("lxc/waydroid/config_session"))?,
            b"session bind\n"
        );
        assert_eq!(
            fs::read(init.work.join("lxc/waydroid/custom.conf"))?,
            b"keep me\n"
        );
        let props = fs::read_to_string(init.work.join("waydroid_base.prop"))?;
        assert!(props.contains("ro.hardware.egl=mesa\n"));
        assert!(props.contains("ro.sf.lcd_density=254\n"));
        assert!(init.work.join("overlay_rw/system").is_dir());
        Ok(())
    }

    #[test]
    fn production_init_bootstraps_missing_layout() -> Result<()> {
        let fixture = Fixture::new()?;
        let init = production(&fixture)?;
        fs::remove_dir(init.work.join("lxc"))?;
        init.initialize_stopped(&NodeInventory::default(), &AtomicBool::new(false), None)?;
        assert!(init.work.join("lxc/waydroid/config").is_file());
        assert!(init.work.join("rootfs").is_dir());
        assert!(init.work.join("overlay/vendor").is_dir());
        assert!(init.work.join("overlay_rw/system").is_dir());
        assert!(init.work.join("waydroid_base.prop").is_file());
        assert!(fs::read_to_string(init.work.join("waydroid.cfg"))?.contains("system_ota = None"));
        Ok(())
    }

    #[test]
    fn transactional_init_can_restore_existing_files() -> Result<()> {
        let fixture = Fixture::new()?;
        let init = production(&fixture)?;
        fs::write(init.work.join("waydroid.cfg"), "[waydroid]\narch = old\n")?;
        let transaction = init.initialize_stopped_transactional(
            &NodeInventory::default(),
            &AtomicBool::new(false),
            None,
        )?;
        assert!(init.work.join("lxc/waydroid/config").exists());
        transaction.rollback()?;
        assert_eq!(
            fs::read_to_string(init.work.join("waydroid.cfg"))?,
            "[waydroid]\narch = old\n"
        );
        assert!(!init.work.join("lxc/waydroid").exists());
        Ok(())
    }

    #[test]
    fn invalid_image_or_cancel_never_changes_production() -> Result<()> {
        let fixture = Fixture::new()?;
        let init = production(&fixture)?;
        fs::write(init.work.join("waydroid.cfg"), "[waydroid]\narch = old\n")?;
        let cancelled = AtomicBool::new(true);
        assert!(
            init.initialize_stopped(&NodeInventory::default(), &cancelled, None)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(init.work.join("waydroid.cfg"))?,
            "[waydroid]\narch = old\n"
        );
        fs::write(init.images.join("system.img"), "invalid")?;
        assert!(
            init.initialize_stopped(&NodeInventory::default(), &AtomicBool::new(false), None)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(init.work.join("waydroid.cfg"))?,
            "[waydroid]\narch = old\n"
        );
        Ok(())
    }

    #[test]
    fn renderer_rejects_duplicate_config_and_property_injection() -> Result<()> {
        let fixture = Fixture::new()?;
        let init = production(&fixture)?;
        assert!(render_config(b"[waydroid]\narch=aarch64\narch=x86_64\n", &init).is_err());
        assert!(
            render_base_properties(None, "[properties]\nro.adb.secure = 0\nother = bad\nline\n")
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn stale_ota_properties_are_removed_for_local_images() -> Result<()> {
        let result = render_base_properties(
            Some(b"waydroid.system_ota=https://example.invalid\nwaydroid.vendor_ota=https://example.invalid\nro.hardware.egl=mesa\n"),
            "[waydroid]\n",
        )?;
        assert!(!result.contains("waydroid.system_ota="));
        assert!(!result.contains("waydroid.vendor_ota="));
        assert!(result.contains("waydroid.updater.disabled=true\n"));
        assert!(result.contains("ro.hardware.egl=mesa\n"));
        Ok(())
    }

    #[test]
    fn publication_failure_restores_predecessor() -> Result<()> {
        let fixture = Fixture::new()?;
        let staging = fixture.root.join("work/staging");
        fs::create_dir(&staging)?;
        fs::write(staging.join("new-first"), "new")?;
        fs::write(fixture.root.join("work/first"), "old")?;
        let outputs = [
            (staging.join("new-first"), fixture.root.join("work/first")),
            (staging.join("missing"), fixture.root.join("work/second")),
        ];
        assert!(publish_with_rollback(&outputs, &staging).is_err());
        assert_eq!(fs::read(fixture.root.join("work/first"))?, b"old");
        assert!(!fixture.root.join("work/second").exists());
        Ok(())
    }
}
