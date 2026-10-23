#![allow(
    clippy::unwrap_used,
    reason = "private-bus tests drive a fixed fake GeoClue"
)]

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use deckard_bridge::location::{DESKTOP_ID, LocationSync};
use serde_json::{Value, json};
use zbus::blocking::Connection;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath};

const CLIENT_PATH: &str = "/org/freedesktop/GeoClue2/Client/1";
const LOCATION_PATH: &str = "/org/freedesktop/GeoClue2/Location/1";

#[derive(Default)]
struct Seen {
    clients: u32,
    desktop_id: String,
    accuracy: u32,
    threshold: u32,
    starts: u32,
    stops: u32,
}

type Shared = Arc<Mutex<Seen>>;

struct Manager(Shared);

#[interface(name = "org.freedesktop.GeoClue2.Manager")]
impl Manager {
    #[zbus(name = "GetClient")]
    fn get_client(&self) -> OwnedObjectPath {
        self.0.lock().unwrap().clients += 1;
        ObjectPath::from_static_str_unchecked(CLIENT_PATH).into()
    }
}

struct Client(Shared);

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
    #[zbus(property, name = "Speed")]
    fn speed(&self) -> f64 {
        -1.0
    }
    #[zbus(property, name = "Heading")]
    fn heading(&self) -> f64 {
        -1.0
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

fn until(seen: &Shared, done: impl Fn(&Seen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(&seen.lock().unwrap()) {
        assert!(Instant::now() < deadline, "GeoClue never saw it");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn request(sync: &LocationSync, request: Value) {
    sync.handle(request.as_object().unwrap());
}

// Run with: dbus-run-session -- env DECKARD_BRIDGE_PRIVATE_BUS=1 cargo test -p deckard-bridge --test location
#[test]
fn cxx_location_follows_androids_request() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let seen = Shared::default();
    let geoclue = Connection::session()?;
    let server = geoclue.object_server();
    server.at("/org/freedesktop/GeoClue2/Manager", Manager(seen.clone()))?;
    server.at(CLIENT_PATH, Client(seen.clone()))?;
    server.at(LOCATION_PATH, Location)?;
    geoclue.request_name("org.freedesktop.GeoClue2")?;
    let updated = || {
        let client = server.interface::<_, Client>(CLIENT_PATH).unwrap();
        let location = ObjectPath::from_static_str_unchecked(LOCATION_PATH);
        let root = ObjectPath::from_static_str_unchecked("/");
        zbus::block_on(Client::location_updated(
            client.signal_emitter(),
            root,
            location,
        ))
        .unwrap();
    };

    let (tx, sent) = mpsc::channel();
    let tx = Mutex::new(tx);
    let bus = Connection::session()?;
    let send = Arc::new(move |message| tx.lock().unwrap().send(message).is_ok());
    let sync = LocationSync::start(bus.inner().clone(), send);

    request(
        &sync,
        json!({"t":"location_request", "on":true, "interval":60000}),
    );
    until(&seen, |s| s.starts == 1 && s.threshold == 60);
    {
        let s = seen.lock().unwrap();
        assert_eq!(
            (s.clients, s.desktop_id.as_str(), s.accuracy),
            (1, DESKTOP_ID, 8)
        );
    }
    updated();
    let fix = sent.recv_timeout(Duration::from_secs(5))?;
    assert_eq!((&fix["lon"], &fix["acc"]), (&json!(-6.26), &json!(25000.0)));
    assert_eq!(fix["time"], 1_790_000_000_500.0);
    assert!(!fix.contains_key("alt") && !fix.contains_key("speed"));

    // A new interval while running changes the threshold, not the client.
    request(
        &sync,
        json!({"t":"location_request", "on":true, "interval":5000}),
    );
    until(&seen, |s| s.threshold == 5);
    assert_eq!(seen.lock().unwrap().starts, 1);

    request(&sync, json!({"t":"location_request", "on":false}));
    until(&seen, |s| s.stops == 1);
    updated();
    assert!(sent.recv_timeout(Duration::from_millis(200)).is_err());

    request(&sync, json!({"t":"location_request", "on":true}));
    until(&seen, |s| s.starts == 2);
    assert_eq!(seen.lock().unwrap().clients, 1);
    sync.bridge_lost();
    until(&seen, |s| s.stops == 2);

    // GeoClue exits when idle; the next request gets a new client.
    geoclue.release_name("org.freedesktop.GeoClue2")?;
    geoclue.request_name("org.freedesktop.GeoClue2")?;
    request(&sync, json!({"t":"location_request", "on":true}));
    until(&seen, |s| s.clients == 2 && s.starts == 3);
    Ok(())
}
