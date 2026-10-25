//! The Android session unit as Settings shows it, followed through systemd.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use deckard_core::keel::APP_ID_PREFIX;
use deckard_core::runtime_state::{ResetError, RuntimeState, State, UnitCall};
use deckard_core::sync;
use serde_json::{Map, Value};
use zbus::MatchRule;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str};

/// The Android session's user unit.
pub const UNIT: &str = "deckard-session.service";
/// systemd's manager interface.
pub const MANAGER: &str = "org.freedesktop.systemd1.Manager";
/// systemd's manager object.
pub const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD: &str = "org.freedesktop.systemd1";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const UNIT_INTERFACE: &str = "org.freedesktop.systemd1.Unit";

/// The runtime state Settings shows; cheap to clone.
#[derive(Clone)]
pub struct Runtime {
    state: Arc<Mutex<RuntimeState>>,
    policy: PathBuf,
    applications: PathBuf,
    memory: PathBuf,
}

impl Runtime {
    fn lock(&self) -> MutexGuard<'_, RuntimeState> {
        sync::lock(&self.state)
    }

    /// `policy` is Settings' `settings.json`, `applications` the desktop
    /// entries and `memory` the container's `memory.current`.
    #[must_use]
    pub fn new(policy: PathBuf, applications: PathBuf, memory: PathBuf) -> Self {
        let mut state = RuntimeState::default();
        if let Some(enabled) = read_policy(&policy).get("startAtLogin") {
            state.set_start_at_login(enabled.as_bool().unwrap_or(false));
        }
        Self {
            state: Arc::new(Mutex::new(state)),
            policy,
            applications,
            memory,
        }
    }

    /// `GetRuntime`'s and `RuntimeChanged`'s dictionary.
    pub fn snapshot(&self) -> HashMap<String, OwnedValue> {
        let mut state = self.lock();
        state.set_has_android_apps(has_apps(&self.applications));
        let now = SystemTime::now().duration_since(UNIX_EPOCH);
        let now_ms = i64::try_from(now.unwrap_or_default().as_millis()).unwrap_or(i64::MAX);
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

    /// Whether the snapshot changed.
    #[must_use]
    pub fn set_connected(&self, connected: bool) -> bool {
        self.lock().set_android_connected(connected)
    }

    /// Whether the snapshot changed.
    #[must_use]
    pub fn set_version(&self, version: &str) -> bool {
        self.lock().set_android_version(version)
    }

    /// Whether the snapshot changed.
    #[must_use]
    pub fn set_unit(&self, active: &str, entered_ms: i64) -> bool {
        self.lock().set_unit(active, entered_ms)
    }

    /// When the session unit became active, ms since the epoch; 0 while it isn't.
    pub fn active_since_ms(&self) -> i64 {
        self.lock().active_since_ms()
    }

    /// The session's state as Settings shows it.
    #[must_use]
    pub fn state(&self) -> State {
        self.lock().state()
    }

    /// Starts a reset.
    ///
    /// # Errors
    /// A reset is running, or the helper is not `available`.
    pub fn request_reset(&self, available: bool) -> Result<(), ResetError> {
        self.lock().request_reset(available)
    }

    /// Ends the running reset.
    pub fn finish_reset(&self) {
        self.lock().finish_reset();
    }

    /// Whether Android should start now that the user logged in.
    #[must_use]
    pub fn at_login(&self) -> bool {
        let mut state = self.lock();
        state.set_has_android_apps(has_apps(&self.applications));
        state.at_login()
    }

    /// Saves the choice in the policy file, keeping its other keys.
    ///
    /// # Errors
    /// The policy file could not be written.
    pub fn set_start_at_login(&self, enabled: bool) -> std::io::Result<()> {
        let mut policy = read_policy(&self.policy);
        policy.insert("startAtLogin".into(), Value::Bool(enabled));
        let parent = self.policy.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let tmp = self
            .policy
            .with_extension(format!("json.{}", std::process::id()));
        fs::write(&tmp, serde_json::to_vec(&policy)?)?;
        fs::rename(tmp, &self.policy)?;
        self.lock().set_start_at_login(enabled);
        Ok(())
    }
}

fn read_policy(path: &Path) -> Map<String, Value> {
    let policy = fs::read(path).ok();
    let policy = policy.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    policy.map(crate::object).unwrap_or_default()
}

fn has_apps(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(APP_ID_PREFIX) || !name.ends_with(".desktop") {
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

async fn manager_call(bus: &zbus::Connection, method: &str) -> zbus::Result<()> {
    let proxy = zbus::Proxy::new(bus, SYSTEMD, MANAGER_PATH, MANAGER).await?;
    let _: OwnedObjectPath = proxy.call(method, &(UNIT, "replace")).await?;
    Ok(())
}

/// Starts, stops or restarts the session unit.
///
/// # Errors
/// systemd refused the job.
pub async fn call_unit(bus: &zbus::Connection, call: UnitCall) -> zbus::Result<()> {
    let method = match call {
        UnitCall::Start => "StartUnit",
        UnitCall::Stop => "StopUnit",
        UnitCall::Restart => "RestartUnit",
    };
    manager_call(bus, method).await
}

fn refresh(properties: &Proxy<'_>, runtime: &Runtime) -> zbus::Result<bool> {
    let values: HashMap<String, OwnedValue> = properties.call("GetAll", &(UNIT_INTERFACE,))?;
    let active = values
        .get("ActiveState")
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("inactive");
    let entered_us = values
        .get("ActiveEnterTimestamp")
        .and_then(|v| u64::try_from(v).ok())
        .unwrap_or(0);
    let entered_ms = i64::try_from(entered_us / 1000).unwrap_or(i64::MAX);
    Ok(runtime.set_unit(active, entered_ms))
}

/// Follows the session unit's `PropertiesChanged` and calls `changed` each
/// time its state moves.
///
/// # Errors
/// Returns the D-Bus error that prevented the subscription.
pub fn watch_unit(
    bus: &Connection,
    runtime: Runtime,
    mut changed: impl FnMut() + Send + 'static,
) -> zbus::Result<()> {
    let manager = Proxy::new(bus, SYSTEMD, MANAGER_PATH, MANAGER)?;
    let path: OwnedObjectPath = manager.call("LoadUnit", &(UNIT,))?;
    let rule = crate::signals(SYSTEMD, path.as_str(), PROPERTIES)?.member("PropertiesChanged")?;
    let rule = rule.add_arg(UNIT_INTERFACE)?.build().to_owned();
    if let Err(error) = manager.call::<_, _, ()>("Subscribe", &()) {
        eprintln!("deckard-bridge: systemd Subscribe failed: {error}");
    }
    let properties = Proxy::new(bus, SYSTEMD, path, PROPERTIES)?;
    let rules: Vec<(MatchRule<'static>, crate::Relevant)> = vec![(rule, |_| true)];
    crate::follow(bus, rules, move || match refresh(&properties, &runtime) {
        Ok(true) => changed(),
        Ok(false) => {}
        Err(error) => eprintln!("deckard-bridge: reading {UNIT} failed: {error}"),
    })?;
    Ok(())
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
            apps.join("deckard.com.example.desktop"),
            "[Desktop Entry]\nName=Example\n",
        )
        .unwrap();
        fs::write(
            apps.join("deckard.hidden.desktop"),
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
}
