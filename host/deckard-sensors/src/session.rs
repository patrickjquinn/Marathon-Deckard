//! One HAL connection: the sensor list, then a thread per active sensor
//! that reads it at the HAL's rate and sends its events.

use std::io;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::net::{RecvFlags, SendFlags, recv, send};
use rustix::time::{ClockId, clock_gettime};

use crate::iio::Sensor;
use crate::wire::{self, Message, Request};

const MIN_PERIOD: Duration = Duration::from_millis(10);
const MAX_PERIOD: Duration = Duration::from_secs(1);
/// Until the HAL batches: Android's `SENSOR_DELAY_NORMAL`.
const DEFAULT_PERIOD: Duration = Duration::from_millis(200);
/// How often on-change sensors are read for a change.
const ON_CHANGE_PERIOD: Duration = Duration::from_millis(200);
/// A rough figure for Android's sensor list, in mA.
const POWER_MA: f32 = 0.5;

const FLAG_WAKE_UP: u32 = 1;
const FLAG_ON_CHANGE: u32 = 2;

#[allow(
    clippy::cast_possible_truncation,
    reason = "the periods are constants under a second"
)]
fn micros(duration: Duration) -> i32 {
    duration.as_micros() as i32
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "periods are clamped to a second"
)]
fn period_ns(duration: Duration) -> u64 {
    duration.as_nanos() as u64
}

fn boottime_ns() -> i64 {
    let now = clock_gettime(ClockId::Boottime);
    now.tv_sec * 1_000_000_000 + now.tv_nsec
}

fn handle(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

fn hello(socket: &OwnedFd, sensors: &[Sensor]) -> io::Result<()> {
    for (index, sensor) in sensors.iter().enumerate() {
        let on_change = sensor.kind.on_change();
        let mut flags = 0;
        if sensor.kind.wake_up() {
            flags |= FLAG_WAKE_UP;
        }
        let (min_delay, max_delay) = if on_change {
            flags |= FLAG_ON_CHANGE;
            (0, 0)
        } else {
            (micros(MIN_PERIOD), micros(MAX_PERIOD))
        };
        let message = Message::new(wire::SENSOR)
            .u32(handle(index))
            .u32(sensor.kind.android_type())
            .u32(flags)
            .f32(sensor.max_range)
            .f32(sensor.resolution)
            .f32(POWER_MA)
            .i32(min_delay)
            .i32(max_delay)
            .name(&sensor.name);
        send(socket, message.bytes(), SendFlags::NOSIGNAL)?;
    }
    send(
        socket,
        Message::new(wire::SENSORS_END).bytes(),
        SendFlags::NOSIGNAL,
    )?;
    Ok(())
}

struct Poller {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Poller {
    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.thread().unpark();
        let _ = self.thread.join();
    }
}

struct Slot {
    period_ns: Arc<AtomicU64>,
    poller: Option<Poller>,
}

/// Reads `sensors[index]` and sends its events until `stop` or the HAL
/// goes away.
fn poll(
    socket: &OwnedFd,
    sensors: &[Sensor],
    index: usize,
    period_ns: &AtomicU64,
    stop: &AtomicBool,
) {
    let Some(sensor) = sensors.get(index) else {
        return;
    };
    let on_change = sensor.kind.on_change();
    let mut last = None;
    let mut failing = false;
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let before = boottime_ns();
        match sensor.read() {
            Ok(sample) if !on_change || last != Some(sample) => {
                // A read can take tens of ms (the magnetometer measures on
                // demand), so the sample is stamped at its middle.
                let timestamp = before + (boottime_ns() - before) / 2;
                let event = Message::new(wire::EVENT)
                    .u32(handle(index))
                    .i64(timestamp)
                    .f32(sample[0])
                    .f32(sample[1])
                    .f32(sample[2]);
                if send(socket, event.bytes(), SendFlags::NOSIGNAL).is_err() {
                    return;
                }
                last = Some(sample);
                failing = false;
            }
            Ok(_) => failing = false,
            Err(failure) => {
                if !failing {
                    eprintln!("deckard-sensors: {}: {failure}", sensor.name);
                }
                failing = true;
            }
        }
        next += if on_change {
            ON_CHANGE_PERIOD
        } else {
            Duration::from_nanos(period_ns.load(Ordering::Relaxed))
        };
        next = next.max(Instant::now());
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            if now >= next {
                break;
            }
            thread::park_timeout(next - now);
        }
    }
}

fn activate(
    socket: &Arc<OwnedFd>,
    sensors: &Arc<Vec<Sensor>>,
    slots: &mut [Slot],
    index: usize,
    enabled: bool,
) {
    let Some(slot) = slots.get_mut(index) else {
        return;
    };
    match (enabled, slot.poller.take()) {
        (true, None) => {
            let stop = Arc::new(AtomicBool::new(false));
            let (socket, sensors, period_ns, flag) = (
                Arc::clone(socket),
                Arc::clone(sensors),
                Arc::clone(&slot.period_ns),
                Arc::clone(&stop),
            );
            let thread = thread::spawn(move || poll(&socket, &sensors, index, &period_ns, &flag));
            slot.poller = Some(Poller { stop, thread });
        }
        (true, running) => slot.poller = running,
        (false, Some(poller)) => poller.stop(),
        (false, None) => {}
    }
}

fn batch(slots: &[Slot], index: usize, period: i64) {
    let Some(slot) = slots.get(index) else {
        return;
    };
    let period =
        Duration::from_nanos(u64::try_from(period).unwrap_or(0)).clamp(MIN_PERIOD, MAX_PERIOD);
    slot.period_ns.store(period_ns(period), Ordering::Relaxed);
    if let Some(poller) = &slot.poller {
        poller.thread.thread().unpark();
    }
}

fn index(handle: u32) -> usize {
    usize::try_from(handle).map_or(usize::MAX, |handle| handle.wrapping_sub(1))
}

/// Serves `socket` until the HAL closes it.
///
/// # Errors
/// The socket failed or the HAL sent something that is not a request.
pub fn serve(socket: OwnedFd, sensors: &Arc<Vec<Sensor>>) -> io::Result<()> {
    let socket = Arc::new(socket);
    let mut slots: Vec<Slot> = sensors
        .iter()
        .map(|_| Slot {
            period_ns: Arc::new(AtomicU64::new(period_ns(DEFAULT_PERIOD))),
            poller: None,
        })
        .collect();
    let result = (|| loop {
        let mut bytes = [0_u8; 32];
        let (_, len) = recv(&*socket, &mut bytes[..], RecvFlags::TRUNC)?;
        if len == 0 {
            return Ok(());
        }
        let request = bytes
            .get(..len)
            .and_then(Request::parse)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a sensors request"))?;
        match request {
            Request::Hello { version } if version == wire::VERSION => hello(&socket, sensors)?,
            Request::Hello { version } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("HAL speaks version {version}"),
                ));
            }
            Request::Activate { handle, enabled } => {
                activate(&socket, sensors, &mut slots, index(handle), enabled);
            }
            Request::Batch { handle, period_ns } => batch(&slots, index(handle), period_ns),
        }
    })();
    for slot in &mut slots {
        if let Some(poller) = slot.poller.take() {
            poller.stop();
        }
    }
    result
}
