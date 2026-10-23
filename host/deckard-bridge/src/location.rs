//! Marathon's location as Android's network location provider (DeckardBridge's
//! `NetworkLocation`): one GeoClue client while Android has a request, none
//! otherwise. GeoClue asks the Shell's agent whether Android may locate.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use async_io::Timer;
use futures_lite::{StreamExt, future};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use zbus::fdo::DBusProxy;
use zbus::message::Message;
use zbus::zvariant::{OwnedObjectPath, Type, Value as Variant, as_value};
use zbus::{MatchRule, MessageStream};

use crate::Outbound;

pub const DESKTOP_ID: &str = "org.marathonos.Deckard";
pub const MAX_INTERVAL_MS: u64 = 24 * 3600 * 1000;
const GEOCLUE: &str = "org.freedesktop.GeoClue2";
const MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const MANAGER: &str = "org.freedesktop.GeoClue2.Manager";
const CLIENT: &str = "org.freedesktop.GeoClue2.Client";
const LOCATION: &str = "org.freedesktop.GeoClue2.Location";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// GeoClue's "exact": Android coarsens it for apps with approximate access.
const ACCURACY_EXACT: u32 = 8;
/// Qt's default, which the C++ bridge used for every GeoClue call.
const CALL_TIMEOUT: Duration = Duration::from_secs(25);

/// A GeoClue Location's properties.
#[derive(Debug, Default, Deserialize, Serialize, Type)]
#[zvariant(signature = "a{sv}")]
#[serde(default, rename_all = "PascalCase")]
pub struct Fix {
    #[serde(with = "as_value::optional", skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(with = "as_value::optional", skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(with = "as_value::optional", skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<f64>,
    #[serde(with = "as_value::optional", skip_serializing_if = "Option::is_none")]
    pub altitude: Option<f64>,
    #[serde(with = "as_value::optional", skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(with = "as_value::optional", skip_serializing_if = "Option::is_none")]
    pub heading: Option<f64>,
    /// Seconds and microseconds since the epoch.
    #[serde(with = "as_value::optional", skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<(u64, u64)>,
}

/// A fix as the bridge's `location` message; `None` when it names no place.
#[must_use]
pub fn message(fix: &Fix) -> Option<Map<String, Value>> {
    let finite = |value: Option<f64>| value.filter(|v| v.is_finite());
    let lat = finite(fix.latitude).filter(|v| (-90.0..=90.0).contains(v))?;
    let lon = finite(fix.longitude).filter(|v| (-180.0..=180.0).contains(v))?;
    let acc = finite(fix.accuracy).filter(|v| *v > 0.0)?;
    let (seconds, micros) = fix.timestamp?;
    let time = seconds.saturating_mul(1000).saturating_add(micros / 1000);
    if time == 0 {
        return None;
    }
    let Value::Object(mut out) = json!({
        "t": "location", "lat": lat, "lon": lon, "acc": acc.min(1e7), "time": time as f64,
    }) else {
        return None;
    };
    // GeoClue's "unknown" values: -DBL_MAX altitude, -1 speed and heading.
    let optional = [
        (
            "alt",
            finite(fix.altitude).filter(|v| *v > -1e5 && *v < 1e5),
        ),
        (
            "speed",
            finite(fix.speed).filter(|v| (0.0..1e4).contains(v)),
        ),
        (
            "heading",
            finite(fix.heading).filter(|v| (0.0..360.0).contains(v)),
        ),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            out.insert(key.into(), json!(value));
        }
    }
    Some(out)
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
        let (events, received) = async_channel::bounded(8);
        let actor = Actor {
            bus: bus.clone(),
            send,
            desired: Arc::clone(&desired),
            events,
            client: None,
            creating: false,
        };
        let run = async move {
            if let Err(error) = actor.run(woken, received).await {
                eprintln!("deckard-bridge: location stopped: {error}");
            }
        };
        bus.executor().spawn(run, "location").detach();
        Self { desired, wake }
    }

    /// Takes a `location_request`; other messages are ignored.
    pub fn handle(&self, message: &Map<String, Value>) {
        if message.get("t").and_then(Value::as_str) != Some("location_request") {
            return;
        }
        let interval = message
            .get("interval")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let interval = if interval.is_finite() {
            interval.clamp(0.0, MAX_INTERVAL_MS as f64) as u64
        } else {
            0
        };
        let on = message.get("on").and_then(Value::as_bool).unwrap_or(false);
        self.desired.interval_ms.store(interval, Ordering::Release);
        self.set_wanted(on);
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

enum Event {
    Wake,
    Created(zbus::Result<OwnedObjectPath>),
    StartFailed,
    OwnerLost,
    Updated(OwnedObjectPath),
    Ignored,
}

struct Client {
    path: OwnedObjectPath,
    started: bool,
    updates: MessageStream,
}

struct Actor {
    bus: zbus::Connection,
    send: Arc<Outbound>,
    desired: Arc<Desired>,
    events: Sender<Event>,
    client: Option<Client>,
    creating: bool,
}

async fn call<B>(
    bus: &zbus::Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<Message>
where
    B: serde::Serialize + Type,
{
    let reply = bus.call_method(Some(GEOCLUE), path, Some(interface), method, body);
    future::or(reply, async {
        Timer::after(CALL_TIMEOUT).await;
        Err(zbus::Error::Failure("GeoClue didn't answer".into()))
    })
    .await
}

async fn set(bus: &zbus::Connection, client: &str, name: &str, value: Variant<'_>) {
    if let Err(error) = call(bus, client, PROPERTIES, "Set", &(CLIENT, name, value)).await {
        eprintln!("deckard-bridge: GeoClue {name}: {error}");
    }
}

impl Actor {
    async fn run(mut self, woken: Receiver<()>, received: Receiver<Event>) -> zbus::Result<()> {
        let mut owners = DBusProxy::new(&self.bus)
            .await?
            .receive_name_owner_changed_with_args(&[(0, GEOCLUE)])
            .await?;
        loop {
            let wake = async { woken.recv().await.ok().map(|()| Event::Wake) };
            let internal = async { received.recv().await.ok() };
            let owner = async {
                let signal = owners.next().await?;
                let lost = signal.args().is_ok_and(|args| args.old_owner().is_some());
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
                let Some(Ok(message)) = client.updates.next().await else {
                    return Some(Event::Ignored);
                };
                let paths = message
                    .body()
                    .deserialize::<(OwnedObjectPath, OwnedObjectPath)>();
                Some(paths.map_or(Event::Ignored, |(_, new)| Event::Updated(new)))
            };
            let event = future::or(future::or(wake, internal), future::or(owner, updated)).await;
            let Some(event) = event else {
                return Ok(());
            };
            self.on(event).await;
        }
    }

    async fn on(&mut self, event: Event) {
        match event {
            Event::Wake => self.apply().await,
            Event::Created(result) => {
                self.creating = false;
                match result {
                    Ok(path) => self.created(path).await,
                    Err(error) => {
                        eprintln!("deckard-bridge: no GeoClue client for Android: {error}")
                    }
                }
            }
            Event::StartFailed => {
                if let Some(client) = self.client.as_mut() {
                    client.started = false;
                }
            }
            // GeoClue exits when idle and forgets its clients.
            Event::OwnerLost => {
                if self.client.take().is_some() {
                    self.creating = false;
                    self.apply().await;
                }
            }
            Event::Updated(path) => self.updated(&path).await,
            Event::Ignored => {}
        }
    }

    async fn created(&mut self, path: OwnedObjectPath) {
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(GEOCLUE)
            .and_then(|rule| rule.path(path.clone()))
            .and_then(|rule| rule.interface(CLIENT))
            .and_then(|rule| rule.member("LocationUpdated"))
            .map(|rule| rule.build());
        let updates = match rule {
            Ok(rule) => MessageStream::for_match_rule(rule, &self.bus, Some(16)).await,
            Err(error) => Err(error),
        };
        let updates = match updates {
            Ok(updates) => updates,
            Err(error) => {
                eprintln!("deckard-bridge: GeoClue updates: {error}");
                return;
            }
        };
        set(&self.bus, path.as_str(), "DesktopId", DESKTOP_ID.into()).await;
        set(
            &self.bus,
            path.as_str(),
            "RequestedAccuracyLevel",
            ACCURACY_EXACT.into(),
        )
        .await;
        self.client = Some(Client {
            path,
            started: false,
            updates,
        });
        self.apply().await;
    }

    async fn apply(&mut self) {
        let wanted = self.desired.wanted.load(Ordering::Acquire);
        let Some(client) = self.client.as_mut() else {
            if wanted && !self.creating {
                self.creating = true;
                let (bus, events) = (self.bus.clone(), self.events.clone());
                let create = async move {
                    let reply = call(&bus, MANAGER_PATH, MANAGER, "GetClient", &()).await;
                    let path =
                        reply.and_then(|reply| reply.body().deserialize::<OwnedObjectPath>());
                    let _ = events.send(Event::Created(path)).await;
                };
                self.bus.executor().spawn(create, "geoclue-client").detach();
            }
            return;
        };
        let path = client.path.as_str();
        if wanted {
            let seconds = self.desired.interval_ms.load(Ordering::Acquire) / 1000;
            let seconds = u32::try_from(seconds).unwrap_or(u32::MAX);
            set(&self.bus, path, "TimeThreshold", seconds.into()).await;
            if client.started {
                return;
            }
            client.started = true;
            let (bus, events, path) = (self.bus.clone(), self.events.clone(), path.to_owned());
            let start = async move {
                if let Err(error) = call(&bus, &path, CLIENT, "Start", &()).await {
                    eprintln!("deckard-bridge: GeoClue refused Android's location: {error}");
                    let _ = events.send(Event::StartFailed).await;
                }
            };
            self.bus.executor().spawn(start, "geoclue-start").detach();
        } else if client.started {
            client.started = false;
            let _ = call(&self.bus, path, CLIENT, "Stop", &()).await;
        }
    }

    async fn updated(&self, path: &OwnedObjectPath) {
        if !self.desired.wanted.load(Ordering::Acquire) {
            return;
        }
        let reply = call(&self.bus, path.as_str(), PROPERTIES, "GetAll", &(LOCATION,)).await;
        let fix = reply.and_then(|reply| reply.body().deserialize::<Fix>());
        if !self.desired.wanted.load(Ordering::Acquire) {
            return;
        }
        if let Some(out) = fix.ok().as_ref().and_then(message) {
            (self.send)(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place() -> Fix {
        Fix {
            latitude: Some(53.35),
            longitude: Some(-6.26),
            accuracy: Some(40.0),
            altitude: Some(12.5),
            speed: Some(-1.0),
            heading: Some(90.0),
            timestamp: Some((1_790_000_000, 500_000)),
        }
    }

    #[test]
    fn a_fix_becomes_the_bridge_message() {
        let m = message(&place()).unwrap_or_default();
        assert_eq!(m["t"], "location");
        assert_eq!(m["lat"], 53.35);
        assert_eq!(m["acc"], 40.0);
        assert_eq!(m["alt"], 12.5);
        assert_eq!(m["heading"], 90.0);
        assert!(!m.contains_key("speed"));
        assert_eq!(m["time"], 1_790_000_000_500.0);

        let bad = [
            Fix {
                latitude: Some(91.0),
                ..place()
            },
            Fix {
                accuracy: Some(0.0),
                ..place()
            },
            Fix {
                longitude: Some(f64::NAN),
                ..place()
            },
            Fix {
                timestamp: None,
                ..place()
            },
        ];
        for fix in bad {
            assert!(message(&fix).is_none());
        }
    }
}
