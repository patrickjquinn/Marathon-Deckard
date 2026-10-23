use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use deckard_core::runtime_state::{ResetError, RuntimeState, State, UnitCall};
use serde_json::Value;
use zbus::zvariant::{OwnedValue, Str};

pub const UNIT: &str = "deckard-session.service";
pub const MANAGER: &str = "org.freedesktop.systemd1.Manager";
pub const MANAGER_PATH: &str = "/org/freedesktop/systemd1";

#[derive(Clone)]
pub struct Runtime {
    state: Arc<Mutex<RuntimeState>>,
    policy: PathBuf,
    applications: PathBuf,
    memory: PathBuf,
}

impl Runtime {
    pub fn new(policy: PathBuf, applications: PathBuf, memory: PathBuf) -> Self {
        let mut state = RuntimeState::default();
        if let Some(enabled) = fs::read(&policy)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value.get("startAtLogin").cloned())
            .map(|value| value.as_bool().unwrap_or(false))
        {
            state.set_start_at_login(enabled);
        }
        Self {
            state: Arc::new(Mutex::new(state)),
            policy,
            applications,
            memory,
        }
    }

    pub fn snapshot(&self) -> HashMap<String, OwnedValue> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_has_android_apps(has_apps(&self.applications));
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let memory = fs::read_to_string(&self.memory)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let snapshot = state.snapshot(now_ms, memory);
        let name = match snapshot.state {
            State::Stopped => "stopped",
            State::Starting => "starting",
            State::Running => "running",
            State::Stopping => "stopping",
        };
        HashMap::from([
            ("state".into(), Str::from(name).into()),
            ("memoryBytes".into(), snapshot.memory_bytes.into()),
            (
                "androidVersion".into(),
                Str::from(snapshot.android_version.as_str()).into(),
            ),
            ("uptimeSeconds".into(), snapshot.uptime_seconds.into()),
            ("startAtLogin".into(), snapshot.start_at_login.into()),
            ("hasApps".into(), snapshot.has_android_apps.into()),
        ])
    }

    pub fn set_connected(&self, connected: bool) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_android_connected(connected)
            .is_empty()
    }

    pub fn set_version(&self, version: &str) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_android_version(version)
            .is_empty()
    }

    pub fn set_unit(&self, active: &str, entered_ms: i64) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_unit(active, entered_ms)
            .is_empty()
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).state()
    }

    pub fn request_reset(&self, available: bool) -> Result<u64, ResetError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_reset(available)
            .map(|(request, _)| request)
    }

    pub fn reset_timeout(&self, request: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.on_stop_timeout(request);
        state.on_reset_helper_failed_to_start(request);
    }

    pub fn reset_finished(&self, request: u64, success: bool) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_reset_helper_finished(request, success);
    }

    pub fn at_login(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_has_android_apps(has_apps(&self.applications));
        !state.at_login().is_empty()
    }

    pub fn set_start_at_login(&self, enabled: bool) -> std::io::Result<()> {
        let mut policy = fs::read(&self.policy)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        policy.insert("startAtLogin".into(), Value::Bool(enabled));
        let parent = self.policy.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let tmp = self
            .policy
            .with_extension(format!("json.{}", std::process::id()));
        fs::write(&tmp, serde_json::to_vec(&policy)?)?;
        fs::rename(tmp, &self.policy)?;
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_start_at_login(enabled);
        Ok(())
    }
}

fn has_apps(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("waydroid.") || !name.ends_with(".desktop") {
            return false;
        }
        fs::read(entry.path()).ok().is_some_and(|bytes| {
            let end = bytes.len().min(64 * 1024);
            !bytes[..end]
                .windows(b"\nNoDisplay=true".len())
                .any(|part| part == b"\nNoDisplay=true")
        })
    })
}

pub async fn call_unit(bus: &zbus::Connection, call: UnitCall) -> zbus::Result<()> {
    let proxy = zbus::Proxy::new(bus, "org.freedesktop.systemd1", MANAGER_PATH, MANAGER).await?;
    let method = match call {
        UnitCall::Start => "StartUnit",
        UnitCall::Stop => "StopUnit",
        UnitCall::Restart => "RestartUnit",
    };
    let _: zbus::zvariant::OwnedObjectPath = proxy.call(method, &(UNIT, "replace")).await?;
    Ok(())
}

pub fn watch_unit(bus: &zbus::blocking::Connection, runtime: &Runtime) -> zbus::Result<()> {
    let manager =
        zbus::blocking::Proxy::new(bus, "org.freedesktop.systemd1", MANAGER_PATH, MANAGER)?;
    let path: zbus::zvariant::OwnedObjectPath = manager.call("LoadUnit", &(UNIT,))?;
    loop {
        let props = zbus::blocking::Proxy::new(
            bus,
            "org.freedesktop.systemd1",
            path.as_str(),
            "org.freedesktop.DBus.Properties",
        )?;
        let values: HashMap<String, OwnedValue> =
            props.call("GetAll", &("org.freedesktop.systemd1.Unit",))?;
        let active = values
            .get("ActiveState")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("inactive");
        let entered_us = values
            .get("ActiveEnterTimestamp")
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(0);
        if runtime.set_unit(active, (entered_us / 1000) as i64) {
            let _ = bus.emit_signal(
                None::<&str>,
                "/org/marathonos/Deckard",
                "org.marathonos.Deckard1",
                "RuntimeChanged",
                &(runtime.snapshot(),),
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests use local temporary fixtures")]
mod tests {
    use super::*;

    #[test]
    fn runtime_snapshot_and_policy_match_settings_keys() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("applications");
        fs::create_dir(&apps).unwrap();
        fs::write(
            apps.join("waydroid.com.example.desktop"),
            "[Desktop Entry]\nName=Example\n",
        )
        .unwrap();
        fs::write(
            apps.join("waydroid.hidden.desktop"),
            "[Desktop Entry]\nNoDisplay=true\n",
        )
        .unwrap();
        let runtime = Runtime::new(
            dir.path().join("settings.json"),
            apps,
            dir.path().join("memory.current"),
        );
        let snapshot = runtime.snapshot();
        assert_eq!(
            String::try_from(snapshot["state"].clone()).unwrap(),
            "stopped"
        );
        assert!(bool::try_from(snapshot["hasApps"].clone()).unwrap());
        assert!(bool::try_from(snapshot["startAtLogin"].clone()).unwrap());
        runtime.set_start_at_login(false).unwrap();
        let snapshot = runtime.snapshot();
        assert!(!bool::try_from(snapshot["startAtLogin"].clone()).unwrap());
        let saved: Value =
            serde_json::from_slice(&fs::read(dir.path().join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved["startAtLogin"], false);
    }

    #[test]
    fn reset_timeout_releases_both_stop_phases() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(
            dir.path().join("settings.json"),
            dir.path().join("apps"),
            dir.path().join("memory"),
        );
        runtime.set_unit("active", 1000);
        let first = runtime.request_reset(true).unwrap();
        assert_eq!(
            runtime.request_reset(true),
            Err(ResetError::AlreadyResetting)
        );
        runtime.reset_timeout(first);
        let second = runtime.request_reset(true).unwrap();
        runtime.set_unit("inactive", 0);
        runtime.reset_timeout(second);
        assert!(runtime.request_reset(true).is_ok());
    }
}
