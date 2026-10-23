//! Host wiring for the Android package D-Bus interface.
//!
//! The caller owns both authenticated bridge transports. Never infer a route
//! from an untrusted JSON field: uninstall requests require DeckardSystem.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use deckard_packages::{BridgeRoute, BridgeSend, MayChange, Packages};
use serde_json::{Map, Value};
use zbus::blocking::Connection;

pub const STORE_HANDLER: &str = "/usr/bin/marathon-store-handler";
const MAX_PARENT_HOPS: usize = 8;

#[derive(Clone)]
pub struct PackageBridge {
    service: Packages,
}

impl PackageBridge {
    /// Mount the package interface alongside `SettingsApi` on the owned bus.
    /// `send` must enqueue to the authenticated user or system socket selected
    /// by `BridgeRoute`, and return false if that socket is disconnected.
    pub fn register(
        bus: Connection,
        install_dir: impl Into<PathBuf>,
        send: Arc<BridgeSend>,
        store_handler: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let policy = may_change_policy(bus.inner().clone(), store_handler.into());
        let service = Packages::new(bus.clone(), install_dir, send, policy)?;
        bus.object_server()
            .at(deckard_packages::OBJECT_PATH, service.interface())
            .map_err(io::Error::other)?;
        Ok(Self { service })
    }

    /// Call only after the socket's peer UID has been authenticated.
    pub fn handle(&self, route: BridgeRoute, message: &Map<String, Value>) -> zbus::Result<()> {
        let kind = message.get("t").and_then(Value::as_str).unwrap_or("");
        self.service.handle(route, kind, message)
    }

    pub fn disconnected(&self, route: BridgeRoute) -> zbus::Result<()> {
        self.service.bridge_lost(route)
    }

    pub fn pending_count(&self) -> usize {
        self.service.pending_count()
    }
}

/// The C++ policy allows Store and Settings through Shell app identity, plus
/// the out-of-app store handler when its executable inode matches exactly.
pub fn may_change_policy(bus: zbus::Connection, handler: PathBuf) -> Arc<MayChange> {
    Arc::new(move |sender: String| {
        let bus = bus.clone();
        let handler = handler.clone();
        Box::pin(async move { caller_may_change(&bus, &sender, &handler).await })
    })
}

async fn caller_may_change(bus: &zbus::Connection, sender: &str, handler: &Path) -> bool {
    let Ok(dbus) = zbus::Proxy::new(
        bus,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await
    else {
        return false;
    };
    let pid_result: zbus::Result<u32> = dbus.call("GetConnectionUnixProcessID", &(sender,)).await;
    let Ok(mut pid) = pid_result else {
        return false;
    };
    if same_executable(pid, handler) {
        return true;
    }
    let Ok(shell) = zbus::Proxy::new(
        bus,
        "org.marathonos.Shell",
        "/org/marathonos/Shell/Navigation",
        "org.marathonos.Shell.Navigation1",
    )
    .await
    else {
        return false;
    };
    for _ in 0..MAX_PARENT_HOPS {
        if pid <= 1 {
            break;
        }
        let app_result: zbus::Result<String> = shell.call("AppIdForPid", &(pid,)).await;
        let Ok(app) = app_result else {
            return false;
        };
        if !app.is_empty() {
            return allowed_app(&app);
        }
        pid = parent_of(pid).unwrap_or(0);
    }
    false
}

fn allowed_app(app: &str) -> bool {
    matches!(app, "settings" | "store")
}

fn same_executable(pid: u32, handler: &Path) -> bool {
    if pid <= 1 {
        return false;
    }
    let Ok(process) = fs::metadata(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let Ok(expected) = fs::metadata(handler) else {
        return false;
    };
    process.dev() == expected.dev() && process.ino() == expected.ino()
}

fn parent_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.rsplit_once(')')?.1.trim_start();
    after_name.split_ascii_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "the test executable always exists")]
mod tests {
    use super::*;

    #[test]
    fn package_mutation_policy_is_narrow() {
        assert!(allowed_app("settings"));
        assert!(allowed_app("store"));
        assert!(!allowed_app("org.marathonos.store"));
        assert!(!allowed_app("shell"));
        assert!(!allowed_app(""));
    }

    #[test]
    fn executable_match_is_inode_based() {
        let executable = std::env::current_exe().expect("test executable");
        assert!(same_executable(std::process::id(), &executable));
        assert!(!same_executable(0, &executable));
        assert!(!same_executable(
            std::process::id(),
            Path::new("/missing/handler")
        ));
        assert!(parent_of(std::process::id()).is_some());
    }
}
