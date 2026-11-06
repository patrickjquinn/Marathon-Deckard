//! Marathon's location as Android's network location provider (DeckardBridge's
//! `NetworkLocation`): one GeoClue client while Android has a request, none
//! otherwise. GeoClue asks the Shell's agent whether Android may locate.
//!
//! GeoClue powers the GNSS receiver (NMEA) only for an exact client, so only a
//! high-accuracy request from an Android app in view gets one; anything else
//! is Wi-Fi and cell. A long interval, or any request while no Android app is
//! in view, takes one fix per period and lets GeoClue go idle in between.

use zbus::runtime::traits::TaskHandle as _;

use std::ops::Bound::Excluded;
use std::ops::RangeBounds;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use async_io::Timer;
use futures_lite::{StreamExt, future};
use serde_json::{Map, Value, json};
use zbus::MessageStream;
use zbus::fdo::DBusProxy;
use zbus::message::Message;
use zbus::{DeserializeDict, OwnedObjectPath, Type, Variant};

use crate::Outbound;

/// The GeoClue client's desktop id, which the Shell's agent authorises.
pub const DESKTOP_ID: &str = "org.marathonos.Deckard";
const MAX_INTERVAL_MS: u64 = 24 * 3600 * 1000;
const GEOCLUE: &str = "org.freedesktop.GeoClue2";
const CLIENT: &str = "org.freedesktop.GeoClue2.Client";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// GeoClue's "exact", the only level that starts GNSS; Android coarsens it
/// for apps with approximate access.
const ACCURACY_EXACT: u32 = 8;
/// GeoClue's "street": Wi-Fi, cell and IP sources.
const ACCURACY_STREET: u32 = 6;
/// `LocationRequest.QUALITY_HIGH_ACCURACY`.
const QUALITY_HIGH_ACCURACY: u64 = 100;
/// Intervals from this on take one fix per interval instead of a running client.
const ONE_SHOT_FROM: Duration = Duration::from_secs(60);
/// The shortest period of a request while no Android app is in view.
const BACKGROUND_PERIOD: Duration = Duration::from_secs(5 * 60);
/// How long a one-shot waits for its fix.
const FIX_TIMEOUT: Duration = Duration::from_secs(60);
/// Qt's default, which the C++ bridge used for every GeoClue call.
const CALL_TIMEOUT: Duration = Duration::from_secs(25);

/// A GeoClue Location's properties; `timestamp` is seconds and microseconds.
#[derive(Debug, Default, DeserializeDict, Type)]
#[zbus(signature = "a{sv}", rename_all = "PascalCase")]
pub struct Fix {
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub accuracy: Option<f64>,
    pub altitude: Option<f64>,
    pub speed: Option<f64>,
    pub heading: Option<f64>,
    pub timestamp: Option<(u64, u64)>,
}

fn within(value: Option<f64>, range: impl RangeBounds<f64>) -> Option<f64> {
    value.filter(|v| range.contains(v))
}

/// A fix as the bridge's `location` message; `None` when it names no place.
#[must_use]
pub fn message(fix: &Fix) -> Option<Map<String, Value>> {
    let lat = within(fix.latitude, -90.0..=90.0)?;
    let lon = within(fix.longitude, -180.0..=180.0)?;
    let acc = within(fix.accuracy, (Excluded(0.0), Excluded(f64::INFINITY)))?;
    let (seconds, micros) = fix.timestamp?;
    let time = seconds.saturating_mul(1000).saturating_add(micros / 1000);
    let mut out = crate::object(json!({
        "t": "location", "lat": lat, "lon": lon, "acc": acc.min(1e7), "time": time,
    }));
    // GeoClue's "unknown" values: -DBL_MAX altitude, -1 speed and heading.
    let optional = [
        ("alt", within(fix.altitude, (Excluded(-1e5), Excluded(1e5)))),
        ("speed", within(fix.speed, 0.0..1e4)),
        ("heading", within(fix.heading, 0.0..360.0)),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            out.insert(key.into(), json!(value));
        }
    }
    (time > 0).then_some(out)
}

#[derive(Default)]
struct Desired {
    wanted: AtomicBool,
    interval_ms: AtomicU64,
    high_accuracy: AtomicBool,
    in_view: AtomicBool,
}

/// How to serve a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// GeoClue's `RequestedAccuracyLevel`.
    pub level: u32,
    /// `None`: a running client. Otherwise one fix per period.
    pub period: Option<Duration>,
}

/// The plan for Android's request: `interval`, whether it asks for high
/// accuracy, and whether an Android app is in front of a lit screen.
#[must_use]
pub fn plan(interval: Duration, high_accuracy: bool, in_view: bool) -> Plan {
    let level = if high_accuracy && in_view {
        ACCURACY_EXACT
    } else {
        ACCURACY_STREET
    };
    let interval = if in_view {
        interval
    } else {
        interval.max(BACKGROUND_PERIOD)
    };
    let period = (interval >= ONE_SHOT_FROM).then_some(interval);
    Plan { level, period }
}

/// Follows Android's `location_request`s; cheap to clone.
#[derive(Clone)]
pub struct LocationSync {
    desired: Arc<Desired>,
    wake: Sender<()>,
}

impl LocationSync {
    /// Runs on `bus` (the system bus in production) and sends fixes with `send`.
    #[must_use]
    pub fn start(bus: zbus::Connection, send: Arc<Outbound>) -> Self {
        let desired = Arc::new(Desired::default());
        let (wake, woken) = async_channel::bounded(1);
        let actor = Actor {
            bus: bus.clone(),
            send,
            desired: Arc::clone(&desired),
            client: None,
            due: None,
        };
        let run = async move {
            if let Err(error) = actor.run(woken).await {
                eprintln!("deckard-bridge: location stopped: {error}");
            }
        };
        bus.spawn("location", run).detach();
        Self { desired, wake }
    }

    /// Takes a `location_request`: `on`, `interval` in ms and `quality`.
    pub fn handle(&self, message: &Map<String, Value>) {
        let interval = message.get("interval").and_then(Value::as_f64);
        let interval = interval.unwrap_or(0.0).clamp(0.0, MAX_INTERVAL_MS as f64) as u64;
        self.desired.interval_ms.store(interval, Ordering::Release);
        let quality = message.get("quality").and_then(Value::as_u64);
        let high = quality == Some(QUALITY_HIGH_ACCURACY);
        self.desired.high_accuracy.store(high, Ordering::Release);
        self.set_wanted(message.get("on").and_then(Value::as_bool) == Some(true));
    }

    /// Whether an Android app is in front of a lit screen.
    pub fn set_in_view(&self, in_view: bool) {
        if self.desired.in_view.swap(in_view, Ordering::AcqRel) != in_view {
            let _ = self.wake.try_send(());
        }
    }

    /// DeckardBridge went away: its request went with it.
    pub fn bridge_lost(&self) {
        self.set_wanted(false);
    }

    fn set_wanted(&self, on: bool) {
        self.desired.wanted.store(on, Ordering::Release);
        // Full means a wake is already pending, which will see this state.
        let _ = self.wake.try_send(());
    }
}

struct Client {
    path: OwnedObjectPath,
    level: u32,
    started: bool,
    updates: MessageStream,
}

enum Event {
    Wake,
    Due,
    OwnerLost,
    Updated(OwnedObjectPath),
    Ignored,
}

/// Owns the GeoClue client. Calls are awaited in turn: a request that
/// arrives meanwhile is applied after, as Android's latest state.
struct Actor {
    bus: zbus::Connection,
    send: Arc<Outbound>,
    desired: Arc<Desired>,
    client: Option<Client>,
    /// One-shot mode: the next start, or the end of the wait for a fix.
    due: Option<Instant>,
}

async fn call<B>(
    bus: &zbus::Connection,
    path: &str,
    iface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<Message>
where
    B: serde::Serialize + Type,
{
    let reply = bus.call_method(Some(GEOCLUE), path, Some(iface), method, body);
    let timeout = async {
        Timer::after(CALL_TIMEOUT).await;
        Err(zbus::Error::Failure("GeoClue didn't answer".into()))
    };
    future::or(reply, timeout).await
}

async fn set(bus: &zbus::Connection, client: &str, name: &str, value: Variant<'_>) {
    if let Err(error) = call(bus, client, PROPERTIES, "Set", &(CLIENT, name, value)).await {
        eprintln!("deckard-bridge: GeoClue {name}: {error}");
    }
}

async fn create(bus: &zbus::Connection) -> zbus::Result<Client> {
    let manager = "org.freedesktop.GeoClue2.Manager";
    let reply = call(
        bus,
        "/org/freedesktop/GeoClue2/Manager",
        manager,
        "GetClient",
        &(),
    )
    .await?;
    let path: OwnedObjectPath = reply.body().deserialize()?;
    let rule = crate::signals(GEOCLUE, path.as_str(), CLIENT)?;
    let rule = rule.member("LocationUpdated").build();
    let updates =
        MessageStream::for_match_rule(rule?, bus, std::num::NonZeroUsize::new(16)).await?;
    set(bus, &path, "DesktopId", DESKTOP_ID.into()).await;
    Ok(Client {
        path,
        level: 0,
        started: false,
        updates,
    })
}

impl Actor {
    async fn run(mut self, woken: Receiver<()>) -> zbus::Result<()> {
        let proxy = DBusProxy::new(&self.bus).await?;
        let mut owners = proxy
            .receive_name_owner_changed_with_args(&[(0, GEOCLUE)])
            .await?;
        loop {
            let wake = async { woken.recv().await.ok().map(|()| Event::Wake) };
            let owner = async {
                let lost = owners
                    .next()
                    .await?
                    .args()
                    .is_ok_and(|a| a.old_owner().is_some());
                Some(if lost {
                    Event::OwnerLost
                } else {
                    Event::Ignored
                })
            };
            let updated = async {
                let Some(client) = self.client.as_mut() else {
                    return future::pending().await;
                };
                let Some(Ok(signal)) = client.updates.next().await else {
                    return Some(Event::Ignored);
                };
                let paths = signal
                    .body()
                    .deserialize::<(OwnedObjectPath, OwnedObjectPath)>();
                Some(paths.map_or(Event::Ignored, |(_, new)| Event::Updated(new)))
            };
            let due_at = self.due;
            let due = async {
                let Some(at) = due_at else {
                    return future::pending().await;
                };
                Timer::at(at).await;
                Some(Event::Due)
            };
            let event = future::or(wake, future::or(owner, future::or(updated, due))).await;
            match event {
                None => return Ok(()),
                Some(Event::Wake | Event::Due) => self.apply().await,
                // GeoClue exits when idle and forgets its clients.
                Some(Event::OwnerLost) => {
                    if self.client.take().is_some() {
                        self.apply().await;
                    }
                }
                Some(Event::Updated(path)) => self.updated(&path).await,
                Some(Event::Ignored) => {}
            }
        }
    }

    async fn stop(&mut self) {
        if let Some(client) = self.client.as_mut().filter(|client| client.started) {
            client.started = false;
            let _ = call(&self.bus, &client.path, CLIENT, "Stop", &()).await;
        }
    }

    async fn apply(&mut self) {
        let wanted = self.desired.wanted.load(Ordering::Acquire);
        if !wanted {
            self.due = None;
            return self.stop().await;
        }
        let interval = Duration::from_millis(self.desired.interval_ms.load(Ordering::Acquire));
        let plan = plan(
            interval,
            self.desired.high_accuracy.load(Ordering::Acquire),
            self.desired.in_view.load(Ordering::Acquire),
        );
        if self.client.is_none() {
            match create(&self.bus).await {
                Ok(client) => self.client = Some(client),
                Err(error) => {
                    return eprintln!("deckard-bridge: no GeoClue client for Android: {error}");
                }
            }
        }
        if self
            .client
            .as_ref()
            .is_some_and(|client| client.level != plan.level)
        {
            self.due = None;
            self.stop().await;
        }
        let now = Instant::now();
        let Some(client) = self.client.as_mut() else {
            return;
        };
        if client.level != plan.level {
            set(
                &self.bus,
                &client.path,
                "RequestedAccuracyLevel",
                plan.level.into(),
            )
            .await;
            client.level = plan.level;
        }
        let threshold = match plan.period {
            None => {
                self.due = None;
                interval.as_secs()
            }
            Some(period) => {
                if client.started {
                    // Still waiting for its fix, or it never came.
                    let deadline = *self.due.get_or_insert(now + FIX_TIMEOUT);
                    if now < deadline {
                        return;
                    }
                    self.due = Some(now + period);
                    return self.stop().await;
                }
                if self.due.is_some_and(|at| now < at) {
                    return;
                }
                self.due = Some(now + FIX_TIMEOUT);
                0
            }
        };
        let threshold = u32::try_from(threshold).unwrap_or(u32::MAX);
        set(&self.bus, &client.path, "TimeThreshold", threshold.into()).await;
        if !client.started {
            let started = call(&self.bus, &client.path, CLIENT, "Start", &()).await;
            if let Err(error) = &started {
                eprintln!("deckard-bridge: GeoClue refused Android's location: {error}");
            }
            client.started = started.is_ok();
        }
    }

    async fn updated(&mut self, path: &str) {
        let wanted = || self.desired.wanted.load(Ordering::Acquire);
        if !wanted() || !self.client.as_ref().is_some_and(|client| client.started) {
            return;
        }
        let location = ("org.freedesktop.GeoClue2.Location",);
        let reply = call(&self.bus, path, PROPERTIES, "GetAll", &location).await;
        let fix = reply.and_then(|reply| reply.body().deserialize::<Fix>());
        let Some(out) = fix.ok().as_ref().and_then(message).filter(|_| wanted()) else {
            return;
        };
        (self.send)(out);
        // A one-shot has its fix: GeoClue idles until the next period.
        if let Some(period) = self.one_shot_period() {
            self.due = Some(Instant::now() + period);
            self.stop().await;
        }
    }

    fn one_shot_period(&self) -> Option<Duration> {
        let interval = Duration::from_millis(self.desired.interval_ms.load(Ordering::Acquire));
        let high = self.desired.high_accuracy.load(Ordering::Acquire);
        plan(interval, high, self.desired.in_view.load(Ordering::Acquire)).period
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fix_becomes_the_bridge_message() {
        let place = || Fix {
            latitude: Some(53.35),
            longitude: Some(-6.26),
            accuracy: Some(40.0),
            altitude: Some(12.5),
            speed: Some(-1.0),
            heading: Some(90.0),
            timestamp: Some((1_790_000_000, 500_000)),
        };
        let m = message(&place()).unwrap_or_default();
        let expected = json!({"t":"location", "lat":53.35, "lon":-6.26, "acc":40.0,
            "alt":12.5, "heading":90.0, "time":1_790_000_000_500_u64});
        assert_eq!(Value::Object(m), expected);
        let mut bad = [place(), place(), place(), place()];
        bad[0].latitude = Some(91.0);
        bad[1].accuracy = Some(0.0);
        bad[2].longitude = Some(f64::NAN);
        bad[3].timestamp = None;
        assert!(bad.iter().all(|fix| message(fix).is_none()));
    }
}
