//! Marathon state mirrored into Android's `/data/deckard`, where its
//! deckard-watch service applies it: the ambience palette and Doze.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::process;

use deckard_core::palette::{self, Color, Theme};
use deckard_core::runtime_state::SHELL_BUS_NAME;
use futures_lite::{StreamExt, future};
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::OwnedValue;
use zbus::{MatchRule, MessageStream, message};

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
    let rule = MatchRule::builder()
        .msg_type(message::Type::Signal)
        .sender(SHELL_BUS_NAME)?
        .path(POWER_PATH)?
        .interface(POWER_INTERFACE)?
        .member("StateChanged")?
        .build();
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

async fn refresh_palette(bus: &zbus::Connection, path: &Path) {
    let reply = bus
        .call_method(
            Some(SHELL_BUS_NAME),
            AMBIENCE_PATH,
            Some(AMBIENCE_INTERFACE),
            "GetAmbience",
            &(),
        )
        .await;
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
/// Changes during a `GetAmbience` fold into one more read, and the signal
/// queues keep draining meanwhile, so a burst never wedges the connection.
///
/// # Errors
/// Returns the D-Bus error that prevented the subscription.
pub fn sync_palette(bus: &zbus::Connection, path: &Path) -> zbus::Result<()> {
    future::block_on(async {
        let ambience = MatchRule::builder()
            .msg_type(message::Type::Signal)
            .sender(SHELL_BUS_NAME)?
            .path(AMBIENCE_PATH)?
            .interface(AMBIENCE_INTERFACE)?
            .member("AmbienceChanged")?
            .build();
        let owner = MatchRule::builder()
            .msg_type(message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .add_arg(SHELL_BUS_NAME)?
            .build();
        let ambience = MessageStream::for_match_rule(ambience, bus, Some(8)).await?;
        let owner = MessageStream::for_match_rule(owner, bus, Some(8)).await?;
        let mut triggers = ambience.or(owner);
        let (stale, refreshes) = async_channel::bounded(1);
        let follow = async move {
            while let Some(trigger) = triggers.next().await {
                if trigger.is_ok() {
                    let _ = stale.try_send(());
                }
            }
        };
        let refresh = async {
            refresh_palette(bus, path).await;
            while refreshes.recv().await.is_ok() {
                refresh_palette(bus, path).await;
            }
        };
        future::zip(follow, refresh).await;
        Ok(())
    })
}
