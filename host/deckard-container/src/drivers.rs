//! Host device preparation before the container starts (upstream
//! `tools/helpers/drivers.py`, `container_manager.set_permissions` and
//! `tools/helpers/protocol.py`).

use std::fs::{self, File, Permissions};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use deckard_host_tools::{cfg, files};

use crate::Result;
use crate::command::{self, Command};
use crate::config;

/// Binder node names, most preferred last (upstream's order: the last one
/// that exists wins).
pub const BINDER: [&str; 4] = ["anbox-binder", "puddlejumper", "bonder", "binder"];
pub const VNDBINDER: [&str; 4] = [
    "anbox-vndbinder",
    "vndpuddlejumper",
    "vndbonder",
    "vndbinder",
];
pub const HWBINDER: [&str; 4] = ["anbox-hwbinder", "hwpuddlejumper", "hwbonder", "hwbinder"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverNodes {
    pub binder: String,
    pub vndbinder: String,
    pub hwbinder: String,
}

impl DriverNodes {
    /// # Errors
    /// The file is unreadable or a node name is missing or unsafe.
    pub fn from_config(path: &Path) -> Result<Self> {
        Self::parse(&config::read(path)?)
    }

    fn parse(text: &str) -> Result<Self> {
        let setting = |name: &str| {
            let value = cfg::get(text, name).ok_or(format!("missing {name} in waydroid.cfg"))?;
            let plain = value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
            if value.is_empty() || value.len() > 64 || !plain {
                return Err("invalid Binder node name".to_owned());
            }
            Ok(value.to_owned())
        };
        Ok(Self {
            binder: setting("binder")?,
            vndbinder: setting("vndbinder")?,
            hwbinder: setting("hwbinder")?,
        })
    }

    /// The installed nodes, preferring upstream's choice per kind.
    ///
    /// # Errors
    /// A kind has no node under `dev_root`.
    pub fn discover(dev_root: &Path) -> Result<Self> {
        let pick = |candidates: &[&str; 4]| {
            let found = candidates
                .iter()
                .rev()
                .find(|name| dev_root.join(name).exists());
            found
                .map(|name| (*name).to_owned())
                .ok_or(format!("Binder node {} not found", candidates[3]))
        };
        Ok(Self {
            binder: pick(&BINDER)?,
            vndbinder: pick(&VNDBINDER)?,
            hwbinder: pick(&HWBINDER)?,
        })
    }

    #[must_use]
    pub fn paths(&self, dev_root: &Path) -> [PathBuf; 3] {
        [&self.binder, &self.vndbinder, &self.hwbinder].map(|name| dev_root.join(name))
    }

    fn open_devices(&self, dev_root: &Path) -> Result<Vec<File>> {
        let mut devices = Vec::with_capacity(3);
        for path in self.paths(dev_root) {
            let resolved = fs::canonicalize(&path)?;
            if !resolved.starts_with(dev_root) {
                let dev_root = dev_root.display();
                let path = path.display();
                return Err(format!("Binder node is not a device under {dev_root}: {path}").into());
            }
            let device = File::open(&resolved)?;
            if !device.metadata()?.file_type().is_char_device() {
                let path = path.display();
                return Err(format!("Binder node is not a character device: {path}").into());
            }
            devices.push(device);
        }
        Ok(devices)
    }

    /// # Errors
    /// A node escapes `dev_root` or is not a character device.
    pub fn audit(&self, dev_root: &Path) -> Result<()> {
        self.open_devices(dev_root).map(drop)
    }

    /// Makes the nodes usable by Android: mode 0666.
    ///
    /// # Errors
    /// As for [`audit`](Self::audit), or a mode cannot be set.
    pub fn prepare(&self, dev_root: &Path) -> Result<()> {
        for device in self.open_devices(dev_root)? {
            device.set_permissions(Permissions::from_mode(0o666))?;
        }
        Ok(())
    }
}

/// Makes `/dev/binder`, `/dev/vndbinder` and `/dev/hwbinder` exist when none
/// of a kind does: loads `binder_linux` if the kernel lacks binderfs, mounts
/// binderfs on `/dev/binderfs` and links its devices into `/dev`.
///
/// Only the devices binderfs creates when mounted are used; allocating others
/// through `binder-control` is not supported.
///
/// # Errors
/// binderfs is unavailable or lacks a default device.
pub fn ensure_binder_nodes() -> Result<()> {
    let dev = Path::new("/dev");
    if DriverNodes::discover(dev).is_ok() {
        return Ok(());
    }
    let defaults = [BINDER[3], VNDBINDER[3], HWBINDER[3]];
    if !binderfs_supported(&fs::read_to_string("/proc/filesystems")?) {
        let devices = format!("devices={}", defaults.join(","));
        let modprobe = Command::new(["modprobe", "binder_linux", &devices]);
        command::run(modprobe.timeout(Duration::from_secs(10)))?;
    }
    let binderfs = dev.join("binderfs");
    if !binderfs.is_dir() {
        fs::create_dir(&binderfs)?;
    }
    if !mounted_binderfs(&fs::read_to_string("/proc/self/mountinfo")?, &binderfs) {
        let flags = rustix::mount::MountFlags::empty();
        rustix::mount::mount("binder", &binderfs, "binder", flags, None)?;
    }
    link_nodes(&binderfs, dev, &defaults)
}

fn binderfs_supported(filesystems: &str) -> bool {
    filesystems
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some("binder"))
}

fn mounted_binderfs(mountinfo: &str, target: &Path) -> bool {
    mountinfo.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let separator = fields.iter().position(|field| *field == "-");
        let fstype = separator.and_then(|separator| fields.get(separator + 1));
        fields
            .get(4)
            .is_some_and(|point| Path::new(point) == target)
            && fstype == Some(&"binder")
    })
}

fn link_nodes(binderfs: &Path, dev: &Path, names: &[&str]) -> Result<()> {
    for name in names {
        let source = binderfs.join(name);
        if !source.exists() {
            let reason = "binder-control allocation is not supported";
            return Err(format!("binderfs has no {name} device; {reason}").into());
        }
        let link = dev.join(name);
        if fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(&source, &link)?;
        }
    }
    Ok(())
}

/// Opens the host device nodes Android uses to everyone, as upstream does
/// (mode 0777): GPU, display, video, DMA heaps and sync nodes.
///
/// # Errors
/// A present node cannot be listed or changed.
pub fn open_device_access(dev: &Path, debug_sync: &Path) -> Result<()> {
    let fixed = [
        "ashmem", "sw_sync", "Vcodec", "MTK_SMI", "mdp_sync", "mtk_cmdq", "pvr_sync", "ion",
    ];
    let mut nodes: Vec<PathBuf> = fixed.iter().map(|name| dev.join(name)).collect();
    nodes.push(debug_sync.to_owned());
    for (directory, prefix) in [
        (dev.join("dri"), "renderD"),
        (dev.to_owned(), "fb"),
        (dev.to_owned(), "video"),
        (dev.join("dma_heap"), ""),
    ] {
        nodes.extend(matching(&directory, prefix)?);
    }
    let graphics = dev.join("graphics");
    if graphics.is_dir() {
        nodes.extend(matching(&graphics, "")?);
        nodes.push(graphics);
    }
    for node in nodes {
        files::found(fs::set_permissions(&node, Permissions::from_mode(0o777)))?;
    }
    Ok(())
}

fn matching(directory: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
    let Some(entries) = files::found(fs::read_dir(directory))? else {
        return Ok(Vec::new());
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(prefix) {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

/// Binder and service-manager protocols for the image's `ro.build.version.sdk`.
#[must_use]
pub fn protocols(build_prop: &str) -> (&'static str, &'static str) {
    let mut sdk = 0;
    for line in build_prop.lines() {
        if let Some(("ro.build.version.sdk", value)) = line.trim().split_once('=') {
            sdk = value.parse().unwrap_or(0);
            break;
        }
    }
    match sdk {
        0..28 => ("aidl", "aidl"),
        28..30 => ("aidl2", "aidl2"),
        30 => ("aidl3", "aidl3"),
        31..33 => ("aidl4", "aidl3"),
        33..35 => ("aidl3", "aidl3"),
        35 => ("aidl3", "aidl5"),
        _ => ("aidl3", "aidl6"),
    }
}

/// Records the mounted image's Binder protocols in `waydroid.cfg`, as upstream
/// does on every start. Call with the rootfs mounted.
///
/// # Errors
/// The config cannot be read or written.
pub fn record_protocols(config_path: &Path, rootfs: &Path) -> Result<()> {
    let build_prop = fs::read_to_string(rootfs.join("system/build.prop"))?;
    let (binder, manager) = protocols(&build_prop);
    let text = config::read(config_path)?;
    if cfg::get(&text, "binder_protocol") == Some(binder)
        && cfg::get(&text, "service_manager_protocol") == Some(manager)
    {
        return Ok(());
    }
    let updates = [
        ("binder_protocol", binder),
        ("service_manager_protocol", manager),
    ];
    config::write(config_path, &config::set(&text, &updates)?)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_installed_binder_names() -> Result<()> {
        let nodes = DriverNodes::parse(
            "[waydroid]\nbinder = binder\nvndbinder = vndbinder\nhwbinder = hwbinder\n",
        )?;
        assert_eq!(nodes.paths(Path::new("/dev"))[0], Path::new("/dev/binder"));
        Ok(())
    }

    #[test]
    fn rejects_escaping_and_missing_names() {
        assert!(
            DriverNodes::parse(
                "[waydroid]\nbinder = ../null\nvndbinder = vndbinder\nhwbinder = hwbinder"
            )
            .is_err()
        );
        assert!(DriverNodes::parse("[waydroid]\nbinder = binder\nvndbinder = vndbinder").is_err());
    }

    #[test]
    fn discovery_prefers_upstream_order() -> Result<()> {
        let dev = tempfile::tempdir()?;
        for name in [
            "anbox-binder",
            "binder",
            "vndbinder",
            "hwbonder",
            "anbox-hwbinder",
        ] {
            fs::write(dev.path().join(name), "")?;
        }
        assert_eq!(
            DriverNodes::discover(dev.path())?,
            DriverNodes {
                binder: "binder".into(),
                vndbinder: "vndbinder".into(),
                hwbinder: "hwbonder".into(),
            }
        );
        fs::remove_file(dev.path().join("vndbinder"))?;
        assert!(DriverNodes::discover(dev.path()).is_err());
        Ok(())
    }

    #[test]
    fn binderfs_detection_and_links() -> Result<()> {
        assert!(binderfs_supported("nodev\tsysfs\nnodev\tbinder\n"));
        assert!(!binderfs_supported("nodev\tsysfs\n"));
        let mountinfo = "40 23 0:42 / /dev/binderfs rw,relatime shared:21 - binder binder rw\n";
        assert!(mounted_binderfs(mountinfo, Path::new("/dev/binderfs")));
        assert!(!mounted_binderfs(mountinfo, Path::new("/dev/other")));
        let root = tempfile::tempdir()?;
        let binderfs = root.path().join("binderfs");
        fs::create_dir(&binderfs)?;
        for name in ["binder", "vndbinder"] {
            fs::write(binderfs.join(name), "")?;
        }
        assert!(link_nodes(&binderfs, root.path(), &["binder", "hwbinder"]).is_err());
        fs::write(binderfs.join("hwbinder"), "")?;
        link_nodes(&binderfs, root.path(), &["binder", "vndbinder", "hwbinder"])?;
        link_nodes(&binderfs, root.path(), &["binder"])?;
        assert_eq!(
            fs::read_link(root.path().join("binder"))?,
            binderfs.join("binder")
        );
        Ok(())
    }

    #[test]
    fn device_access_opens_android_nodes_only() -> Result<()> {
        let dev = tempfile::tempdir()?;
        for path in ["dri", "dma_heap", "graphics"] {
            fs::create_dir(dev.path().join(path))?;
        }
        let opened = [
            "dri/renderD128",
            "dma_heap/system",
            "graphics/fb0",
            "fb0",
            "video19",
        ];
        let created = [opened.as_slice(), &["dri/card0", "null"]].concat();
        for path in created {
            fs::write(dev.path().join(path), "")?;
            fs::set_permissions(dev.path().join(path), fs::Permissions::from_mode(0o600))?;
        }
        open_device_access(dev.path(), &dev.path().join("missing"))?;
        let mode = |path: &str| -> Result<u32> {
            Ok(fs::metadata(dev.path().join(path))?.permissions().mode() & 0o777)
        };
        for path in opened {
            assert_eq!(mode(path)?, 0o777, "{path}");
        }
        assert_eq!(mode("dri/card0")?, 0o600);
        assert_eq!(mode("null")?, 0o600);
        Ok(())
    }

    #[test]
    fn protocols_follow_android_sdk() {
        assert_eq!(protocols(""), ("aidl", "aidl"));
        assert_eq!(protocols("ro.build.version.sdk=29\n"), ("aidl2", "aidl2"));
        assert_eq!(protocols("ro.build.version.sdk=30"), ("aidl3", "aidl3"));
        assert_eq!(protocols("ro.build.version.sdk=32"), ("aidl4", "aidl3"));
        assert_eq!(
            protocols("# c\nro.build.version.sdk=33\n"),
            ("aidl3", "aidl3")
        );
        assert_eq!(protocols("ro.build.version.sdk=35"), ("aidl3", "aidl5"));
        assert_eq!(protocols("ro.build.version.sdk=36"), ("aidl3", "aidl6"));
    }
}
