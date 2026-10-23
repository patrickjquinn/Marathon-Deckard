//! `waydroid init` from images already on the device (upstream
//! `tools/actions/initializer.py`, `tools/helpers/lxc.py`,
//! `tools/helpers/images.py`). Renders the LXC instance, `waydroid.cfg` and
//! `waydroid_base.prop` from the templates in `lxc/`, which Deckard ships.
//! Images are never downloaded.

use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use deckard_host_tools::cfg;

use crate::command::{Command, Runner};
use crate::drivers::DriverNodes;
use crate::{Result, config, ensure, lxc, mounts};

const CONFIG_BASE: &str = include_str!("../lxc/config_base");
const CONFIG_3: &str = include_str!("../lxc/config_3");
const CONFIG_4: &str = include_str!("../lxc/config_4");
const SECCOMP: &str = include_str!("../lxc/waydroid.seccomp");

/// Image directories that take precedence over `images_path`.
pub const PREINSTALLED: [&str; 2] = [
    "/etc/waydroid-extra/images",
    "/usr/share/waydroid-extra/images",
];
/// The Waydroid tools version the image is told it runs under.
const TOOLS_VERSION: &str = "1.6.3";
const PRODUCTS: [&str; 5] = ["brand", "device", "manufacturer", "model", "name"];
const FILE: &str = "bind,create=file,optional";
const DIR: &str = "bind,create=dir,optional";

/// One LXC instance: its state directory, container name and bridge.
pub struct Instance {
    pub work: PathBuf,
    pub name: String,
    pub bridge: String,
}

impl Instance {
    #[must_use]
    pub fn production() -> Self {
        Self {
            work: PathBuf::from("/var/lib/waydroid"),
            name: "waydroid".into(),
            bridge: "waydroid0".into(),
        }
    }

    fn lxc(&self) -> PathBuf {
        self.work.join("lxc").join(&self.name)
    }
}

/// Where `system.img` and `vendor.img` are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Images {
    /// A preinstalled directory: OTA settings are cleared, the updater off.
    Preinstalled(PathBuf),
    /// The instance's `images` directory: configured OTA settings are kept.
    Installed(PathBuf),
}

impl Images {
    /// The first preinstalled directory holding both images, else `installed`.
    ///
    /// # Errors
    /// No directory holds both images.
    pub fn find(preinstalled: &[PathBuf], installed: &Path) -> Result<Self> {
        let has_images = |dir: &Path| {
            ["system.img", "vendor.img"].iter().all(|name| {
                fs::symlink_metadata(dir.join(name)).is_ok_and(|metadata| {
                    metadata.is_file() || metadata.file_type().is_block_device()
                })
            })
        };
        if let Some(dir) = preinstalled.iter().find(|dir| has_images(dir)) {
            return Ok(Self::Preinstalled(dir.clone()));
        }
        ensure(
            has_images(installed),
            "Android images are not installed; Deckard does not download them",
        )?;
        Ok(Self::Installed(installed.to_owned()))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Preinstalled(path) | Self::Installed(path) => path,
        }
    }
}

const FIXED_NODES: [&str; 25] = [
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
    "/sys/module/lowmemorykiller",
    "/dev/sw_sync",
    "/sys/kernel/debug",
    "/sys/class/leds/vibrator",
    "/sys/devices/virtual/timed_output/vibrator",
    "/dev/Vcodec",
    "/dev/MTK_SMI",
    "/dev/mdp_sync",
    "/dev/mtk_cmdq",
    "/mnt/wslg",
];
const NFC: &str = "/system/etc/libnfc-nci.conf";

/// Device nodes and paths present on the host.
#[derive(Default)]
pub struct NodeInventory {
    paths: BTreeSet<String>,
}

fn validate_token(token: &str) -> Result<()> {
    let unsafe_byte = token
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte == 0 || byte == b'\\');
    ensure(!token.is_empty() && !unsafe_byte, "unsafe LXC token")
}

impl NodeInventory {
    /// # Errors
    /// A device directory cannot be listed or holds an unusable name.
    pub fn from_host() -> Result<Self> {
        let mut paths = BTreeSet::new();
        let known = FIXED_NODES.iter().chain(&[NFC]);
        for path in known {
            if Path::new(path).exists() {
                paths.insert((*path).to_owned());
            }
        }
        for (dir, prefix) in [
            ("/dev", "fb"),
            ("/dev/graphics", "fb"),
            ("/dev", "video"),
            ("/dev/dma_heap", ""),
        ] {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with(prefix)
                    && entry.file_type()?.is_char_device()
                {
                    let path = entry.path().to_string_lossy().into_owned();
                    validate_token(&path)?;
                    paths.insert(path);
                }
            }
        }
        Ok(Self { paths })
    }

    /// # Errors
    /// A path is relative or unusable in an LXC mount entry.
    pub fn from_paths(paths: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut inventory = Self::default();
        for path in paths {
            validate_token(&path)?;
            ensure(path.starts_with('/'), "device path must be absolute")?;
            inventory.paths.insert(path);
        }
        Ok(inventory)
    }
}

/// The host facts the rendered files depend on.
pub struct Host {
    /// Waydroid's name for the architecture (`waydroid.cfg` `arch`).
    pub arch: &'static str,
    /// The kernel's name (`lxc.arch`).
    pub lxc_arch: &'static str,
    pub lxc_major: u32,
    pub nodes: DriverNodes,
    pub devices: NodeInventory,
    /// The first DRM render node not driven by an unsupported driver.
    pub render_node: Option<String>,
    pub vulkan: Option<&'static str>,
    /// `ro.product.waydroid.*` values from the device tree.
    pub products: Vec<(&'static str, String)>,
    pub ashmem: bool,
}

impl Host {
    /// Call after the Binder nodes are set up.
    ///
    /// # Errors
    /// The host is a Halium (Android-vendor) device, its architecture or LXC
    /// is unsupported, or a Binder node is missing.
    pub fn probe(runner: &mut impl Runner) -> Result<Self> {
        let halium = env::var_os("PATH")
            .is_some_and(|path| env::split_paths(&path).any(|dir| dir.join("getprop").is_file()));
        ensure(!halium, "Halium hosts (with getprop) are not supported")?;
        let (arch, lxc_arch) = match env::consts::ARCH {
            "aarch64" => ("arm64", "aarch64"),
            "x86_64" => ("x86_64", "x86_64"),
            other => return Err(format!("unsupported architecture: {other}").into()),
        };
        let version =
            runner.run(Command::line("lxc-info --version").timeout(Duration::from_secs(3)))?;
        let major = version
            .trim()
            .split('.')
            .next()
            .and_then(|major| major.parse().ok());
        let lxc_major: u32 = major.ok_or("invalid LXC version")?;
        ensure(lxc_major >= 3, "LXC 3 or newer is required")?;
        let drm = Path::new("/sys/class/drm");
        let render_node = render_node(Path::new("/dev/dri"), drm);
        let driver = render_node
            .as_deref()
            .and_then(|node| kernel_driver(drm, node));
        Ok(Self {
            arch,
            lxc_arch,
            lxc_major,
            nodes: DriverNodes::discover(Path::new("/dev"))?,
            devices: NodeInventory::from_host()?,
            render_node,
            vulkan: driver.as_deref().and_then(vulkan_driver),
            products: device_tree(Path::new("/proc/device-tree")),
            ashmem: Path::new("/dev/ashmem").exists(),
        })
    }
}

fn kernel_driver(drm: &Path, node: &str) -> Option<String> {
    let name = Path::new(node).file_name()?;
    let uevent = fs::read_to_string(drm.join(name).join("device/uevent")).ok()?;
    let driver = uevent.lines().find_map(|line| line.strip_prefix("DRIVER="));
    driver.map(str::to_owned)
}

fn render_node(dri: &Path, drm: &Path) -> Option<String> {
    let mut nodes = Vec::new();
    let entries = fs::read_dir(dri).ok()?;
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with("renderD") {
            nodes.push(entry.path().to_string_lossy().into_owned());
        }
    }
    nodes.sort();
    nodes
        .into_iter()
        .find(|node| kernel_driver(drm, node).as_deref() != Some("nvidia"))
}

fn vulkan_driver(kernel_driver: &str) -> Option<&'static str> {
    match kernel_driver {
        "i915" | "xe" => Some("intel"),
        "amdgpu" => Some("radeon"),
        "panfrost" => Some("panfrost"),
        "msm" | "msm_dpu" => Some("freedreno"),
        "vc4" => Some("broadcom"),
        "nouveau" => Some("nouveau"),
        _ => None,
    }
}

fn device_tree(root: &Path) -> Vec<(&'static str, String)> {
    let mut products = Vec::new();
    for product in PRODUCTS {
        let Ok(value) = fs::read_to_string(root.join(product)) else {
            continue;
        };
        let value = value.trim().trim_end_matches('\0').trim();
        if !value.is_empty() && lxc::validate_property_value(value).is_ok() {
            products.push((product, value.to_owned()));
        }
    }
    products
}

/// `lxc.mount.entry` lines in upstream's `generate_nodes_lxc_config` order.
struct Entries<'a> {
    text: String,
    devices: &'a NodeInventory,
}

impl Entries<'_> {
    fn add(&mut self, source: &str, target: &str, kind: &str, options: &str) {
        let _ = writeln!(
            self.text,
            "lxc.mount.entry = {source} {target} {kind} {options} 0 0"
        );
    }

    /// `path` when the host has it, mounted at the same place.
    fn host(&mut self, paths: &[&str], options: &str) {
        for path in paths {
            self.host_at(path, &path[1..], options);
        }
    }

    fn host_at(&mut self, path: &str, target: &str, options: &str) {
        if self.devices.paths.contains(path) {
            self.add(path, target, "none", options);
        }
    }
}

/// One initialization of `instance` from `images` on `host`.
pub struct Init<'a> {
    pub instance: &'a Instance,
    pub images: Images,
    pub host: Host,
}

impl Init<'_> {
    fn render_lxc(&self) -> String {
        let mut config = format!("{CONFIG_BASE}{CONFIG_3}");
        if self.host.lxc_major >= 4 {
            config.push_str(CONFIG_4);
        }
        config
            .replace("@LXC@", &self.instance.lxc().to_string_lossy())
            .replace("@WORK@", &self.instance.work.to_string_lossy())
            .replace("@ARCH@", self.host.lxc_arch)
            .replace("@NAME@", &self.instance.name)
            .replace("@BRIDGE@", &self.instance.bridge)
    }

    fn render_nodes(&self) -> String {
        let devices = &self.host.devices;
        let mut e = Entries {
            text: String::new(),
            devices,
        };
        e.add("tmpfs", "dev", "tmpfs", "nosuid");
        e.host(&FIXED_NODES[..7], FILE);
        e.host(&["/dev/char"], DIR);
        e.host(&FIXED_NODES[8..13], FILE);
        if let Some(render) = &self.host.render_node {
            e.add(render, &render[1..], "none", FILE);
        }
        for prefix in [
            "/dev/fb",
            "/dev/graphics/fb",
            "/dev/video",
            "/dev/dma_heap/",
        ] {
            let matches = devices.paths.iter().filter(|path| path.starts_with(prefix));
            for path in matches {
                e.add(path, &path[1..], "none", FILE);
            }
        }
        let nodes = &self.host.nodes;
        for (name, target) in [
            (&nodes.binder, "binder"),
            (&nodes.vndbinder, "vndbinder"),
            (&nodes.hwbinder, "hwbinder"),
        ] {
            e.add(
                &format!("/dev/{name}"),
                &format!("dev/{target}"),
                "none",
                FILE,
            );
        }
        e.add(
            "none",
            "dev/pts",
            "devpts",
            "defaults,mode=644,ptmxmode=666,create=dir",
        );
        e.host(&["/dev/uhid"], FILE);
        e.host_at("/dev/net/tun", "dev/tun", FILE);
        e.host(&["/sys/module/lowmemorykiller"], DIR);
        let host_permissions = self.instance.work.join("host-permissions");
        e.add(
            &host_permissions.to_string_lossy(),
            "vendor/etc/host-permissions",
            "none",
            "bind,optional",
        );
        e.host(&["/dev/sw_sync"], FILE);
        e.host(&["/sys/kernel/debug"], "rbind,create=dir,optional");
        e.host(&FIXED_NODES[18..20], DIR);
        e.host(&FIXED_NODES[20..24], FILE);
        e.add("tmpfs", "mnt_extra", "tmpfs", "nodev");
        e.host_at("/mnt/wslg", "mnt_extra/wslg", "rbind,create=dir,optional");
        for path in ["tmp", "var", "run"] {
            e.add("tmpfs", path, "tmpfs", "nodev");
        }
        e.host(&[NFC], "bind,optional");
        e.text
    }

    fn render_config(&self, existing: &str) -> Result<String> {
        let images = self
            .images
            .path()
            .to_str()
            .ok_or("images path is not UTF-8")?;
        let nodes = &self.host.nodes;
        let mut updates = vec![
            ("arch", self.host.arch),
            ("vendor_type", "MAINLINE"),
            ("binder", nodes.binder.as_str()),
            ("vndbinder", nodes.vndbinder.as_str()),
            ("hwbinder", nodes.hwbinder.as_str()),
            ("images_path", images),
        ];
        if matches!(self.images, Images::Preinstalled(_)) {
            let cleared = [
                ("system_ota", "None"),
                ("vendor_ota", "None"),
                ("system_datetime", "0"),
                ("vendor_datetime", "0"),
            ];
            updates.extend(cleared);
        }
        for (key, value) in config::DEFAULTS {
            if cfg::get(existing, key).is_none() {
                updates.push((key, value));
            }
        }
        config::set(existing, &updates)
    }

    /// Upstream's `make_base_props` for a mainline host.
    fn render_base_properties(&self, config_text: &str) -> Result<String> {
        let host = &self.host;
        let mut props: Vec<(String, String)> = Vec::new();
        let mut push = |key: &str, value: &str| props.push((key.to_owned(), value.to_owned()));
        if !host.ashmem {
            push("sys.use_memfd", "true");
        }
        push("ro.adb.secure", "1");
        push("ro.debuggable", "0");
        let (gralloc, egl) = match &host.render_node {
            Some(node) => {
                push("gralloc.gbm.device", node);
                ("gbm", "mesa")
            }
            None => ("default", "swiftshader"),
        };
        push("debug.stagefright.ccodec", "0");
        push("ro.hardware.gralloc", gralloc);
        push("ro.hardware.egl", egl);
        if let Some(vulkan) = host.vulkan {
            push("ro.hardware.vulkan", vulkan);
        }
        push("ro.hardware.camera", "v4l2");
        push("ro.opengles.version", "196610");
        match self.images {
            Images::Preinstalled(_) => push("waydroid.updater.disabled", "true"),
            Images::Installed(_) => {
                for key in ["system_ota", "vendor_ota"] {
                    let value = cfg::get(config_text, key).unwrap_or("None");
                    lxc::validate_property_value(value)?;
                    push(&format!("waydroid.{key}"), value);
                }
            }
        }
        push("waydroid.tools_version", TOOLS_VERSION);
        push("ro.vndk.lite", "true");
        for (product, value) in &host.products {
            push(&format!("ro.product.waydroid.{product}"), value);
        }
        for (key, value) in property_overrides(config_text)? {
            props.retain(|(existing, _)| *existing != key);
            props.push((key, value));
        }
        let mut text = String::new();
        for (key, value) in props {
            let _ = writeln!(text, "{key}={value}");
        }
        Ok(text)
    }

    /// Writes the instance's files into `staging` and lists each staged
    /// output with the path it replaces.
    fn stage(
        &self,
        staging: &Path,
        rendered: [&str; 4],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Vec<(PathBuf, PathBuf)>> {
        let [lxc_config, nodes, config_text, properties] = rendered;
        let work = &self.instance.work;
        let mode = |path: &Path, mode| fs::set_permissions(path, Permissions::from_mode(mode));
        fs::create_dir(staging)?;
        mode(staging, 0o700)?;
        let lxc_staging = staging.join("lxc");
        fs::create_dir(&lxc_staging)?;
        let existing = self.instance.lxc();
        if fs::symlink_metadata(&existing).is_ok() {
            trusted(&existing, Kind::Directory)?;
        }
        let session = config::read(&existing.join("config_session"))?;
        let lxc_files = [
            ("config", lxc_config),
            ("config_nodes", nodes),
            ("config_session", &session),
            ("waydroid.seccomp", SECCOMP),
        ];
        for (name, text) in lxc_files {
            write_new(&lxc_staging.join(name), text.as_bytes())?;
        }
        mode(&lxc_staging, 0o755)?;
        write_new(&staging.join("waydroid.cfg"), config_text.as_bytes())?;
        write_new(&staging.join("waydroid_base.prop"), properties.as_bytes())?;
        for path in [
            "overlay_rw",
            "overlay_rw/system",
            "overlay_rw/vendor",
            "overlay_work",
        ] {
            fs::create_dir(staging.join(path))?;
            mode(&staging.join(path), 0o755)?;
        }
        check()?;
        let mut outputs = Vec::new();
        if existing.exists() {
            for (name, _) in lxc_files {
                outputs.push((lxc_staging.join(name), existing.join(name)));
            }
        } else {
            outputs.push((lxc_staging, existing));
        }
        // Upstream drops the writable layers on every init.
        for name in [
            "waydroid.cfg",
            "waydroid_base.prop",
            "overlay_rw",
            "overlay_work",
        ] {
            outputs.push((staging.join(name), work.join(name)));
        }
        Ok(outputs)
    }

    /// Publishes the instance. Cancellation and the deadline are checked
    /// before each step up to publication; nothing is replaced on error.
    ///
    /// # Errors
    /// The work directory is untrusted, the images are invalid, the rootfs is
    /// still mounted, a step fails, or the job was cancelled or timed out.
    pub fn run(&self, cancelled: &AtomicBool, deadline: Option<Instant>) -> Result<Published> {
        let check = || {
            ensure(
                !cancelled.load(Ordering::Acquire),
                "initialization cancelled",
            )?;
            ensure(
                !deadline.is_some_and(|end| Instant::now() >= end),
                "initialization timed out",
            )
        };
        check()?;
        let work = &self.instance.work;
        trusted(work, Kind::Directory)?;
        for image in ["system.img", "vendor.img"] {
            validate_image(&self.images.path().join(image))?;
        }
        let rootfs = work.join("rootfs");
        let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
        let mounted = !mounts::mountpoints_beneath(&rootfs, &mountinfo).is_empty();
        ensure(
            !mounted,
            "the Android rootfs is still mounted; stop the container first",
        )?;
        let config_text = self.render_config(&config::read(&work.join("waydroid.cfg"))?)?;
        let properties = self.render_base_properties(&config_text)?;
        let rendered = [
            self.render_lxc(),
            self.render_nodes(),
            config_text,
            properties,
        ];
        check()?;

        let mut created = Vec::new();
        let directories = [
            work.join("lxc"),
            rootfs,
            work.join("overlay"),
            work.join("overlay/vendor"),
            work.join("host-permissions"),
        ];
        for directory in directories {
            let mut prepare = || -> Result<()> {
                if fs::symlink_metadata(&directory).is_err() {
                    fs::create_dir(&directory)?;
                    created.push(directory.clone());
                    fs::set_permissions(&directory, Permissions::from_mode(0o755))?;
                }
                trusted(&directory, Kind::Directory)
            };
            if let Err(error) = prepare() {
                remove_created(&created);
                return Err(error);
            }
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos());
        let staging = work.join(format!(".deckard-init-{}-{nonce}", std::process::id()));
        let rendered = rendered.each_ref().map(String::as_str);
        let published = self
            .stage(&staging, rendered, &check)
            .and_then(|outputs| publish_with_rollback(&outputs, &staging));
        match published {
            Ok(_) => Ok(Published { staging }),
            Err(error) => {
                if !has_backup(&staging) {
                    let _ = fs::remove_dir_all(&staging);
                }
                remove_created(&created);
                Err(error)
            }
        }
    }
}

fn property_overrides(config_text: &str) -> Result<Vec<(String, String)>> {
    let mut overrides = Vec::new();
    let mut in_properties = false;
    for line in config_text.lines() {
        let trimmed = line.trim();
        if let Some(name) = cfg::section(trimmed) {
            in_properties = name.eq_ignore_ascii_case("properties");
        } else if in_properties && !trimmed.is_empty() && !trimmed.starts_with(['#', ';']) {
            let (key, value) = trimmed.split_once('=').ok_or("invalid property override")?;
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            lxc::validate_property_key(&key)?;
            lxc::validate_property_value(value)?;
            overrides.push((key, value.to_owned()));
        }
    }
    Ok(overrides)
}

fn has_backup(staging: &Path) -> bool {
    let Ok(entries) = fs::read_dir(staging) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with("backup-"))
}

fn remove_created(created: &[PathBuf]) {
    for directory in created.iter().rev() {
        let _ = fs::remove_dir(directory);
    }
}

/// A published initialization; [`commit`](Self::commit) drops what it replaced.
#[must_use]
pub struct Published {
    staging: PathBuf,
}

impl Published {
    /// # Errors
    /// The replaced files cannot be removed.
    pub fn commit(self) -> Result<()> {
        Ok(fs::remove_dir_all(&self.staging)?)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Directory,
    /// A regular file or a block device.
    File,
}

/// A daemon-owned `kind` that only the daemon can write.
fn trusted(path: &Path, kind: Kind) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let right_kind = match kind {
        Kind::Directory => metadata.is_dir(),
        Kind::File => metadata.is_file() || metadata.file_type().is_block_device(),
    };
    let owned =
        metadata.uid() == rustix::process::geteuid().as_raw() && metadata.mode() & 0o022 == 0;
    let noun = if kind == Kind::Directory {
        "directory"
    } else {
        "file"
    };
    ensure(
        right_kind && owned,
        format!("untrusted {noun}: {}", path.display()),
    )
}

/// A daemon-owned regular file or block device holding an ext filesystem.
fn validate_image(path: &Path) -> Result<()> {
    trusted(path, Kind::File)?;
    let mut image = File::open(path)?;
    image.seek(SeekFrom::Start(1024 + 56))?;
    let mut magic = [0; 2];
    image.read_exact(&mut magic)?;
    ensure(
        magic == [0x53, 0xef],
        format!("not an ext image: {}", path.display()),
    )
}

fn sync_parent(path: &Path) -> Result<()> {
    Ok(File::open(path.parent().ok_or("missing output parent")?)?.sync_all()?)
}

/// (target, backup, whether the target existed before).
type PublishedEntry = (PathBuf, PathBuf, bool);

fn publish_with_rollback(
    outputs: &[(PathBuf, PathBuf)],
    staging: &Path,
) -> Result<Vec<PublishedEntry>> {
    let mut published = Vec::new();
    for (index, (source, destination)) in outputs.iter().enumerate() {
        let backup = staging.join(format!("backup-{index}"));
        let had_previous = fs::symlink_metadata(destination).is_ok();
        let mut step = || -> Result<()> {
            if had_previous {
                trusted(
                    destination,
                    if source.is_dir() {
                        Kind::Directory
                    } else {
                        Kind::File
                    },
                )?;
                fs::rename(destination, &backup)?;
                published.push((destination.clone(), backup.clone(), true));
            }
            fs::rename(source, destination)?;
            if !had_previous {
                published.push((destination.clone(), backup.clone(), false));
            }
            sync_parent(destination)
        };
        if let Err(error) = step() {
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
        sync_parent(target)?;
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

    struct Fixture {
        root: tempfile::TempDir,
        instance: Instance,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let root = tempfile::tempdir()?;
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755))?;
            let work = root.path().join("work");
            fs::create_dir_all(work.join("images"))?;
            for image in ["system.img", "vendor.img"] {
                let mut bytes = vec![0; 1082];
                bytes[1080..1082].copy_from_slice(&[0x53, 0xef]);
                fs::write(work.join("images").join(image), bytes)?;
            }
            Ok(Self {
                instance: Instance {
                    work,
                    name: "waydroid".into(),
                    bridge: "waydroid0".into(),
                },
                root,
            })
        }

        fn init(&self, images: Images) -> Result<Init<'_>> {
            let devices = [
                "/dev/zero",
                "/dev/fuse",
                "/dev/char",
                "/dev/fb0",
                "/dev/net/tun",
            ];
            Ok(Init {
                instance: &self.instance,
                images,
                host: host(&devices)?,
            })
        }

        fn installed(&self) -> Images {
            Images::Installed(self.instance.work.join("images"))
        }

        fn run(&self, images: Images, cancelled: bool) -> Result<Published> {
            self.init(images)?.run(&AtomicBool::new(cancelled), None)
        }

        fn read(&self, path: &str) -> Result<String> {
            Ok(fs::read_to_string(self.instance.work.join(path))?)
        }

        fn write(&self, path: &str, text: &str) -> Result<()> {
            Ok(fs::write(self.instance.work.join(path), text)?)
        }
    }

    fn host(devices: &[&str]) -> Result<Host> {
        Ok(Host {
            arch: "arm64",
            lxc_arch: "aarch64",
            lxc_major: 7,
            nodes: DriverNodes {
                binder: "binder".into(),
                vndbinder: "vndbinder".into(),
                hwbinder: "hwbinder".into(),
            },
            devices: NodeInventory::from_paths(devices.iter().map(|path| (*path).to_owned()))?,
            render_node: Some("/dev/dri/renderD128".into()),
            vulkan: None,
            products: vec![("model", "Raspberry Pi Compute Module 5 Lite Rev 1.0".into())],
            ashmem: false,
        })
    }

    #[test]
    fn renders_the_installed_pi_instance() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write(
            "waydroid.cfg",
            "[waydroid]\narch = arm64\n\
             system_ota = https://ota.waydro.id/system/lineage/waydroid_arm64/VANILLA.json\n\
             vendor_ota = https://ota.waydro.id/vendor/waydroid_arm64/MAINLINE.json\n\
             binder_protocol = aidl3\n\n\
             [properties]\nro.sf.lcd_density = 254\nro.product.cpu.abilist32 = \n",
        )?;
        fixture.run(fixture.installed(), false)?.commit()?;
        let work = fixture.instance.work.display();
        let config = fixture.read("lxc/waydroid/config")?;
        for line in [
            format!("lxc.rootfs.path = {work}/rootfs\nlxc.arch = aarch64\n"),
            format!("lxc.include = {work}/lxc/waydroid/config_session\n"),
            format!("lxc.seccomp.profile = {work}/lxc/waydroid/waydroid.seccomp\n"),
            "lxc.mount.auto = cgroup:rw sys:ro proc\n".into(),
            "lxc.hook.mount = /usr/libexec/deckard-shared-folders\n".into(),
            "lxc.uts.name = waydroid\n".into(),
            "lxc.net.0.link = waydroid0\n".into(),
        ] {
            assert!(config.contains(&line), "{line:?} missing");
        }
        assert!(config.ends_with("lxc.seccomp.allow_nesting = 1\n\n"));
        assert!(!config.contains('@'));
        let mut expected = String::new();
        let file = "none bind,create=file,optional";
        for line in [
            "tmpfs dev tmpfs nosuid".to_owned(),
            format!("/dev/zero dev/zero {file}"),
            format!("/dev/fuse dev/fuse {file}"),
            "/dev/char dev/char none bind,create=dir,optional".to_owned(),
            format!("/dev/dri/renderD128 dev/dri/renderD128 {file}"),
            format!("/dev/fb0 dev/fb0 {file}"),
            format!("/dev/binder dev/binder {file}"),
            format!("/dev/vndbinder dev/vndbinder {file}"),
            format!("/dev/hwbinder dev/hwbinder {file}"),
            "none dev/pts devpts defaults,mode=644,ptmxmode=666,create=dir".to_owned(),
            format!("/dev/net/tun dev/tun {file}"),
            format!("{work}/host-permissions vendor/etc/host-permissions none bind,optional"),
            "tmpfs mnt_extra tmpfs nodev".to_owned(),
            "tmpfs tmp tmpfs nodev".to_owned(),
            "tmpfs var tmpfs nodev".to_owned(),
            "tmpfs run tmpfs nodev".to_owned(),
        ] {
            let _ = writeln!(expected, "lxc.mount.entry = {line} 0 0");
        }
        assert_eq!(fixture.read("lxc/waydroid/config_nodes")?, expected);
        assert_eq!(fixture.read("lxc/waydroid/config_session")?, "");
        assert_eq!(fixture.read("lxc/waydroid/waydroid.seccomp")?, SECCOMP);
        assert_eq!(
            fixture.read("waydroid_base.prop")?,
            "sys.use_memfd=true\nro.adb.secure=1\nro.debuggable=0\n\
             gralloc.gbm.device=/dev/dri/renderD128\ndebug.stagefright.ccodec=0\n\
             ro.hardware.gralloc=gbm\nro.hardware.egl=mesa\nro.hardware.camera=v4l2\n\
             ro.opengles.version=196610\n\
             waydroid.system_ota=https://ota.waydro.id/system/lineage/waydroid_arm64/VANILLA.json\n\
             waydroid.vendor_ota=https://ota.waydro.id/vendor/waydroid_arm64/MAINLINE.json\n\
             waydroid.tools_version=1.6.3\nro.vndk.lite=true\n\
             ro.product.waydroid.model=Raspberry Pi Compute Module 5 Lite Rev 1.0\n\
             ro.sf.lcd_density=254\nro.product.cpu.abilist32=\n"
        );
        let cfg = fixture.read("waydroid.cfg")?;
        for line in [
            "arch = arm64\n",
            "vendor_type = MAINLINE\n",
            "binder = binder\n",
            "binder_protocol = aidl3\n",
            "suspend_action = freeze\n",
            "mount_overlays = True\n",
            "auto_adb = False\n",
            "system_ota = https://ota.waydro.id/system/",
        ] {
            assert!(cfg.contains(line), "{line:?} missing from {cfg}");
        }
        for dir in [
            "rootfs",
            "overlay/vendor",
            "overlay_rw/system",
            "overlay_work",
            "host-permissions",
        ] {
            assert!(fixture.instance.work.join(dir).is_dir(), "{dir}");
        }
        assert_eq!(fs::read_dir(&fixture.instance.work)?.count(), 9);
        Ok(())
    }

    #[test]
    fn preinstalled_images_clear_ota_and_keep_session_and_custom_files() -> Result<()> {
        let fixture = Fixture::new()?;
        let lxc = fixture.instance.work.join("lxc/waydroid");
        fs::create_dir_all(&lxc)?;
        fs::write(lxc.join("config_session"), "session bind\n")?;
        fs::write(lxc.join("custom.conf"), "keep me\n")?;
        fixture.write(
            "waydroid.cfg",
            "[waydroid]\nsystem_ota = https://example.invalid\nsystem_datetime = 9\nmount_overlays = False\n",
        )?;
        fs::create_dir_all(fixture.instance.work.join("overlay_rw/system/app"))?;
        let shipped = fixture.root.path().join("shipped");
        fs::create_dir(&shipped)?;
        for image in ["system.img", "vendor.img"] {
            fs::copy(
                fixture.instance.work.join("images").join(image),
                shipped.join(image),
            )?;
        }
        let images = Images::find(
            std::slice::from_ref(&shipped),
            &fixture.instance.work.join("images"),
        )?;
        assert_eq!(images, Images::Preinstalled(shipped.clone()));
        fixture.run(images, false)?.commit()?;
        let cfg = fixture.read("waydroid.cfg")?;
        assert!(cfg.contains("system_ota = None\n"));
        assert!(cfg.contains("system_datetime = 0\n"));
        assert!(cfg.contains("mount_overlays = False\n"));
        assert!(cfg.contains(&format!("images_path = {}\n", shipped.display())));
        let props = fixture.read("waydroid_base.prop")?;
        assert!(props.contains("waydroid.updater.disabled=true\n"));
        assert!(!props.contains("waydroid.system_ota"));
        assert_eq!(
            fixture.read("lxc/waydroid/config_session")?,
            "session bind\n"
        );
        assert_eq!(fixture.read("lxc/waydroid/custom.conf")?, "keep me\n");
        assert!(!fixture.instance.work.join("overlay_rw/system/app").exists());
        Ok(())
    }

    #[test]
    fn missing_images_cancel_and_bad_input_change_nothing() -> Result<()> {
        let fixture = Fixture::new()?;
        let empty = fixture.root.path().join("empty");
        fs::create_dir(&empty)?;
        assert!(Images::find(&[], &empty).is_err());
        let old = "[waydroid]\narch = old\n";
        fixture.write("waydroid.cfg", old)?;
        assert!(fixture.run(fixture.installed(), true).is_err());
        assert!(
            fixture
                .init(fixture.installed())?
                .run(&AtomicBool::new(false), Some(Instant::now()))
                .is_err()
        );
        fixture.write("waydroid.cfg", "[waydroid]\n[properties]\nnot a property\n")?;
        assert!(fixture.run(fixture.installed(), false).is_err());
        fixture.write("waydroid.cfg", old)?;
        fixture.write("images/system.img", "not an image")?;
        assert!(fixture.run(fixture.installed(), false).is_err());
        assert_eq!(fixture.read("waydroid.cfg")?, old);
        assert_eq!(fs::read_dir(&fixture.instance.work)?.count(), 2);
        Ok(())
    }

    #[test]
    fn publication_failure_restores_predecessor() -> Result<()> {
        let fixture = Fixture::new()?;
        let work = &fixture.instance.work;
        let staging = work.join("staging");
        fs::create_dir(&staging)?;
        fs::write(staging.join("new-first"), "new")?;
        fs::write(work.join("first"), "old")?;
        let outputs = [
            (staging.join("new-first"), work.join("first")),
            (staging.join("missing"), work.join("second")),
        ];
        assert!(publish_with_rollback(&outputs, &staging).is_err());
        assert_eq!(fs::read(work.join("first"))?, b"old");
        assert!(!work.join("second").exists());
        Ok(())
    }

    #[test]
    fn host_facts_follow_upstream_detection() -> Result<()> {
        let root = tempfile::tempdir()?;
        let dri = root.path().join("dri");
        let drm = root.path().join("drm");
        fs::create_dir_all(&dri)?;
        for (node, driver) in [("renderD128", "nvidia"), ("renderD129", "v3d")] {
            fs::write(dri.join(node), "")?;
            fs::create_dir_all(drm.join(node).join("device"))?;
            fs::write(
                drm.join(node).join("device/uevent"),
                format!("DRIVER={driver}\n"),
            )?;
        }
        let node = render_node(&dri, &drm).ok_or("no node")?;
        assert!(node.ends_with("renderD129"));
        assert_eq!(kernel_driver(&drm, &node).as_deref(), Some("v3d"));
        assert_eq!(vulkan_driver("v3d"), None);
        assert_eq!(vulkan_driver("vc4"), Some("broadcom"));
        let tree = root.path().join("tree");
        fs::create_dir(&tree)?;
        fs::write(tree.join("model"), "Pi 5\0")?;
        fs::write(tree.join("name"), "\0")?;
        assert_eq!(device_tree(&tree), [("model", "Pi 5".to_owned())]);
        Ok(())
    }
}
