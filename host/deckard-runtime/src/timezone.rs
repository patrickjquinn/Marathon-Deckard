//! Android follows the host time zone through `persist.sys.timezone`, as
//! Marathon's earlier `marathon-waydroid-session` did with `waydroid prop set`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use deckard_launch::binder::{BinderConfig, Platform};
use zbus::blocking::Connection;

use crate::Result;

const PROPERTY: &str = "persist.sys.timezone";
const MAX_VALUE_BYTES: usize = 91;

#[zbus::proxy(
    interface = "org.freedesktop.timedate1",
    default_service = "org.freedesktop.timedate1",
    default_path = "/org/freedesktop/timedate1"
)]
trait Timedate {
    #[zbus(property)]
    fn timezone(&self) -> zbus::Result<String>;
}

/// An IANA zone name that fits an Android property value.
fn valid(zone: &str) -> bool {
    let plain = zone
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'+'));
    !zone.is_empty()
        && zone.len() <= MAX_VALUE_BYTES
        && !zone.starts_with('/')
        && !zone.contains("..")
        && plain
}

fn apply(config: &BinderConfig, zone: &str) -> Result<()> {
    if !valid(zone) {
        return Err(format!("unusable host time zone {zone:?}").into());
    }
    Platform::connect(config)?.setprop(PROPERTY, zone)
}

fn follow(config: &BinderConfig, stop: &AtomicBool) -> Result<()> {
    let timedate = TimedateProxyBlocking::new(&Connection::system()?)?;
    let changes = timedate.receive_timezone_changed();
    apply(config, &timedate.timezone()?)?;
    for change in changes {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = change
            .get()
            .map_err(Into::into)
            .and_then(|zone| apply(config, &zone))
        {
            eprintln!("deckard-runtime: time zone update failed: {error}");
        }
    }
    Ok(())
}

/// Sets Android's zone to the host's now and whenever timedated changes it.
/// Call once Android has booted.
pub fn propagate(config: &BinderConfig, stop: &Arc<AtomicBool>) {
    let config = config.clone();
    let stop = Arc::clone(stop);
    thread::spawn(move || {
        if let Err(error) = follow(&config, &stop) {
            eprintln!("deckard-runtime: time zone propagation failed: {error}");
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_iana_names_and_rejects_property_abuse() {
        for zone in [
            "Europe/Dublin",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+5",
            "UTC",
        ] {
            assert!(valid(zone), "{zone}");
        }
        for zone in [
            "",
            "/etc/passwd",
            "../x",
            "Europe/Dublin\nro.adb.secure=0",
            "a b",
        ] {
            assert!(!valid(zone), "{zone:?}");
        }
        assert!(!valid(&"A".repeat(92)));
    }
}
