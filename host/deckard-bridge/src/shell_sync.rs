//! Marathon state mirrored into Android's `/data/deckard`, where its
//! deckard-watch service applies it: the ambience palette and Doze.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::process;

use deckard_core::palette::{self, Color, Theme};
use deckard_core::runtime_state::SHELL_BUS_NAME;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::OwnedValue;

const POWER_PATH: &str = "/org/marathonos/Shell/Power";
const POWER_INTERFACE: &str = "org.marathonos.Shell.Power1";
const AMBIENCE_PATH: &str = "/org/marathonos/Shell/Ambience";
const AMBIENCE_INTERFACE: &str = "org.marathonos.Shell.Ambience1";
const HIGHLIGHT: &str = "/desktop/jolla/theme/color/highlight";

type State = HashMap<String, OwnedValue>;

/// Replaces `path` by a rename, which deckard-watch sees as one `moved_to`.
/// Returns whether the content changed.
fn replace(path: &Path, content: &str) -> io::Result<bool> {
    if fs::read(path).is_ok_and(|current| current == content.as_bytes()) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", process::id()));
    fs::write(&temporary, content)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
    Ok(true)
}

fn write(path: &Path, content: &str, done: &str) {
    match replace(path, content) {
        Ok(true) => eprintln!("deckard-bridge: {done}"),
        Ok(false) => {}
        Err(error) => eprintln!("deckard-bridge: cannot write {}: {error}", path.display()),
    }
}

/// Writes `1` or `0` to `path` as Power1 reports Marathon entering or
/// leaving Doze; deckard-watch forces or releases Android's deep idle.
///
/// # Errors
/// Returns the D-Bus error that prevented the subscription.
pub fn sync_doze(bus: &Connection, path: &Path) -> zbus::Result<()> {
    let rule = crate::signals(SHELL_BUS_NAME, POWER_PATH, POWER_INTERFACE)?;
    let rule = rule.member("StateChanged")?.build();
    let signals = MessageIterator::for_match_rule(rule, bus, Some(8))?;
    for signal in signals {
        let Ok(signal) = signal else { continue };
        let Ok(state) = signal.body().deserialize::<State>() else {
            continue;
        };
        match state.get("dozing").map(bool::try_from) {
            Some(Ok(true)) => write(path, "1\n", "Android idles with Marathon"),
            Some(Ok(false)) => write(path, "0\n", "Android wakes with Marathon"),
            _ => {}
        }
    }
    Ok(())
}

fn refresh_palette(bus: &Connection, path: &Path) {
    let reply = bus.call_method(
        Some(SHELL_BUS_NAME),
        AMBIENCE_PATH,
        Some(AMBIENCE_INTERFACE),
        "GetAmbience",
        &(),
    );
    let ambience = match reply.and_then(|reply| reply.body().deserialize::<State>()) {
        Ok(ambience) => ambience,
        Err(error) => {
            eprintln!("deckard-bridge: cannot read the ambience: {error}");
            return;
        }
    };
    let Some(highlight) = ambience
        .get(HIGHLIGHT)
        .and_then(|value| <&str>::try_from(value).ok())
        .and_then(|value| Color::parse(value).ok())
    else {
        return;
    };
    let content = palette::runtime_file(highlight, Theme::default());
    let done = format!("palette for highlight {}", highlight.hex());
    write(path, &content, &done);
}

/// Writes Android's 65 system colours for the current ambience highlight to
/// `path`, again whenever the ambience or the Shell's bus owner changes.
///
/// # Errors
/// Returns the D-Bus error that prevented the subscription.
pub fn sync_palette(bus: &Connection, path: &Path) -> zbus::Result<()> {
    let ambience = crate::signals(SHELL_BUS_NAME, AMBIENCE_PATH, AMBIENCE_INTERFACE)?;
    let rules: Vec<(MatchRule<'static>, crate::Relevant)> = vec![
        (ambience.member("AmbienceChanged")?.build(), |_| true),
        (crate::owner_changes(SHELL_BUS_NAME)?, |_| true),
    ];
    let (bus, path) = (bus.clone(), path.to_owned());
    crate::follow(&bus.clone(), rules, move || refresh_palette(&bus, &path))?;
    Ok(())
}
