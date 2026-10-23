use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

use deckard_bridge::app_channel::{AppChannel, ChannelEvent, INBOUND_CAPACITY};
use deckard_core::framing;
use deckard_core::ids::PackageName;
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn frame_object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_else(Map::new)
}

fn frame(value: Value) -> Result<Vec<u8>, deckard_core::Error> {
    framing::encode(&frame_object(value))
}

#[test]
fn stalled_consumer_keeps_one_burst_and_the_peer() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("bridge.sock");
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
    let (channel, _handle, events) = AppChannel::bind(&path, &packages, package)?;
    thread::spawn(move || channel.run());

    let mut peer = UnixStream::connect(&path)?;
    let Ok(ChannelEvent::Connected(generation)) = events.recv_timeout(Duration::from_secs(2))
    else {
        return Err("expected a connection".into());
    };
    let mut burst = Vec::new();
    for index in 0..INBOUND_CAPACITY + 50 {
        burst.extend(frame(json!({"t": "n", "i": index}))?);
    }
    peer.write_all(&burst)?;
    thread::sleep(Duration::from_millis(500));

    let queued: Vec<ChannelEvent> = events.try_iter().collect();
    assert_eq!(queued.len(), INBOUND_CAPACITY);
    assert!(
        queued
            .iter()
            .all(|event| matches!(event, ChannelEvent::Message(g, _) if *g == generation))
    );

    thread::sleep(Duration::from_millis(100));
    peer.write_all(&frame(json!({"t": "after"}))?)?;
    match events.recv_timeout(Duration::from_secs(2))? {
        ChannelEvent::Message(received, message) => {
            assert_eq!(received, generation);
            assert_eq!(message["t"], "after");
        }
        other => return Err(format!("expected the next message, got {other:?}").into()),
    }
    Ok(())
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
    first.write_all(&framing::encode(&frame_object(json!({"t":"hello","v":1})))?)?;
    match events.recv_timeout(Duration::from_secs(2))? {
        ChannelEvent::Message(received, message) => {
            assert_eq!(received, generation);
            assert_eq!(message["t"], "hello");
        }
        _ => return Err("expected framed message".into()),
    }
    handle.send_to(generation, frame_object(json!({"t":"reply","ok":true})))?;
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
    assert_eq!(handle.generation(), Some(generation));
    handle.send(frame_object(json!({"t":"still_connected"})))?;
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
    assert_eq!(handle.generation(), Some(replacement));
    assert!(
        handle
            .send_to(generation, frame_object(json!({"t":"stale"})))
            .is_err()
    );
    assert_ne!(handle.generation(), Some(generation));
    Ok(())
}
