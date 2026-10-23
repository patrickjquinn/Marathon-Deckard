#![allow(clippy::unwrap_used, reason = "fixed JSON fixtures")]

#[allow(dead_code)]
#[path = "../src/app_channel.rs"]
mod app_channel;
#[allow(dead_code)]
#[path = "../src/audio.rs"]
mod audio;

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::json;
use zbus::blocking::{Connection, connection::Builder};
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
