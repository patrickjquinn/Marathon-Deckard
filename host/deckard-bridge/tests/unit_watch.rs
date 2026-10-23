#![allow(
    clippy::unwrap_used,
    reason = "the fake unit's lock is never poisoned in this test"
)]

use std::collections::HashMap;
use std::env;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use deckard_bridge::runtime::{self, Runtime};
use deckard_core::runtime_state::State;
use zbus::blocking::Connection;
use zbus::interface;
use zbus::zvariant::{OwnedObjectPath, Value};

const UNIT_PATH: &str = "/org/freedesktop/systemd1/unit/deckard_2dsession_2eservice";

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
type Shared = Arc<Mutex<(String, u64)>>;

struct Manager(mpsc::Sender<()>);

#[interface(name = "org.freedesktop.systemd1.Manager")]
impl Manager {
    fn load_unit(&self, name: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        if name != runtime::UNIT {
            return Err(zbus::fdo::Error::Failed(name.into()));
        }
        OwnedObjectPath::try_from(UNIT_PATH).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    fn subscribe(&self) {
        let _ = self.0.send(());
    }
}

struct Unit(Shared);

#[interface(name = "org.freedesktop.systemd1.Unit")]
impl Unit {
    #[zbus(property)]
    fn active_state(&self) -> String {
        self.0.lock().unwrap().0.clone()
    }

    #[zbus(property)]
    fn active_enter_timestamp(&self) -> u64 {
        self.0.lock().unwrap().1
    }
}

fn change(systemd: &Connection, unit: &Shared, active: &str, entered_us: u64) -> TestResult {
    *unit.lock().unwrap() = (active.into(), entered_us);
    let changed = HashMap::from([("ActiveState", Value::from(active))]);
    systemd.emit_signal(
        None::<&str>,
        UNIT_PATH,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &("org.freedesktop.systemd1.Unit", changed, Vec::<&str>::new()),
    )?;
    Ok(())
}

#[test]
fn follows_unit_property_changes_without_polling() -> TestResult {
    if env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let unit: Shared = Arc::new(Mutex::new(("inactive".into(), 0)));
    let (subscribed, subscriptions) = mpsc::channel();
    let systemd = Connection::session()?;
    systemd
        .object_server()
        .at("/org/freedesktop/systemd1", Manager(subscribed))?;
    systemd
        .object_server()
        .at(UNIT_PATH, Unit(Arc::clone(&unit)))?;
    systemd.request_name("org.freedesktop.systemd1")?;

    let runtime = Runtime::new(
        dir.path().join("settings.json"),
        dir.path().join("applications"),
        dir.path().join("memory.current"),
    );
    let (changed, changes) = mpsc::channel();
    let bus = Connection::session()?;
    let watched = runtime.clone();
    thread::spawn(move || {
        runtime::watch_unit(&bus, &watched, || {
            let _ = changed.send(());
        })
    });
    subscriptions.recv_timeout(Duration::from_secs(2))?;

    change(&systemd, &unit, "active", 5_000_000)?;
    changes.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(runtime.state(), State::Starting);
    assert_eq!(runtime.active_since_ms(), 5_000);

    change(&systemd, &unit, "deactivating", 0)?;
    changes.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(runtime.state(), State::Stopping);
    assert_eq!(runtime.active_since_ms(), 0);
    Ok(())
}
