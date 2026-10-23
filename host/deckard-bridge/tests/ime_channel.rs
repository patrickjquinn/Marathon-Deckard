// These modules are not yet exported by lib.rs; exercise them before cutover wiring.
#[allow(dead_code)]
#[path = "../src/app_channel.rs"]
mod app_channel;
#[allow(dead_code, unused_imports)]
#[path = "../src/ime.rs"]
mod ime;

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use app_channel::{AppChannel, ChannelEvent};
use deckard_core::framing;
use deckard_core::ids::PackageName;
use ime::{ImeController, ShellCall, ShellEvent, ShellSignal};
use serde_json::{Map, Value, json};
use zbus::blocking::Connection;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

#[test]
fn ime_focus_panel_replay_and_disconnect() {
    let mut controller = ImeController::default();
    assert!(
        controller
            .shell_signal(ShellSignal::PanelChanged(
                "waydroid.org.example.music".into(),
                220,
                420,
            ))
            .is_none()
    );
    let effects = controller.android_message(&object(json!({
        "t": "start", "package": "org.example.music", "purpose": 5, "hints": 7
    })));
    assert_eq!(
        effects.shell_calls,
        [ShellCall::SetEditor(
            "waydroid.org.example.music".into(),
            5,
            7
        )]
    );
    assert_eq!(
        effects.to_android,
        Some(object(json!({"t":"panel","covered":220,"height":420})))
    );
    assert!(
        controller
            .shell_signal(ShellSignal::CommitString(
                "waydroid.org.other".into(),
                "wrong".into(),
                0,
                0,
            ))
            .is_none()
    );
    assert_eq!(
        controller.shell_signal(ShellSignal::Key(
            "waydroid.org.example.music".into(),
            "enter".into(),
        )),
        Some(object(json!({"t":"key","key":"enter"})))
    );
    assert_eq!(
        controller
            .android_message(&object(json!({
                "t":"state", "text":"a😀b", "cursor":3, "anchor":1
            })))
            .shell_calls,
        [ShellCall::SetSurroundingText(
            "waydroid.org.example.music".into(),
            "a😀b".into(),
            3,
            1,
        )]
    );
    assert_eq!(
        controller.disconnected(),
        [
            ShellCall::HidePanel("waydroid.org.example.music".into()),
            ShellCall::ClearEditor("waydroid.org.example.music".into())
        ]
    );
    assert!(
        controller
            .shell_signal(ShellSignal::Key(
                "waydroid.org.example.music".into(),
                "enter".into(),
            ))
            .is_none()
    );
}

#[test]
fn app_channel_frames_and_rejects_stale_replies() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("deckard/bridge.sock");
    let packages = dir.path().join("packages.list");
    let package = PackageName::try_from("org.marathonos.deckard.bridge")?;
    fs::write(
        &packages,
        format!(
            "{} {} 0 /data/user/0\n",
            package.as_str(),
            fs::metadata(dir.path())?.uid()
        ),
    )?;
    let (channel, handle, events) = AppChannel::bind(&path, &packages, package)?;
    thread::spawn(move || channel.run());

    let mut first = UnixStream::connect(&path)?;
    first.set_read_timeout(Some(Duration::from_secs(2)))?;
    let generation = match events.recv_timeout(Duration::from_secs(2))? {
        ChannelEvent::Connected(generation) => generation,
        _ => return Err("expected first connection".into()),
    };
    first.write_all(&framing::encode(&object(json!({"t":"hello","v":1})))?)?;
    match events.recv_timeout(Duration::from_secs(2))? {
        ChannelEvent::Message(received, message) => {
            assert_eq!(received, generation);
            assert_eq!(message["t"], "hello");
        }
        _ => return Err("expected framed message".into()),
    }
    handle.send_to(generation, object(json!({"t":"reply","ok":true})))?;
    let mut header = [0_u8; 4];
    first.read_exact(&mut header)?;
    let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
    first.read_exact(&mut body)?;
    assert_eq!(serde_json::from_slice::<Value>(&body)?["t"], "reply");

    fs::write(
        &packages,
        "org.marathonos.deckard.bridge 4294967294 0 /data/user/0\n",
    )?;
    let _denied = UnixStream::connect(&path)?;
    assert!(events.recv_timeout(Duration::from_millis(150)).is_err());
    assert!(handle.is_current(generation));
    handle.send(object(json!({"t":"still_connected"})))?;
    first.read_exact(&mut header)?;
    let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
    first.read_exact(&mut body)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&body)?["t"],
        "still_connected"
    );

    fs::write(
        &packages,
        format!(
            "org.marathonos.deckard.bridge {} 0 /data/user/0\n",
            fs::metadata(dir.path())?.uid()
        ),
    )?;

    let _second = UnixStream::connect(&path)?;
    let replacement = loop {
        match events.recv_timeout(Duration::from_secs(2))? {
            ChannelEvent::Connected(new) => break new,
            ChannelEvent::Disconnected(old) if old == generation => {}
            _ => return Err("unexpected replacement event".into()),
        }
    };
    assert!(handle.is_current(replacement));
    assert!(
        handle
            .send_to(generation, object(json!({"t":"stale"})))
            .is_err()
    );
    assert!(!handle.is_current(generation));
    Ok(())
}

#[test]
fn private_bus_delivers_shell_signal() -> TestResult {
    if env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let shell = Connection::session()?;
    shell.request_name(ime::SHELL_NAME)?;
    let listener = Connection::session()?;
    let destination = listener
        .unique_name()
        .ok_or("listener has no unique name")?
        .to_string();
    let (sender, events) = mpsc::channel();
    thread::spawn(move || ime::listen_shell(listener, sender));
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2))?,
        ShellEvent::Listening
    );
    shell.emit_signal(
        Some(destination.as_str()),
        ime::TEXT_INPUT_PATH,
        ime::TEXT_INPUT_INTERFACE,
        "CommitString",
        &("waydroid.org.example", "x", 1_i32, 0_i32),
    )?;
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2))?,
        ShellEvent::Signal(ShellSignal::CommitString(
            "waydroid.org.example".into(),
            "x".into(),
            1,
            0,
        ))
    );
    Ok(())
}
