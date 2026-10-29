use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use deckard_bridge::app_channel::{AppChannel, ChannelEvent};
use deckard_bridge::audio::{self, AudioState};
use deckard_core::ids::PackageName;
use serde_json::{Value, json};
use zbus::connection::Builder;
use zbus::{OwnedVariant as OwnedValue, Str};

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const SETTINGS_PATH: &str = "/org/marathonos/Shell/Settings";
const SETTINGS_INTERFACE: &str = "org.marathonos.Shell.Settings1";

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
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

struct ShellSettings {
    state: HashMap<String, OwnedValue>,
}

#[zbus::interface(name = "org.marathonos.Shell.Settings1")]
impl ShellSettings {
    fn get_state(&self) -> HashMap<String, OwnedValue> {
        self.state.clone()
    }
}

fn read(stream: &mut UnixStream) -> TestResult<Value> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header)?;
    let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

#[test]
fn audio_state_matches_cpp_policy_and_clamps_volume() {
    let mut state = HashMap::new();
    let audible = AudioState {
        quiet: false,
        alarm: 0.9,
    };
    assert_eq!(AudioState::from_shell(&state), audible);
    state.insert("audioProfile".into(), Str::from("vibrate").into());
    state.insert("alarmVolume".into(), OwnedValue::from(1.7_f64));
    let loud_quiet = AudioState {
        quiet: true,
        alarm: 1.0,
    };
    assert_eq!(AudioState::from_shell(&state), loud_quiet);
    state.insert("audioProfile".into(), Str::from("normal").into());
    state.insert("dndEnabled".into(), OwnedValue::from(true));
    state.insert("alarmVolume".into(), OwnedValue::from(f64::NAN));
    let dnd = AudioState {
        quiet: true,
        alarm: 0.9,
    };
    assert_eq!(AudioState::from_shell(&state), dnd);
}

#[test]
fn policy_reaches_android_on_connect_and_on_shell_changes_only() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let state = HashMap::from([
        ("audioProfile".to_owned(), Str::from("silent").into()),
        ("alarmVolume".to_owned(), OwnedValue::from(0.4_f64)),
    ]);
    let shell = zbus::block_on(
        Builder::address(bus.address.as_str())
            .name("org.marathonos.Shell")
            .serve_at(SETTINGS_PATH, ShellSettings { state })
            .build(),
    )?;

    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("bridge.sock");
    let packages = dir.path().join("packages.list");
    let uid = fs::metadata(dir.path())?.uid();
    fs::write(
        &packages,
        format!("org.marathonos.deckard.bridge {uid} 0 /data\n"),
    )?;
    let package = PackageName::try_from("org.marathonos.deckard.bridge")?;
    let (channel, handle, events) = AppChannel::bind(&socket, &packages, package)?;
    thread::spawn(move || channel.run());
    let wake = audio::spawn(
        zbus::block_on(Builder::address(bus.address.as_str()).build())?,
        handle,
    )?;

    let mut android = UnixStream::connect(&socket)?;
    android.set_read_timeout(Some(Duration::from_secs(2)))?;
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(2))?,
        ChannelEvent::Connected(_)
    ));
    assert!(wake.wake());
    assert_eq!(
        read(&mut android)?,
        json!({"t":"audio","quiet":true,"alarm":0.4})
    );

    assert!(wake.wake());
    android.set_read_timeout(Some(Duration::from_millis(300)))?;
    assert!(read(&mut android).is_err());

    let settings = zbus::block_on(
        shell
            .object_server()
            .interface::<_, ShellSettings>(SETTINGS_PATH),
    )?;
    zbus::block_on(settings.get_mut())
        .state
        .insert("audioProfile".into(), Str::from("normal").into());
    zbus::block_on(shell.emit_signal(
        None::<&str>,
        SETTINGS_PATH,
        SETTINGS_INTERFACE,
        "PropertyChanged",
        &("audioProfile", zbus::Variant::from("normal")),
    ))?;
    android.set_read_timeout(Some(Duration::from_secs(2)))?;
    assert_eq!(
        read(&mut android)?,
        json!({"t":"audio","quiet":false,"alarm":0.4})
    );
    Ok(())
}
