//! `waydroidusermonitor`: Android's framework reports user unlock and package
//! changes to this service (upstream `tools/interfaces/IUserMonitor.py`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;

use deckard_launch::binder::{self, Reply, Service};
use rsbinder::{Parcel, StatusCode};

const USER_UNLOCKED: u32 = 1;
const PACKAGE_STATE_CHANGED: u32 = 2;
const PACKAGE_REMOVED: i32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Android user `0`'s storage is unlocked; the whole catalog is current.
    UserUnlocked,
    /// One package was added, updated or removed.
    Package { package: String, removed: bool },
}

fn decode(code: u32, parcel: &mut Parcel) -> Result<Event, Reply> {
    let bad = |_| Reply::Status(StatusCode::BadValue);
    match code {
        USER_UNLOCKED => {
            parcel.read::<i32>().map_err(bad)?;
            Ok(Event::UserUnlocked)
        }
        PACKAGE_STATE_CHANGED => {
            let mode: i32 = parcel.read().map_err(bad)?;
            let package =
                binder::string(parcel).map_err(|_| Reply::Status(StatusCode::BadValue))?;
            parcel.read::<i32>().map_err(bad)?;
            Ok(Event::Package {
                package,
                removed: mode == PACKAGE_REMOVED,
            })
        }
        _ => Err(Reply::Status(StatusCode::UnknownTransaction)),
    }
}

/// Queues each event; when the queue is full the event is dropped and
/// `resync` set, so the consumer rereads the whole catalog.
pub struct Monitor {
    pub events: SyncSender<Event>,
    pub resync: Arc<AtomicBool>,
}

impl Service for Monitor {
    const NAME: &'static str = "waydroidusermonitor";
    const INTERFACE: &'static str = "lineageos.waydroid.IUserMonitor";

    fn transact(&self, code: u32, request: &mut Parcel) -> Reply {
        match decode(code, request) {
            Ok(event) => {
                if self.events.try_send(event).is_err() {
                    self.resync.store(true, Ordering::Release);
                }
                Reply::Words(&[0])
            }
            Err(reply) => reply,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(
        code: u32,
        mode: Option<i32>,
        package: Option<&str>,
    ) -> rsbinder::Result<Result<Event, Reply>> {
        let mut parcel = Parcel::new();
        if let Some(mode) = mode {
            parcel.write(&mode)?;
        }
        if let Some(package) = package {
            parcel.write(package)?;
            parcel.write(&10123i32)?;
        }
        parcel.set_data_position(0);
        Ok(decode(code, &mut parcel))
    }

    #[test]
    fn transactions_match_upstream_user_monitor() -> rsbinder::Result<()> {
        assert_eq!(decoded(1, Some(0), None)?, Ok(Event::UserUnlocked));
        for (mode, removed) in [(0, false), (1, true), (2, false)] {
            assert_eq!(
                decoded(2, Some(mode), Some("org.example"))?,
                Ok(Event::Package {
                    package: "org.example".into(),
                    removed,
                })
            );
        }
        let bad = Err(Reply::Status(StatusCode::BadValue));
        assert_eq!(decoded(2, Some(0), None)?, bad);
        let unknown = Err(Reply::Status(StatusCode::UnknownTransaction));
        assert_eq!(decoded(3, None, None)?, unknown);
        Ok(())
    }
}
