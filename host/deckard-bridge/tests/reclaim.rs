use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use deckard_bridge::reclaim::Reclaimer;
use deckard_core::pressure;
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn events(dir: &Path, high: u64, max: u64) -> TestResult {
    let content = format!("low 0\nhigh {high}\nmax {max}\noom 0\noom_kill 0\noom_group_kill 0\n");
    fs::write(dir.join("memory.events"), content)?;
    Ok(())
}

#[test]
fn asks_when_the_container_goes_over_budget() -> TestResult {
    let dir = tempfile::tempdir()?;
    events(dir.path(), 5, 0)?;
    let (sent, requests) = mpsc::channel::<Map<String, Value>>();
    let reclaimer = Reclaimer::new(dir.path().into(), dir.path().join("none"), move |message| {
        sent.send(message).is_ok()
    });

    events(dir.path(), 6, 0)?;
    assert!(requests.recv_timeout(Duration::from_millis(200)).is_err());

    reclaimer.connected();
    events(dir.path(), 6, 0)?;
    assert!(requests.recv_timeout(Duration::from_millis(200)).is_err());

    events(dir.path(), 9, 0)?;
    let request = requests.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(request["t"], "reclaim");
    assert_eq!(request["count"], pressure::COUNT);
    assert_eq!(request["reason"], "container-high");

    reclaimer.handle(
        json!({"t": "reclaimed", "ok": true, "killed": []})
            .as_object()
            .ok_or("object")?,
    );
    reclaimer.disconnected();
    events(dir.path(), 20, 1)?;
    assert!(requests.recv_timeout(Duration::from_millis(200)).is_err());
    Ok(())
}
