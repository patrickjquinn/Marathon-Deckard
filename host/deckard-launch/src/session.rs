//! The launcher's [`Backend`]: the session bus, the container manager and
//! Android's `IPlatform`, plus the D-Bus proxies the host crates share.

use std::collections::HashMap;
use std::env;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::blocking::fdo::DBusProxy;
use zbus::names::BusName;
use zbus::zvariant::OwnedObjectPath;

use crate::binder::{BinderConfig, Platform};
use crate::{Backend, Container, Result, Session};

pub const SESSION_NAME: &str = "id.waydro.Session";
const SESSION_WAIT: Duration = Duration::from_secs(35);
const USER_WAIT: Duration = Duration::from_secs(90);

/// The root container manager.
#[zbus::proxy(
    interface = "id.waydro.ContainerManager",
    default_service = "id.waydro.Container",
    default_path = "/ContainerManager"
)]
pub trait ContainerManager {
    fn start(&self, session: HashMap<String, String>) -> zbus::Result<()>;
    fn stop(&self, quit_session: bool) -> zbus::Result<()>;
    fn freeze(&self) -> zbus::Result<()>;
    fn unfreeze(&self) -> zbus::Result<()>;
    fn get_session(&self) -> zbus::Result<HashMap<String, String>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Systemd {
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.marathonos.Shell.Navigation1",
    default_service = "org.marathonos.Shell",
    default_path = "/org/marathonos/Shell/Navigation"
)]
trait Navigation {
    fn show_android_app(&self, app_id: &str) -> zbus::Result<bool>;
}

fn user_ready(boot: &str, credential_encrypted: &str) -> bool {
    boot == "1" && matches!(credential_encrypted, "true" | "1")
}

/// The Wayland socket for `WAYLAND_DISPLAY` under `XDG_RUNTIME_DIR`.
///
/// # Errors
/// The runtime directory is unset or either value is unsafe.
pub fn wayland_socket(runtime_dir: Option<&str>, display: Option<&str>) -> Result<PathBuf> {
    let display = display.filter(|v| !v.is_empty()).unwrap_or("wayland-0");
    if Path::new(display).is_absolute() {
        return Ok(PathBuf::from(display));
    }
    let runtime_dir = Path::new(
        runtime_dir
            .filter(|v| !v.is_empty())
            .ok_or("XDG_RUNTIME_DIR is unset")?,
    );
    if !runtime_dir.is_absolute() || display.contains('/') || display == "." || display == ".." {
        return Err("invalid Wayland runtime path".into());
    }
    Ok(runtime_dir.join(display))
}

pub struct NativeBackend {
    session_bus: Connection,
    container: ContainerManagerProxyBlocking<'static>,
    session_unit: String,
    config: BinderConfig,
    platform: Option<Platform>,
}

impl NativeBackend {
    /// # Errors
    /// The unit name is invalid, a bus is unavailable or the config is unreadable.
    pub fn new(config_path: &Path, session_unit: &str) -> Result<Self> {
        if session_unit.is_empty()
            || session_unit.contains('/')
            || !session_unit.ends_with(".service")
        {
            return Err("invalid session unit name".into());
        }
        Ok(Self {
            session_bus: Connection::session()?,
            container: ContainerManagerProxyBlocking::new(&Connection::system()?)?,
            session_unit: session_unit.to_owned(),
            config: BinderConfig::from_file(config_path)?,
            platform: None,
        })
    }

    fn has_session(&self) -> Result<bool> {
        let name = BusName::try_from(SESSION_NAME)?;
        Ok(DBusProxy::new(&self.session_bus)?.name_has_owner(name)?)
    }

    fn start_session(&self) -> Result<()> {
        let socket = wayland_socket(
            env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            env::var("WAYLAND_DISPLAY").ok().as_deref(),
        )?;
        UnixStream::connect(&socket)
            .map_err(|err| format!("Wayland socket {} is not ready: {err}", socket.display()))?;
        SystemdProxyBlocking::new(&self.session_bus)?.start_unit(&self.session_unit, "replace")?;
        let deadline = Instant::now() + SESSION_WAIT;
        while !self.has_session()? {
            if Instant::now() >= deadline {
                return Err(format!("{} did not acquire {SESSION_NAME}", self.session_unit).into());
            }
            thread::sleep(Duration::from_millis(150));
        }
        Ok(())
    }

    fn platform(&mut self) -> Result<&Platform> {
        let platform = match self.platform.take() {
            Some(platform) => platform,
            None => Platform::connect(&self.config)?,
        };
        Ok(self.platform.insert(platform))
    }

    fn wait_for_user_ready(&mut self) -> Result<()> {
        let deadline = Instant::now() + USER_WAIT;
        loop {
            let platform = self.platform()?;
            let boot = platform.getprop("sys.boot_completed", "")?;
            let credential_encrypted = platform.getprop("sys.user.0.ce_available", "")?;
            if user_ready(&boot, &credential_encrypted) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("Android user did not unlock before launch timeout".into());
            }
            thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Backend for NativeBackend {
    fn ready(&mut self, session: Session) -> Result<Container> {
        if !self.has_session()? {
            if session == Session::Existing {
                return Err("Deckard session is stopped".into());
            }
            self.start_session()?;
        }
        if session == Session::Start {
            // The bus name can be owned before Android has booted or unlocked.
            self.wait_for_user_ready()?;
        }
        let state = self.container.get_session()?;
        if state.get("state").is_some_and(|state| state == "FROZEN") {
            Ok(Container::Frozen)
        } else {
            Ok(Container::Running)
        }
    }

    fn unfreeze(&mut self) -> Result<()> {
        Ok(self.container.unfreeze()?)
    }

    fn freeze(&mut self) -> Result<()> {
        Ok(self.container.freeze()?)
    }

    fn setprop(&mut self, key: &str, value: &str) -> Result<()> {
        self.platform()?.setprop(key, value)
    }

    fn getprop(&mut self, key: &str, fallback: &str) -> Result<String> {
        self.platform()?.getprop(key, fallback)
    }

    fn launch_app(&mut self, package: &str) -> Result<()> {
        self.platform()?.launch_app(package)
    }

    fn launch_intent(&mut self, action: &str, uri: &str) -> Result<String> {
        self.platform()?.launch_intent(action, uri)
    }

    fn settings_put_string(&mut self, table: i32, key: &str, value: &str) -> Result<()> {
        self.platform()?.settings_put_string(table, key, value)
    }

    fn show_android_app(&mut self, package: &str) -> Result<()> {
        let navigation = NavigationProxyBlocking::new(&self.session_bus)?;
        if !navigation.show_android_app(&format!("waydroid.{package}"))? {
            return Err(format!("Shell rejected Android window handoff for {package}").into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_wayland_socket_like_python_session() -> Result<()> {
        assert_eq!(
            wayland_socket(Some("/run/user/1000"), None)?,
            Path::new("/run/user/1000/wayland-0")
        );
        assert_eq!(
            wayland_socket(None, Some("/run/user/1000/marathon"))?,
            Path::new("/run/user/1000/marathon")
        );
        assert!(wayland_socket(None, None).is_err());
        assert!(wayland_socket(Some("/run/user/1000"), Some("../socket")).is_err());
        Ok(())
    }

    #[test]
    fn fresh_session_requires_boot_and_user_unlock() {
        assert!(user_ready("1", "true"));
        assert!(user_ready("1", "1"));
        assert!(!user_ready("1", "false"));
        assert!(!user_ready("0", "true"));
    }
}
