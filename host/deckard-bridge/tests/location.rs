#![allow(
    clippy::unwrap_used,
    reason = "private-bus tests drive a fixed fake GeoClue"
)]

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use deckard_bridge::location::{DESKTOP_ID, LocationSync, Plan, plan};
use serde_json::{Value, json};
use zbus::Connection;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::{ObjectPath, OwnedObjectPath};

const CLIENT_PATH: &str = "/org/freedesktop/GeoClue2/Client/1";
const LOCATION_PATH: &str = "/org/freedesktop/GeoClue2/Location/1";

#[derive(Debug, Default, PartialEq)]
struct Asked {
    clients: u32,
    desktop_id: String,
    accuracy: u32,
    threshold: u32,
    starts: u32,
    stops: u32,
}

type Seen = Arc<Mutex<Asked>>;

struct Manager(Seen);

#[interface(name = "org.freedesktop.GeoClue2.Manager")]
impl Manager {
    #[zbus(name = "GetClient")]
    fn get_client(&self) -> OwnedObjectPath {
        self.0.lock().unwrap().clients += 1;
        ObjectPath::from_static_str_unchecked(CLIENT_PATH).into()
    }
}

struct Client(Seen);

#[interface(name = "org.freedesktop.GeoClue2.Client")]
impl Client {
    #[zbus(property, name = "DesktopId")]
    fn desktop_id(&self) -> String {
        self.0.lock().unwrap().desktop_id.clone()
    }
    #[zbus(property, name = "DesktopId")]
    fn set_desktop_id(&mut self, value: String) {
        self.0.lock().unwrap().desktop_id = value;
    }
    #[zbus(property, name = "RequestedAccuracyLevel")]
    fn accuracy(&self) -> u32 {
        self.0.lock().unwrap().accuracy
    }
    #[zbus(property, name = "RequestedAccuracyLevel")]
    fn set_accuracy(&mut self, value: u32) {
        self.0.lock().unwrap().accuracy = value;
    }
    #[zbus(property, name = "TimeThreshold")]
    fn threshold(&self) -> u32 {
        self.0.lock().unwrap().threshold
    }
    #[zbus(property, name = "TimeThreshold")]
    fn set_threshold(&mut self, value: u32) {
        self.0.lock().unwrap().threshold = value;
    }
    #[zbus(name = "Start")]
    fn start(&self) {
        self.0.lock().unwrap().starts += 1;
    }
    #[zbus(name = "Stop")]
    fn stop(&self) {
        self.0.lock().unwrap().stops += 1;
    }
    #[zbus(signal, name = "LocationUpdated")]
    async fn location_updated(
        emitter: &SignalEmitter<'_>,
        old: ObjectPath<'_>,
        new: ObjectPath<'_>,
    ) -> zbus::Result<()>;
}

/// A fix with GeoClue's unknown altitude, and a property the bridge ignores.
struct Location;

#[interface(name = "org.freedesktop.GeoClue2.Location")]
impl Location {
    #[zbus(property, name = "Latitude")]
    fn latitude(&self) -> f64 {
        53.35
    }
    #[zbus(property, name = "Longitude")]
    fn longitude(&self) -> f64 {
        -6.26
    }
    #[zbus(property, name = "Accuracy")]
    fn accuracy(&self) -> f64 {
        25000.0
    }
    #[zbus(property, name = "Altitude")]
    fn altitude(&self) -> f64 {
        -f64::MAX
    }
    #[zbus(property, name = "Description")]
    fn description(&self) -> String {
        String::new()
    }
    #[zbus(property, name = "Timestamp")]
    fn timestamp(&self) -> (u64, u64) {
        (1_790_000_000, 500_000)
    }
}

fn until(seen: &Seen, done: impl Fn(&Asked) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(&seen.lock().unwrap()) {
        assert!(Instant::now() < deadline, "GeoClue never saw it");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The bus tests share one name and object paths on the session bus.
static GEOCLUE: Mutex<()> = Mutex::new(());

/// A fake GeoClue on the session bus; owns the name.
fn geoclue(seen: &Seen) -> Result<Connection, Box<dyn std::error::Error>> {
    let geoclue = zbus::block_on(Connection::session())?;
    let server = geoclue.object_server();
    zbus::block_on(server.at("/org/freedesktop/GeoClue2/Manager", Manager(seen.clone())))?;
    zbus::block_on(server.at(CLIENT_PATH, Client(seen.clone())))?;
    zbus::block_on(server.at(LOCATION_PATH, Location))?;
    zbus::block_on(geoclue.request_name("org.freedesktop.GeoClue2"))?;
    Ok(geoclue)
}

/// GeoClue has a new fix for the client.
fn updated(geoclue: &Connection) -> zbus::Result<()> {
    let server = geoclue.object_server();
    let client = zbus::block_on(server.interface::<_, Client>(CLIENT_PATH))?;
    let (root, location) = (
        ObjectPath::try_from("/")?,
        ObjectPath::try_from(LOCATION_PATH)?,
    );
    zbus::block_on(Client::location_updated(
        client.signal_emitter(),
        root,
        location,
    ))
}

fn sync(bus: &Connection) -> (LocationSync, mpsc::Receiver<serde_json::Map<String, Value>>) {
    let (tx, sent) = mpsc::channel();
    let tx = Mutex::new(tx);
    let sync = LocationSync::start(
        bus.clone(),
        Arc::new(move |m| tx.lock().unwrap().send(m).is_ok()),
    );
    (sync, sent)
}

#[test]
fn only_a_high_accuracy_request_in_view_is_exact() {
    let minute = Duration::from_secs(60);
    let second = Duration::from_secs(1);
    let running = |level| Plan {
        level,
        period: None,
    };
    let every = |level, period| Plan {
        level,
        period: Some(period),
    };
    assert_eq!(plan(second, true, true), running(8));
    assert_eq!(plan(second, false, true), running(6));
    assert_eq!(plan(minute, true, true), every(8, minute));
    // Out of view: Wi-Fi and cell, one fix at most every five minutes.
    assert_eq!(plan(second, true, false), every(6, 5 * minute));
    assert_eq!(plan(30 * minute, false, false), every(6, 30 * minute));
}

// Run with: dbus-run-session -- env DECKARD_BRIDGE_PRIVATE_BUS=1 cargo test -p deckard-bridge --test location
#[test]
fn cxx_location_follows_androids_request() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let _serial = GEOCLUE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let seen = Seen::default();
    let geoclue = geoclue(&seen)?;
    let updated = || updated(&geoclue);
    let bus = zbus::block_on(Connection::session())?;
    let (sync, sent) = sync(&bus);
    let request = |on: bool, interval: u64| {
        sync.handle(json!({"on":on, "interval":interval}).as_object().unwrap())
    };

    sync.set_in_view(true);
    request(true, 50000);
    until(&seen, |s| s.starts == 1 && s.threshold == 50);
    let asked = Asked {
        clients: 1,
        desktop_id: DESKTOP_ID.into(),
        accuracy: 6,
        threshold: 50,
        starts: 1,
        stops: 0,
    };
    assert_eq!(*seen.lock().unwrap(), asked);
    updated()?;
    let fix = sent.recv_timeout(Duration::from_secs(5))?;
    let expected = json!({"t":"location", "lat":53.35, "lon":-6.26, "acc":25000.0, "time":1_790_000_000_500_u64});
    assert_eq!(Value::Object(fix), expected);

    // A new interval while running changes the threshold, not the client.
    request(true, 5000);
    until(&seen, |s| s.threshold == 5);
    assert_eq!(seen.lock().unwrap().starts, 1);

    request(false, 0);
    until(&seen, |s| s.stops == 1);
    updated()?;
    assert!(sent.recv_timeout(Duration::from_millis(200)).is_err());

    request(true, 0);
    until(&seen, |s| s.starts == 2);
    assert_eq!(seen.lock().unwrap().clients, 1);
    sync.bridge_lost();
    until(&seen, |s| s.stops == 2);

    // GeoClue exits when idle; the next request gets a new client.
    zbus::block_on(geoclue.release_name("org.freedesktop.GeoClue2"))?;
    zbus::block_on(geoclue.request_name("org.freedesktop.GeoClue2"))?;
    request(true, 0);
    until(&seen, |s| s.clients == 2 && s.starts == 3);
    Ok(())
}

#[test]
fn cxx_location_out_of_view_takes_one_fix_and_stops() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let _serial = GEOCLUE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let seen = Seen::default();
    let geoclue = geoclue(&seen)?;
    let bus = zbus::block_on(Connection::session())?;
    let (sync, sent) = sync(&bus);
    let request = json!({"on": true, "interval": 1000, "quality": 100});

    // An app in front asks for high accuracy: GNSS, a running client.
    sync.set_in_view(true);
    sync.handle(request.as_object().unwrap());
    until(&seen, |s| {
        s.starts == 1 && s.accuracy == 8 && s.threshold == 1
    });
    updated(&geoclue)?;
    sent.recv_timeout(Duration::from_secs(5))?;
    updated(&geoclue)?;
    sent.recv_timeout(Duration::from_secs(5))?;
    assert_eq!(seen.lock().unwrap().stops, 0);

    // The screen goes off: no GNSS, and one fix per period.
    sync.set_in_view(false);
    until(&seen, |s| s.stops == 1 && s.starts == 2 && s.accuracy == 6);
    assert_eq!(seen.lock().unwrap().threshold, 0);
    updated(&geoclue)?;
    sent.recv_timeout(Duration::from_secs(5))?;
    until(&seen, |s| s.stops == 2);
    // Android asking again inside the period starts nothing.
    sync.handle(request.as_object().unwrap());
    updated(&geoclue)?;
    assert!(sent.recv_timeout(Duration::from_millis(300)).is_err());
    assert_eq!(seen.lock().unwrap().starts, 2);

    // Back in view, the request runs again at once.
    sync.set_in_view(true);
    until(&seen, |s| s.starts == 3 && s.accuracy == 8);
    Ok(())
}
