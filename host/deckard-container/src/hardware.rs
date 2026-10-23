//! `waydroidhardware` (`lineageos.waydroid.IHardware`), which Android calls to
//! suspend, reboot or shut down its container (upstream
//! `tools/services/hardware_manager.py`).
//!
//! - `enableNFC` and `enableBluetooth` are documented no-ops upstream ("not
//!   implemented"); Deckard answers `false` and touches no host radio.
//! - `upgrade` replaces the images with zips Android downloaded from Waydroid's
//!   OTA channels. Marathon ships and updates the images itself and disables
//!   Android's updater, so both upgrade codes are refused with
//!   `INVALID_OPERATION`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{SyncSender, TrySendError};

use deckard_launch::binder::{
    self, BAD_VALUE, BinderConfig, INVALID_OPERATION, ParcelRead, Reply, UNKNOWN_TRANSACTION,
    WOULD_BLOCK,
};

use crate::{Error, Result};

pub const SERVICE: &str = "waydroidhardware";
pub const INTERFACE: &str = "lineageos.waydroid.IHardware";
/// Requests waiting for the daemon; Android gets `WOULD_BLOCK` beyond this.
pub const QUEUE_DEPTH: usize = 16;

const ENABLE_NFC: u32 = 1;
const ENABLE_BLUETOOTH: u32 = 2;
const SUSPEND: u32 = 3;
const REBOOT: u32 = 4;
const UPGRADE: u32 = 5;
const UPGRADE2: u32 = 6;
const SHUTDOWN_REQUEST: u32 = 7;

/// A request the daemon acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareRequest {
    Suspend,
    Reboot,
    /// A reason starting with `1` asks for a reboot.
    ShutdownRequest(String),
}

#[derive(Debug, PartialEq, Eq)]
enum Admission {
    Answer(Reply),
    Queue(HardwareRequest),
}

/// `(system_zip, system_time, vendor_zip, vendor_time)`, the times being
/// int32 for `upgrade` and int64 for `upgrade2`.
fn read_upgrade(parcel: &mut impl ParcelRead, wide: bool) -> deckard_launch::Result<()> {
    for _ in 0..2 {
        parcel.string()?;
        if wide {
            parcel.long()?;
        } else {
            parcel.int()?;
        }
    }
    Ok(())
}

fn admit(code: u32, parcel: &mut impl ParcelRead) -> Admission {
    let bad = || Admission::Answer(Reply::Status(BAD_VALUE));
    match code {
        ENABLE_NFC | ENABLE_BLUETOOTH => match parcel.int() {
            Ok(_) => Admission::Answer(Reply::Words(&[0, 0])),
            Err(_) => bad(),
        },
        SUSPEND => Admission::Queue(HardwareRequest::Suspend),
        REBOOT => Admission::Queue(HardwareRequest::Reboot),
        UPGRADE | UPGRADE2 => match read_upgrade(parcel, code == UPGRADE2) {
            Ok(()) => {
                eprintln!("deckard-containerd: refused Android image upgrade request");
                Admission::Answer(Reply::Status(INVALID_OPERATION))
            }
            Err(_) => bad(),
        },
        SHUTDOWN_REQUEST => match parcel.string() {
            Ok(reason) => Admission::Queue(HardwareRequest::ShutdownRequest(reason)),
            Err(_) => bad(),
        },
        _ => Admission::Answer(Reply::Status(UNKNOWN_TRANSACTION)),
    }
}

/// Registers `waydroidhardware` and serves it on the calling thread until
/// `stop` is set. Admitted requests go to `requests`; success replies
/// acknowledge admission, not completion.
///
/// # Errors
/// libgbinder or the service manager is unavailable.
pub fn run(
    config: &BinderConfig,
    requests: SyncSender<HardwareRequest>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let handler =
        Box::new(
            move |code, parcel: &mut binder::Parcel<'_>| match admit(code, parcel) {
                Admission::Answer(reply) => reply,
                Admission::Queue(request) => match requests.try_send(request) {
                    Ok(()) => Reply::Words(&[0]),
                    Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                        Reply::Status(WOULD_BLOCK)
                    }
                },
            },
        );
    binder::serve(config, SERVICE, INTERFACE, handler, stop)
        .map_err(|error| Error::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Values(VecDeque<&'static str>);

    impl ParcelRead for Values {
        fn int(&mut self) -> deckard_launch::Result<i32> {
            Ok(self.string()?.parse()?)
        }
        fn long(&mut self) -> deckard_launch::Result<i64> {
            Ok(self.string()?.parse()?)
        }
        fn string(&mut self) -> deckard_launch::Result<String> {
            self.0
                .pop_front()
                .map(str::to_owned)
                .ok_or_else(|| "truncated".into())
        }
    }

    fn admitted(code: u32, values: &[&'static str]) -> Admission {
        admit(code, &mut Values(values.iter().copied().collect()))
    }

    #[test]
    fn radio_toggles_are_upstream_no_ops_that_answer_false() {
        for code in [ENABLE_NFC, ENABLE_BLUETOOTH] {
            for enable in ["0", "1"] {
                assert_eq!(
                    admitted(code, &[enable]),
                    Admission::Answer(Reply::Words(&[0, 0]))
                );
            }
            assert_eq!(
                admitted(code, &[]),
                Admission::Answer(Reply::Status(BAD_VALUE))
            );
        }
    }

    #[test]
    fn lifecycle_requests_are_queued() {
        assert_eq!(
            admitted(SUSPEND, &[]),
            Admission::Queue(HardwareRequest::Suspend)
        );
        assert_eq!(
            admitted(REBOOT, &[]),
            Admission::Queue(HardwareRequest::Reboot)
        );
        assert_eq!(
            admitted(SHUTDOWN_REQUEST, &["1reboot"]),
            Admission::Queue(HardwareRequest::ShutdownRequest("1reboot".into()))
        );
        assert_eq!(
            admitted(SHUTDOWN_REQUEST, &[]),
            Admission::Answer(Reply::Status(BAD_VALUE))
        );
    }

    #[test]
    fn upgrades_are_refused_and_never_queued() {
        let refused = Admission::Answer(Reply::Status(INVALID_OPERATION));
        assert_eq!(
            admitted(UPGRADE, &["/data/system.zip", "7", "/data/vendor.zip", "8"]),
            refused
        );
        assert_eq!(
            admitted(UPGRADE2, &["/s.zip", "1790477549", "/v.zip", "1790486210"]),
            refused
        );
        assert_eq!(
            admitted(UPGRADE, &["/s.zip", "7"]),
            Admission::Answer(Reply::Status(BAD_VALUE))
        );
        assert_eq!(
            admitted(99, &[]),
            Admission::Answer(Reply::Status(UNKNOWN_TRANSACTION))
        );
    }
}
