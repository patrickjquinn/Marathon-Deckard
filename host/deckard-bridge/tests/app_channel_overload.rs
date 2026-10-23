use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

use deckard_bridge::app_channel::{AppChannel, ChannelEvent, INBOUND_CAPACITY};
use deckard_core::ids::PackageName;
use deckard_core::transport;
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn frame(value: Value) -> Result<Vec<u8>, deckard_core::Error> {
    transport::encode(&value.as_object().cloned().unwrap_or_else(Map::new))
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
