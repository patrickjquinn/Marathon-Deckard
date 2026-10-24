#![allow(
    clippy::unwrap_used,
    reason = "the fake Shell's lock is never poisoned in this test"
)]

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use deckard_bridge::shell_sync;
use deckard_core::runtime_state::SHELL_BUS_NAME;
use zbus::blocking::Connection;
use zbus::interface;
use zbus::zvariant::Value;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct Ambience(Arc<Mutex<String>>);

#[interface(name = "org.marathonos.Shell.Ambience1")]
impl Ambience {
    fn get_ambience(&self) -> HashMap<String, Value<'static>> {
        HashMap::from([(
            "/desktop/jolla/theme/color/highlight".into(),
            Value::from(self.0.lock().unwrap().clone()),
        )])
    }
}

fn colors(path: &Path) -> HashMap<String, String> {
    let content = fs::read_to_string(path).unwrap_or_default();
    let pairs = content
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(name, color)| (name.to_owned(), color.to_owned()));
    pairs.collect()
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() {
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn mirrors_ambience_and_doze_into_data_deckard() -> TestResult {
    if env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let highlight = Arc::new(Mutex::new(String::from("#ff7df0d4")));
    let shell = Connection::session()?;
    shell.object_server().at(
        "/org/marathonos/Shell/Ambience",
        Ambience(Arc::clone(&highlight)),
    )?;
    shell.request_name(SHELL_BUS_NAME)?;

    let palette = dir.path().join("deckard/palette");
    let bus = Connection::session()?;
    let palette_path = palette.clone();
    thread::spawn(move || shell_sync::sync_palette(&bus, &palette_path));
    wait_for("the first palette", || {
        colors(&palette)
            .get("system_accent1_100")
            .map(String::as_str)
            == Some("#ff7df0d4")
    })?;
    assert_eq!(colors(&palette).len(), 65);

    *highlight.lock().unwrap() = "#ffff8a3d".into();
    // A burst while GetAmbience is in flight never wedges the bus.
    for _ in 0..64 {
        shell.emit_signal(
            None::<&str>,
            "/org/marathonos/Shell/Ambience",
            "org.marathonos.Shell.Ambience1",
            "AmbienceChanged",
            &(HashMap::<String, Value<'static>>::new(),),
        )?;
    }
    wait_for("the new palette", || {
        colors(&palette)
            .get("system_accent1_100")
            .map(String::as_str)
            == Some("#ffff8a3d")
    })?;
    assert_eq!(colors(&palette)["system_neutral1_900"], "#ff040404");

    let doze = dir.path().join("doze");
    let bus = Connection::session()?;
    let doze_path = doze.clone();
    thread::spawn(move || shell_sync::sync_doze(&bus, &doze_path));
    for (dozing, expected) in [(true, "1\n"), (false, "0\n")] {
        wait_for("the doze state", || {
            let state = HashMap::from([("dozing", Value::from(dozing))]);
            let _ = shell.emit_signal(
                None::<&str>,
                "/org/marathonos/Shell/Power",
                "org.marathonos.Shell.Power1",
                "StateChanged",
                &(state,),
            );
            fs::read_to_string(&doze).is_ok_and(|content| content == expected)
        })?;
    }
    Ok(())
}
