//! Host wiring for the Android package D-Bus interface.
//!
//! The caller owns both authenticated bridge transports. Never infer a route
//! from an untrusted JSON field: uninstall requests require DeckardSystem.

use std::io;
use std::path::PathBuf;
use std::slice;
use std::sync::Arc;

use deckard_packages::{BridgeRoute, BridgeSend, MayChange, Packages};
use serde_json::{Map, Value};
use zbus::blocking::Connection;

use crate::auth;

pub const STORE_HANDLER: &str = "/usr/bin/marathon-store-handler";

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
}

/// Store and Settings may change packages, through their Shell app id, and
/// so may the out-of-app store handler, by executable inode.
pub fn may_change_policy(bus: zbus::Connection, handler: PathBuf) -> Arc<MayChange> {
    Arc::new(move |sender: String| {
        let (bus, handler) = (bus.clone(), handler.clone());
        Box::pin(async move {
            let Ok(pid) = auth::pid(&bus, &sender).await else {
                return false;
            };
            if pid > 1 && auth::runs_one_of(pid, slice::from_ref(&handler)) {
                return true;
            }
            let app = auth::app(&bus, pid).await;
            app.is_ok_and(|app| matches!(app.as_str(), "settings" | "store"))
        })
    })
}
