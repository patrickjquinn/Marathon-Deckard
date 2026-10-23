//! Android follows the host time zone through `persist.sys.timezone`, as
//! Marathon's earlier `marathon-waydroid-session` did with `waydroid prop set`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use deckard_launch::binder::{BinderConfig, Platform};
use zbus::blocking::{Connection, Proxy};

use crate::{Result, invalid};

const PROPERTY: &str = "persist.sys.timezone";
const MAX_VALUE_BYTES: usize = 91;

/// An IANA zone name that fits an Android property value.
fn valid(zone: &str) -> bool {
    !zone.is_empty()
        && zone.len() <= MAX_VALUE_BYTES
        && !zone.starts_with('/')
        && !zone.contains("..")
        && zone
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'+'))
}

fn apply(config: &BinderConfig, zone: &str) -> Result<()> {
    if !valid(zone) {
        return Err(invalid(format!("unusable host time zone {zone:?}")));
    }
    Platform::connect(config)?.setprop(PROPERTY, zone)
}

fn follow(config: &BinderConfig, stop: &AtomicBool) -> Result<()> {
    let bus = Connection::system()?;
    let timedate = Proxy::new(
        &bus,
        "org.freedesktop.timedate1",
        "/org/freedesktop/timedate1",
        "org.freedesktop.timedate1",
    )?;
    let changes = timedate.receive_property_changed::<String>("Timezone");
    apply(config, &timedate.get_property::<String>("Timezone")?)?;
    for change in changes {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = change
            .get()
            .map_err(crate::Error::from)
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
