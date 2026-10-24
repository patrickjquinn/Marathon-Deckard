//! Deckard's host names, and the ones the prebuilt Android image speaks.

/// The root state directory: images, overlay, LXC and `waydroid.cfg`.
pub const WORK: &str = "/var/lib/deckard";
/// The instance's configuration in [`WORK`].
pub const CONFIG: &str = "/var/lib/deckard/waydroid.cfg";
/// The LXC container, also its cgroup's suffix.
pub const CONTAINER: &str = "deckard";
/// The container's cgroup.
pub const CGROUP: &str = "/sys/fs/cgroup/lxc.payload.deckard";
/// The host bridge.
pub const BRIDGE: &str = "deckard0";
/// Runtime files shared with LXC.
pub const RUN: &str = "/run/deckard-lxc";
/// The user's state under `XDG_DATA_HOME`; Android's `/data` is its `data`.
pub const USER_STATE: &str = "deckard";
/// In `XDG_RUNTIME_DIR` while the session asks for the container.
pub const REQUESTED: &str = "deckard-requested";
/// An Android app's desktop, window, Keel and task id is this and its package.
pub const APP_ID_PREFIX: &str = "deckard.";

/// Contracts of the prebuilt Android image, renamed with Deckard's own image
/// build.
pub mod android {
    /// The image's binder service and interface of each host service.
    pub const HARDWARE: (&str, &str) = ("waydroidhardware", "lineageos.waydroid.IHardware");
    pub const PLATFORM: (&str, &str) = ("waydroidplatform", "lineageos.waydroid.IPlatform");
    pub const USER_MONITOR: (&str, &str) =
        ("waydroidusermonitor", "lineageos.waydroid.IUserMonitor");
    /// The composer HAL in the vendor image.
    pub const COMPOSER: &str = "vendor/lib64/hw/hwcomposer.waydroid.so";
    /// Where the image reads the session's properties, in its rootfs.
    pub const VENDOR_PROPERTIES: &str = "vendor/waydroid.prop";
    /// `waydroid.active_apps` value for Android's own full-screen window.
    pub const FULL_UI: &str = "Waydroid";
}

/// An Android property of the image's namespace: `prop!("active_apps")` is
/// `waydroid.active_apps`, `prop!("persist.", "width")` `persist.waydroid.width`.
#[macro_export]
macro_rules! prop {
    ($name:literal) => {
        concat!("waydroid.", $name)
    };
    ($scope:literal, $name:literal) => {
        concat!($scope, "waydroid.", $name)
    };
}
