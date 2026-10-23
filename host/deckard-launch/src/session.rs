use crate::binder::{BinderConfig, Platform};
use crate::{Backend, Result};
use std::collections::HashMap;
use std::env;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

const SESSION_NAME: &str = "id.waydro.Session";
const CONTAINER_NAME: &str = "id.waydro.Container";
const SESSION_WAIT: Duration = Duration::from_secs(35);
const USER_WAIT: Duration = Duration::from_secs(90);

fn user_ready(boot: &str, credential_encrypted: &str) -> bool {
    boot == "1" && matches!(credential_encrypted, "true" | "1")
}

pub fn wayland_socket(runtime_dir: Option<&str>, display: Option<&str>) -> Result<PathBuf> {
    let display = display.filter(|v| !v.is_empty()).unwrap_or("wayland-0");
    let path = Path::new(display);
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    let runtime_dir = runtime_dir
        .filter(|v| !v.is_empty())
        .ok_or("XDG_RUNTIME_DIR is unset")?;
    let runtime_dir = Path::new(runtime_dir);
    if !runtime_dir.is_absolute() || display.contains('/') || display == "." || display == ".." {
        return Err("invalid Wayland runtime path".into());
    }
    Ok(runtime_dir.join(display))
}

pub struct NativeBackend {
    session_bus: Connection,
    system_bus: Connection,
    session_unit: String,
    config: BinderConfig,
    platform: Option<Platform>,
}

impl NativeBackend {
    pub fn new(config_path: &Path, session_unit: &str) -> Result<Self> {
        if session_unit.is_empty()
            || session_unit.contains('/')
            || !session_unit.ends_with(".service")
        {
            return Err("invalid session unit name".into());
        }
        Ok(Self {
            session_bus: Connection::session()?,
            system_bus: Connection::system()?,
            session_unit: session_unit.to_owned(),
            config: BinderConfig::from_file(config_path)?,
            platform: None,
        })
    }

    fn has_session(&self) -> Result<bool> {
        let proxy = Proxy::new(
            &self.session_bus,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )?;
        Ok(proxy.call("NameHasOwner", &(SESSION_NAME,))?)
    }

    fn container_proxy(&self) -> Result<Proxy<'_>> {
        Ok(Proxy::new(
            &self.system_bus,
            CONTAINER_NAME,
            "/ContainerManager",
            "id.waydro.ContainerManager",
        )?)
    }

    fn container_frozen(&self) -> Result<bool> {
        let session: HashMap<String, String> = self.container_proxy()?.call("GetSession", &())?;
        Ok(session.get("state").is_some_and(|state| state == "FROZEN"))
    }

    fn start_session(&self) -> Result<()> {
        let socket = wayland_socket(
            env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            env::var("WAYLAND_DISPLAY").ok().as_deref(),
        )?;
        UnixStream::connect(&socket)
            .map_err(|err| format!("Wayland socket {} is not ready: {err}", socket.display()))?;
        let manager = Proxy::new(
            &self.session_bus,
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
        )?;
        let _: OwnedObjectPath =
            manager.call("StartUnit", &(self.session_unit.as_str(), "replace"))?;
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
        if self.platform.is_none() {
            self.platform = Some(Platform::connect(&self.config)?);
        }
        Ok(self.platform.as_ref().expect("platform initialized"))
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
    fn ready(&mut self, start_session: bool) -> Result<bool> {
        if !self.has_session()? {
            if !start_session {
                return Err("Deckard session is stopped".into());
            }
            self.start_session()?;
        }
        if start_session {
            // The bus name can be owned before Android has booted or unlocked.
            self.wait_for_user_ready()?;
        }
        self.container_frozen()
    }

    fn unfreeze(&mut self) -> Result<()> {
        self.container_proxy()?.call::<_, _, ()>("Unfreeze", &())?;
        Ok(())
    }

    fn freeze(&mut self) -> Result<()> {
        self.container_proxy()?.call::<_, _, ()>("Freeze", &())?;
        Ok(())
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
    fn show_android_app(&mut self, package: &str) -> Result<()> {
        let app_id = format!("waydroid.{package}");
        let proxy = Proxy::new(
            &self.session_bus,
            "org.marathonos.Shell",
            "/org/marathonos/Shell/Navigation",
            "org.marathonos.Shell.Navigation1",
        )?;
        let shown: bool = proxy.call("ShowAndroidApp", &(app_id,))?;
        if !shown {
            return Err(format!("Shell rejected Android window handoff for {package}").into());
        }
        Ok(())
    }
    fn settings_put_string(&mut self, table: i32, key: &str, value: &str) -> Result<()> {
        self.platform()?.settings_put_string(table, key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_wayland_socket_like_python_session() {
        assert_eq!(
            wayland_socket(Some("/run/user/1000"), None).unwrap(),
            Path::new("/run/user/1000/wayland-0")
        );
        assert_eq!(
            wayland_socket(None, Some("/run/user/1000/marathon")).unwrap(),
            Path::new("/run/user/1000/marathon")
        );
        assert!(wayland_socket(None, None).is_err());
        assert!(wayland_socket(Some("/run/user/1000"), Some("../socket")).is_err());
    }

    #[test]
    fn fresh_session_requires_boot_and_user_unlock() {
        assert!(user_ready("1", "true"));
        assert!(user_ready("1", "1"));
        assert!(!user_ready("1", "false"));
        assert!(!user_ready("0", "true"));
    }
}
