#![allow(clippy::unwrap_used, reason = "fixed JSON fixtures")]

#[allow(dead_code)]
#[path = "../src/app_channel.rs"]
mod app_channel;
#[allow(dead_code)]
#[path = "../src/audio.rs"]
mod audio;
#[allow(dead_code)]
#[path = "../src/mpris.rs"]
mod mpris;

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::json;
use zbus::blocking::{Connection, Proxy, connection::Builder};
use zbus::zvariant::{OwnedValue, Str};

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

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
        let mut address = String::new();
        BufReader::new(
            daemon
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("missing bus address pipe"))?,
        )
        .read_line(&mut address)?;
        if address.trim().is_empty() {
            return Err(io::Error::other("bus address missing").into());
        }
        Ok(Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        })
    }

    fn connection(&self) -> TestResult<Connection> {
        Ok(Builder::address(self.address.as_str())?
            .method_timeout(Duration::from_secs(2))
            .build()?)
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn media(sid: i32, package: &str) -> serde_json::Map<String, serde_json::Value> {
    json!({
        "t":"media", "sid":sid, "pkg":package, "label":"Music", "state":"Playing",
        "position":1000, "duration":10000, "speed":1.0,
        "title":"First", "artist":"Artist", "album":"Album",
        "can":{"play":true,"pause":true,"next":true,"previous":true,"seek":true}
    })
    .as_object()
    .unwrap()
    .clone()
}

fn name(sid: i32, package: &str) -> String {
    format!(
        "org.mpris.MediaPlayer2.waydroid_{}.instance{sid}",
        package.replace('.', "_")
    )
}

fn owner(bus: &Connection, name: &str) -> zbus::Result<String> {
    let proxy = Proxy::new(
        bus,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    proxy.call("GetNameOwner", &(name,))
}

#[test]
fn media_bridge_owns_distinct_players_and_routes_commands() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let client = bus.connection()?;
    let art = tempfile::tempdir()?;
    let mut bridge = mpris::MediaBridge::new_at(Some(&bus.address), art.path());
    bridge.connected(7);
    bridge.handle_authenticated(7, &media(3, "org.example.first"))?;
    bridge.handle_authenticated(7, &media(4, "org.example.second"))?;
    assert_eq!(bridge.active_players(), 2);

    let first = name(3, "org.example.first");
    let second = name(4, "org.example.second");
    assert_ne!(owner(&client, &first)?, owner(&client, &second)?);
    let player = Proxy::new(
        &client,
        first.as_str(),
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    )?;
    assert_eq!(player.get_property::<String>("PlaybackStatus")?, "Playing");
    let _: () = player.call("Next", &())?;
    assert_eq!(
        bridge.drain_commands(),
        vec![(
            7,
            json!({"t":"media_cmd","sid":3,"cmd":"next","position":0})
                .as_object()
                .unwrap()
                .clone()
        )]
    );

    bridge.handle_authenticated(8, json!({"t":"media_gone","sid":3}).as_object().unwrap())?;
    assert_eq!(bridge.active_players(), 2);
    bridge.handle_authenticated(7, json!({"t":"media_gone","sid":3}).as_object().unwrap())?;
    assert_eq!(bridge.active_players(), 1);
    assert!(owner(&client, &first).is_err());
    assert!(owner(&client, &second).is_ok());
    Ok(())
}

#[test]
fn disconnect_and_reconnect_drop_stale_players() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let client = bus.connection()?;
    let art = tempfile::tempdir()?;
    let mut bridge = mpris::MediaBridge::new_at(Some(&bus.address), art.path());
    bridge.connected(10);
    bridge.handle_authenticated(10, &media(1, "org.example.old"))?;
    let old = name(1, "org.example.old");
    assert!(owner(&client, &old).is_ok());
    bridge.connected(11);
    assert_eq!(bridge.active_players(), 0);
    assert!(owner(&client, &old).is_err());
    bridge.handle_authenticated(10, &media(1, "org.example.old"))?;
    assert_eq!(bridge.active_players(), 0);
    bridge.handle_authenticated(11, &media(2, "org.example.new"))?;
    bridge.disconnected(10);
    assert_eq!(bridge.active_players(), 1);
    bridge.disconnected(11);
    assert_eq!(bridge.active_players(), 0);
    assert!(owner(&client, &name(2, "org.example.new")).is_err());
    assert!(bridge.drain_commands().is_empty());
    Ok(())
}

#[zbus::interface(name = "org.marathonos.Shell.Settings1")]
impl ShellSettings {
    #[zbus(name = "GetState")]
    fn get_state(&self) -> HashMap<String, OwnedValue> {
        self.state.clone()
    }
}

struct ShellSettings {
    state: HashMap<String, OwnedValue>,
}

#[test]
fn audio_state_matches_cpp_policy_and_clamps_volume() {
    let mut state = HashMap::new();
    assert_eq!(
        audio::AudioState::from_shell(&state),
        audio::AudioState {
            quiet: false,
            alarm: 0.9
        }
    );
    state.insert(
        "audioProfile".into(),
        OwnedValue::from(Str::from("vibrate")),
    );
    state.insert("alarmVolume".into(), OwnedValue::from(1.7_f64));
    assert_eq!(
        audio::AudioState::from_shell(&state),
        audio::AudioState {
            quiet: true,
            alarm: 1.0
        }
    );
    state.insert("audioProfile".into(), OwnedValue::from(Str::from("normal")));
    state.insert("dndEnabled".into(), OwnedValue::from(true));
    state.insert("alarmVolume".into(), OwnedValue::from(f64::NAN));
    assert_eq!(
        audio::AudioState::from_shell(&state),
        audio::AudioState {
            quiet: true,
            alarm: 0.9
        }
    );
}

#[test]
fn audio_refresh_deduplicates_and_resends() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let state = HashMap::from([
        (
            "audioProfile".to_owned(),
            OwnedValue::from(Str::from("silent")),
        ),
        ("alarmVolume".to_owned(), OwnedValue::from(0.4_f64)),
    ]);
    let _shell = Builder::address(bus.address.as_str())?
        .name("org.marathonos.Shell")?
        .serve_at("/org/marathonos/Shell/Settings", ShellSettings { state })?
        .build()?;
    let mut sync = audio::AudioSync::new(bus.connection()?);
    let expected = json!({"t":"audio","quiet":true,"alarm":0.4})
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(sync.refresh()?, Some(expected.clone()));
    assert_eq!(sync.refresh()?, None);
    assert_eq!(sync.resend()?, expected);
    Ok(())
}

#[test]
fn media_adapter_command_cannot_survive_generation_change() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let client = bus.connection()?;
    let art = tempfile::tempdir()?;
    let mut bridge = mpris::MediaBridge::new_at(Some(&bus.address), art.path());
    bridge.connected(1);
    bridge.handle_authenticated(1, &media(1, "org.example.old"))?;
    let old = name(1, "org.example.old");
    let player = Proxy::new(
        &client,
        old.as_str(),
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    )?;
    let _: () = player.call("Play", &())?;
    bridge.connected(2);
    assert!(bridge.drain_commands().is_empty());
    Ok(())
}
