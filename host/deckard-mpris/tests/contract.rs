#![allow(clippy::unwrap_used, reason = "fixed private-bus test fixtures")]

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use deckard_mpris::MprisAdapter;
use image::{DynamicImage, ImageFormat};
use serde_json::{Value, json};
use zbus::blocking::{Connection, Proxy, connection::Builder};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const ROOT: &str = "org.mpris.MediaPlayer2";

struct PrivateBus {
    daemon: Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl PrivateBus {
    fn start() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("bus");
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = daemon
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing daemon stdout"))?;
        let mut address = String::new();
        BufReader::new(stdout).read_line(&mut address)?;
        if address.trim().is_empty() {
            return Err(io::Error::other("dbus-daemon did not publish address").into());
        }
        Ok(Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        })
    }

    fn connection(&self) -> TestResult<Connection> {
        Ok(Builder::address(self.address.as_str())?
            .method_timeout(Duration::from_secs(3))
            .build()?)
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn message(sid: i32, package: &str) -> Value {
    json!({
        "t": "media", "sid": sid, "pkg": package, "label": "Music", "state": "Playing",
        "position": 1000, "duration": 10000, "speed": 1.0,
        "title": "First", "artist": "Artist", "album": "Album",
        "can": {"play": true, "pause": true, "next": true, "previous": true, "seek": true}
    })
}

fn name(package: &str, sid: i32) -> String {
    format!(
        "org.mpris.MediaPlayer2.waydroid_{}.instance{sid}",
        package.replace('.', "_")
    )
}

fn one(adapter: &MprisAdapter) -> TestResult<Value> {
    let mut commands = adapter.take_commands();
    if commands.len() != 1 {
        return Err(io::Error::other(format!("expected one command: {commands:?}")).into());
    }
    Ok(Value::Object(commands.remove(0)))
}

#[test]
fn two_players_have_distinct_connections_and_legacy_properties() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let client = bus.connection()?;
    let art = tempfile::tempdir()?;
    let mut adapter = MprisAdapter::new(Some(&bus.address), art.path())?;
    for (sid, package) in [(3, "org.lineageos.eleven"), (4, "org.example.player")] {
        adapter.handle(message(sid, package).as_object().unwrap())?;
    }
    assert_eq!(adapter.active_players(), 2);

    let first = name("org.lineageos.eleven", 3);
    let second = name("org.example.player", 4);
    let dbus = Proxy::new(
        &client,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let first_owner: String = dbus.call("GetNameOwner", &(first.as_str(),))?;
    let second_owner: String = dbus.call("GetNameOwner", &(second.as_str(),))?;
    assert_ne!(first_owner, second_owner);

    let root = Proxy::new(&client, first.as_str(), PATH, ROOT)?;
    assert_eq!(root.get_property::<String>("Identity")?, "Music");
    assert_eq!(
        root.get_property::<String>("DesktopEntry")?,
        "waydroid.org.lineageos.eleven"
    );
    assert!(!root.get_property::<bool>("CanQuit")?);
    assert!(!root.get_property::<bool>("CanRaise")?);
    assert!(!root.get_property::<bool>("HasTrackList")?);
    assert!(
        root.get_property::<Vec<String>>("SupportedUriSchemes")?
            .is_empty()
    );

    let player = Proxy::new(&client, first.as_str(), PATH, PLAYER)?;
    assert_eq!(player.get_property::<String>("PlaybackStatus")?, "Playing");
    assert_eq!(player.get_property::<f64>("Rate")?, 1.0);
    assert_eq!(player.get_property::<f64>("Volume")?, 1.0);
    assert!(player.get_property::<bool>("CanSeek")?);
    assert!(player.get_property::<bool>("CanControl")?);
    let metadata: HashMap<String, OwnedValue> = player.get_property("Metadata")?;
    let title: String = metadata["xesam:title"].clone().try_into()?;
    assert_eq!(title, "First");
    let length: i64 = metadata["mpris:length"].clone().try_into()?;
    assert_eq!(length, 10_000_000);
    let artists: Vec<String> = metadata["xesam:artist"].clone().try_into()?;
    assert_eq!(artists, vec!["Artist"]);
    let track: OwnedObjectPath = metadata["mpris:trackid"].clone().try_into()?;
    assert_eq!(track.as_str(), "/org/marathonos/deckard/session3/track1");

    player.call::<_, _, ()>("Next", &())?;
    assert_eq!(
        one(&adapter)?,
        json!({"t":"media_cmd","sid":3,"cmd":"next","position":0})
    );
    for (method, command) in [
        ("Previous", "previous"),
        ("Pause", "pause"),
        ("PlayPause", "playpause"),
        ("Stop", "stop"),
        ("Play", "play"),
    ] {
        let _: () = player.call(method, &())?;
        assert_eq!(one(&adapter)?["cmd"], command);
    }
    let _: () = player.call("OpenUri", &(String::from("file:///ignored"),))?;
    assert!(adapter.take_commands().is_empty());
    let _: () = player.call("Seek", &(2_000_000_i64,))?;
    let command = one(&adapter)?;
    assert_eq!(command["cmd"], "seek");
    assert!(command["position"].as_i64().unwrap() >= 3_000);
    let _: () = player.call("SetPosition", &(track.clone(), 4_123_456_i64))?;
    assert_eq!(one(&adapter)?["position"], 4_123);
    let wrong = OwnedObjectPath::try_from("/org/marathonos/deckard/other")?;
    let _: () = player.call("SetPosition", &(wrong, 100_i64))?;
    assert!(adapter.take_commands().is_empty());

    adapter.handle(json!({"t": "media_gone", "sid": 3}).as_object().unwrap())?;
    assert_eq!(adapter.active_players(), 1);
    let deadline = Instant::now() + Duration::from_secs(2);
    while dbus
        .call::<_, _, String>("GetNameOwner", &(first.as_str(),))
        .is_ok()
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        dbus.call::<_, _, String>("GetNameOwner", &(first.as_str(),))
            .is_err()
    );
    assert!(
        dbus.call::<_, _, String>("GetNameOwner", &(second.as_str(),))
            .is_ok()
    );
    adapter.clear();
    assert_eq!(adapter.active_players(), 0);
    Ok(())
}

#[test]
fn updates_emit_properties_and_seeked_with_new_track_id() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let client = bus.connection()?;
    let art = tempfile::tempdir()?;
    let mut adapter = MprisAdapter::new(Some(&bus.address), art.path())?;
    let first = message(9, "org.example.player");
    adapter.handle(first.as_object().unwrap())?;
    let service = name("org.example.player", 9);
    let properties = Proxy::new(
        &client,
        service.as_str(),
        PATH,
        "org.freedesktop.DBus.Properties",
    )?;
    let mut changes = properties.receive_signal("PropertiesChanged")?;
    let player = Proxy::new(&client, service.as_str(), PATH, PLAYER)?;
    let mut seeked = player.receive_signal("Seeked")?;

    let mut update = first.clone();
    update["state"] = json!("Paused");
    update["position"] = json!(7000);
    adapter.handle(update.as_object().unwrap())?;
    let change = changes
        .next()
        .ok_or_else(|| io::Error::other("no property signal"))?;
    let (iface, changed, invalidated): (String, HashMap<String, OwnedValue>, Vec<String>) =
        change.body().deserialize()?;
    assert_eq!(iface, PLAYER);
    assert!(invalidated.is_empty());
    assert_eq!(
        String::try_from(changed["PlaybackStatus"].clone())?,
        "Paused"
    );
    assert!(bool::try_from(changed["CanSeek"].clone())?);
    let signal = seeked
        .next()
        .ok_or_else(|| io::Error::other("no seeked signal"))?;
    let (position,): (i64,) = signal.body().deserialize()?;
    assert_eq!(position, 7_000_000);

    update["title"] = json!("Second");
    adapter.handle(update.as_object().unwrap())?;
    let changed_track = changes
        .next()
        .ok_or_else(|| io::Error::other("no track signal"))?;
    let (_, changed, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
        changed_track.body().deserialize()?;
    let changed_metadata: HashMap<String, OwnedValue> = changed["Metadata"].clone().try_into()?;
    let changed_title: String = changed_metadata["xesam:title"].clone().try_into()?;
    assert_eq!(changed_title, "Second");
    let metadata: HashMap<String, OwnedValue> = player.get_property("Metadata")?;
    let track: OwnedObjectPath = metadata["mpris:trackid"].clone().try_into()?;
    assert_eq!(track.as_str(), "/org/marathonos/deckard/session9/track2");
    Ok(())
}

#[test]
fn player_limit_seek_capability_and_art_lifecycle() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let client = bus.connection()?;
    let art = tempfile::tempdir()?;
    let mut adapter = MprisAdapter::new(Some(&bus.address), art.path())?;

    let mut png = std::io::Cursor::new(Vec::new());
    DynamicImage::new_rgba8(2, 2).write_to(&mut png, ImageFormat::Png)?;
    let mut first = message(1, "org.example.player");
    first["art"] = json!(base64::engine::general_purpose::STANDARD.encode(png.get_ref()));
    first["can"]["seek"] = json!(false);
    adapter.handle(first.as_object().unwrap())?;
    let service = name("org.example.player", 1);
    let player = Proxy::new(&client, service.as_str(), PATH, PLAYER)?;
    let metadata: HashMap<String, OwnedValue> = player.get_property("Metadata")?;
    let art_url = String::try_from(metadata["mpris:artUrl"].clone())?;
    let art_path = art_url.strip_prefix("file://").unwrap();
    assert!(std::path::Path::new(art_path).is_file());
    assert!(!player.get_property::<bool>("CanSeek")?);
    let _: () = player.call("Seek", &(1_000_000_i64,))?;
    assert!(adapter.take_commands().is_empty());

    for sid in 2..=8 {
        adapter.handle(message(sid, "org.example.player").as_object().unwrap())?;
    }
    assert_eq!(adapter.active_players(), 8);
    assert!(
        adapter
            .handle(message(9, "org.example.player").as_object().unwrap())
            .is_err()
    );
    assert_eq!(adapter.active_players(), 8);

    let mut cleared = first.clone();
    cleared["art"] = json!("");
    adapter.handle(cleared.as_object().unwrap())?;
    let refreshed = Proxy::new(&client, service.as_str(), PATH, PLAYER)?;
    let metadata: HashMap<String, OwnedValue> = refreshed.get_property("Metadata")?;
    assert!(!metadata.contains_key("mpris:artUrl"));
    assert!(!std::path::Path::new(art_path).exists());
    adapter.clear();
    assert_eq!(adapter.active_players(), 0);
    Ok(())
}

#[test]
fn new_wipes_stale_art_and_clear_drops_queued_commands() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let client = bus.connection()?;
    let art = tempfile::tempdir()?;
    let stale = art.path().join("1-1.png");
    std::fs::write(&stale, b"left by an earlier run")?;
    let mut adapter = MprisAdapter::new(Some(&bus.address), art.path())?;
    assert!(!stale.exists());
    adapter.handle(message(1, "org.example.old").as_object().unwrap())?;
    let service = name("org.example.old", 1);
    let player = Proxy::new(&client, service.as_str(), PATH, PLAYER)?;
    let _: () = player.call("Play", &())?;
    adapter.clear();
    assert!(adapter.take_commands().is_empty());
    Ok(())
}
