//! `waydroidusermonitor`: Android's framework reports user unlock and package
//! changes to this service (upstream `tools/interfaces/IUserMonitor.py`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;

use deckard_launch::binder::{BAD_VALUE, Handler, ParcelRead, Reply, UNKNOWN_TRANSACTION};

pub const SERVICE: &str = "waydroidusermonitor";
pub const INTERFACE: &str = "lineageos.waydroid.IUserMonitor";

const USER_UNLOCKED: u32 = 1;
const PACKAGE_STATE_CHANGED: u32 = 2;
const PACKAGE_REMOVED: i32 = 1;
const OK: Reply = Reply::Words(&[0]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Android user `0`'s storage is unlocked; the whole catalog is current.
    UserUnlocked,
    /// One package was added, updated or removed.
    Package { package: String, removed: bool },
}

fn decode(code: u32, parcel: &mut impl ParcelRead) -> Result<Event, Reply> {
    let bad = |_| Reply::Status(BAD_VALUE);
    match code {
        USER_UNLOCKED => {
            parcel.int().map_err(bad)?;
            Ok(Event::UserUnlocked)
        }
        PACKAGE_STATE_CHANGED => {
            let mode = parcel.int().map_err(bad)?;
            let package = parcel.string().map_err(bad)?;
            parcel.int().map_err(bad)?;
            Ok(Event::Package {
                package,
                removed: mode == PACKAGE_REMOVED,
            })
        }
        _ => Err(Reply::Status(UNKNOWN_TRANSACTION)),
    }
}

/// Queues each event for `events`. When the queue is full the event is
/// dropped and `resync` is set, so the consumer rereads the whole catalog.
pub fn handler(events: SyncSender<Event>, resync: Arc<AtomicBool>) -> Box<Handler> {
    Box::new(move |code, parcel| match decode(code, parcel) {
        Ok(event) => {
            if events.try_send(event).is_err() {
                resync.store(true, Ordering::Release);
            }
            OK
        }
        Err(reply) => reply,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Values(VecDeque<&'static str>);

    impl ParcelRead for Values {
        fn int(&mut self) -> crate::Result<i32> {
            Ok(self.string()?.parse()?)
        }
        fn long(&mut self) -> crate::Result<i64> {
            Ok(self.string()?.parse()?)
        }
        fn string(&mut self) -> crate::Result<String> {
            self.0
                .pop_front()
                .map(str::to_owned)
                .ok_or_else(|| "truncated".into())
        }
    }

    fn decoded(code: u32, values: &[&'static str]) -> Result<Event, Reply> {
        decode(code, &mut Values(values.iter().copied().collect()))
    }

    #[test]
    fn transactions_match_upstream_user_monitor() {
        assert_eq!(decoded(1, &["0"]), Ok(Event::UserUnlocked));
        for (mode, removed) in [("0", false), ("1", true), ("2", false)] {
            assert_eq!(
                decoded(2, &[mode, "org.example", "10123"]),
                Ok(Event::Package {
                    package: "org.example".into(),
                    removed,
                })
            );
        }
        assert_eq!(decoded(2, &["0"]), Err(Reply::Status(BAD_VALUE)));
        assert_eq!(decoded(3, &[]), Err(Reply::Status(UNKNOWN_TRANSACTION)));
    }
}
