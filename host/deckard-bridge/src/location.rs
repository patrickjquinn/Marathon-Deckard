//! Marathon's location as Android's network location provider (DeckardBridge's
//! `NetworkLocation`): one GeoClue client while Android has a request, none
//! otherwise. GeoClue asks the Shell's agent whether Android may locate.

use zbus::runtime::traits::TaskHandle as _;

use std::ops::Bound::Excluded;
use std::ops::RangeBounds;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

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
/// GeoClue's "exact": Android coarsens it for apps with approximate access.
const ACCURACY_EXACT: u32 = 8;
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
        };
        let run = async move {
            if let Err(error) = actor.run(woken).await {
                eprintln!("deckard-bridge: location stopped: {error}");
            }
        };
        bus.spawn("location", run).detach();
        Self { desired, wake }
    }

    /// Takes a `location_request`: `on`, and `interval` in ms.
    pub fn handle(&self, message: &Map<String, Value>) {
        let interval = message.get("interval").and_then(Value::as_f64);
        let interval = interval.unwrap_or(0.0).clamp(0.0, MAX_INTERVAL_MS as f64) as u64;
        self.desired.interval_ms.store(interval, Ordering::Release);
        self.set_wanted(message.get("on").and_then(Value::as_bool) == Some(true));
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
    started: bool,
    updates: MessageStream,
}

enum Event {
    Wake,
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
    set(bus, &path, "RequestedAccuracyLevel", ACCURACY_EXACT.into()).await;
    Ok(Client {
        path,
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
            let event = future::or(wake, future::or(owner, updated)).await;
            match event {
                None => return Ok(()),
                Some(Event::Wake) => self.apply().await,
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

    async fn apply(&mut self) {
        let wanted = self.desired.wanted.load(Ordering::Acquire);
        if wanted && self.client.is_none() {
            match create(&self.bus).await {
                Ok(client) => self.client = Some(client),
                Err(error) => eprintln!("deckard-bridge: no GeoClue client for Android: {error}"),
            }
        }
        let Some(client) = self.client.as_mut() else {
            return;
        };
        if wanted {
            let seconds = self.desired.interval_ms.load(Ordering::Acquire) / 1000;
            let seconds = u32::try_from(seconds).unwrap_or(u32::MAX);
            set(&self.bus, &client.path, "TimeThreshold", seconds.into()).await;
            if !client.started {
                let started = call(&self.bus, &client.path, CLIENT, "Start", &()).await;
                if let Err(error) = &started {
                    eprintln!("deckard-bridge: GeoClue refused Android's location: {error}");
                }
                client.started = started.is_ok();
            }
        } else if client.started {
            client.started = false;
            let _ = call(&self.bus, &client.path, CLIENT, "Stop", &()).await;
        }
    }

    async fn updated(&self, path: &str) {
        let wanted = || self.desired.wanted.load(Ordering::Acquire);
        if !wanted() {
            return;
        }
        let location = ("org.freedesktop.GeoClue2.Location",);
        let reply = call(&self.bus, path, PROPERTIES, "GetAll", &location).await;
        let fix = reply.and_then(|reply| reply.body().deserialize::<Fix>());
        if let Some(out) = fix.ok().as_ref().and_then(message).filter(|_| wanted()) {
            (self.send)(out);
        }
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
