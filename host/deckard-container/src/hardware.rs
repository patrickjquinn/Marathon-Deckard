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
use std::sync::mpsc::SyncSender;

use deckard_launch::binder::{self, BinderConfig, Reply, Service};
use rsbinder::{Parcel, StatusCode};

use crate::{Error, Result};

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

/// `(system_zip, system_time, vendor_zip, vendor_time)`; the times are int32
/// for `upgrade` and int64 for `upgrade2`.
fn read_upgrade(parcel: &mut Parcel, code: u32) -> deckard_launch::Result<()> {
    for _ in 0..2 {
        binder::string(parcel)?;
        if code == UPGRADE2 {
            parcel.read::<i64>()?;
        } else {
            parcel.read::<i32>()?;
        }
    }
    Ok(())
}

fn admit(code: u32, parcel: &mut Parcel) -> Admission {
    let bad = Admission::Answer(Reply::Status(StatusCode::BadValue));
    match code {
        ENABLE_NFC | ENABLE_BLUETOOTH => match parcel.read::<i32>() {
            Ok(_) => Admission::Answer(Reply::Words(&[0, 0])),
            Err(_) => bad,
        },
        SUSPEND => Admission::Queue(HardwareRequest::Suspend),
        REBOOT => Admission::Queue(HardwareRequest::Reboot),
        UPGRADE | UPGRADE2 => match read_upgrade(parcel, code) {
            Ok(()) => {
                eprintln!("deckard-containerd: refused Android image upgrade request");
                Admission::Answer(Reply::Status(StatusCode::InvalidOperation))
            }
            Err(_) => bad,
        },
        SHUTDOWN_REQUEST => match binder::string(parcel) {
            Ok(reason) => Admission::Queue(HardwareRequest::ShutdownRequest(reason)),
            Err(_) => bad,
        },
        _ => Admission::Answer(Reply::Status(StatusCode::UnknownTransaction)),
    }
}

struct Hardware(SyncSender<HardwareRequest>);

impl Service for Hardware {
    const NAME: &'static str = "waydroidhardware";
    const INTERFACE: &'static str = "lineageos.waydroid.IHardware";

    fn transact(&self, code: u32, request: &mut Parcel) -> Reply {
        match admit(code, request) {
            Admission::Answer(reply) => reply,
            Admission::Queue(request) => match self.0.try_send(request) {
                Ok(()) => Reply::Words(&[0]),
                Err(_) => Reply::Status(StatusCode::WouldBlock),
            },
        }
    }
}

/// Registers `waydroidhardware` and serves it until `stop` is set. Admitted
/// requests go to `requests`; success replies acknowledge admission, not
/// completion.
///
/// # Errors
/// The Binder device cannot be opened.
pub fn run(
    config: &BinderConfig,
    requests: SyncSender<HardwareRequest>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    binder::serve(config, Hardware(requests), stop)
        .map_err(|error| Error::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Value {
        Int(i32),
        Long(i64),
        Text(&'static str),
    }

    fn admitted(code: u32, values: &[Value]) -> rsbinder::Result<Admission> {
        let mut parcel = Parcel::new();
        for value in values {
            match value {
                Value::Int(value) => parcel.write(value)?,
                Value::Long(value) => parcel.write(value)?,
                Value::Text(value) => parcel.write(*value)?,
            }
        }
        parcel.set_data_position(0);
        Ok(admit(code, &mut parcel))
    }

    #[test]
    fn radio_toggles_are_upstream_no_ops_that_answer_false() -> rsbinder::Result<()> {
        for code in [ENABLE_NFC, ENABLE_BLUETOOTH] {
            let answer = Admission::Answer(Reply::Words(&[0, 0]));
            assert_eq!(admitted(code, &[Value::Int(1)])?, answer);
            let bad = Admission::Answer(Reply::Status(StatusCode::BadValue));
            assert_eq!(admitted(code, &[])?, bad);
        }
        Ok(())
    }

    #[test]
    fn lifecycle_requests_are_queued() -> rsbinder::Result<()> {
        assert_eq!(
            admitted(SUSPEND, &[])?,
            Admission::Queue(HardwareRequest::Suspend)
        );
        assert_eq!(
            admitted(REBOOT, &[])?,
            Admission::Queue(HardwareRequest::Reboot)
        );
        assert_eq!(
            admitted(SHUTDOWN_REQUEST, &[Value::Text("1reboot")])?,
            Admission::Queue(HardwareRequest::ShutdownRequest("1reboot".into()))
        );
        Ok(())
    }

    #[test]
    fn upgrades_are_refused_and_never_queued() -> rsbinder::Result<()> {
        let refused = Admission::Answer(Reply::Status(StatusCode::InvalidOperation));
        let upgrade = [
            Value::Text("/s.zip"),
            Value::Int(7),
            Value::Text("/v.zip"),
            Value::Int(8),
        ];
        assert_eq!(admitted(UPGRADE, &upgrade)?, refused);
        let upgrade2 = [
            Value::Text("/s.zip"),
            Value::Long(1_790_477_549),
            Value::Text("/v.zip"),
            Value::Long(1_790_486_210),
        ];
        assert_eq!(admitted(UPGRADE2, &upgrade2)?, refused);
        let short = Admission::Answer(Reply::Status(StatusCode::BadValue));
        assert_eq!(admitted(UPGRADE, &upgrade[..2])?, short);
        let unknown = Admission::Answer(Reply::Status(StatusCode::UnknownTransaction));
        assert_eq!(admitted(99, &[])?, unknown);
        Ok(())
    }
}
