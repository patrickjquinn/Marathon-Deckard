//! When to ask DeckardSystem to end cached Android processes under memory
//! pressure (protocol/bridge.md, `reclaim`). The adapter owns the PSI and
//! `memory.events` watches and the clock.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Map, Value, json};

/// PSI trigger on the container's `memory.pressure`: 10 % of a 10 s window,
/// the longest window unprivileged triggers allow.
pub const CONTAINER_TRIGGER: &str = "some 1000000 10000000";
/// PSI trigger on `/proc/pressure/memory`, the Shell's avg10 High level.
pub const SYSTEM_TRIGGER: &str = "some 2000000 10000000";
/// Processes each request asks DeckardSystem to end.
pub const COUNT: u64 = 2;
/// The least time between two requests.
const GAP: Duration = Duration::from_secs(2);
/// The gap after a reclaim that found nothing to end.
const IDLE_GAP: Duration = Duration::from_secs(10);

/// What ran short, sent as the request's `reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// `memory.events` counted a trip over `memory.high` or `memory.max`.
    ContainerHigh,
    /// The container's PSI trigger fired.
    ContainerStall,
    /// The phone-wide PSI trigger fired.
    PhoneStall,
}

/// `memory.events`' `high` and `max` counters: the container's trips over
/// its budget.
#[must_use]
pub fn over_budget(events: &str) -> u64 {
    let mut total = 0_u64;
    for line in events.lines() {
        if let Some((key, count)) = line.split_once(' ')
            && matches!(key, "high" | "max")
        {
            total = total.saturating_add(count.parse().unwrap_or(0));
        }
    }
    total
}

/// Rate limit and back-off for `reclaim` requests.
#[derive(Debug, Default)]
pub struct ReclaimPolicy {
    connected: bool,
    idle: bool,
    serial: u64,
    last: Option<Instant>,
}

impl ReclaimPolicy {
    /// Whether DeckardSystem is there to ask.
    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    /// The `reclaim` message for `reason`, or `None` while DeckardSystem is
    /// away or within the gap since the last request.
    #[must_use]
    pub fn request(&mut self, reason: Reason, now: Instant) -> Option<Map<String, Value>> {
        let gap = if self.idle { IDLE_GAP } else { GAP };
        if !self.connected
            || self
                .last
                .is_some_and(|last| now.saturating_duration_since(last) < gap)
        {
            return None;
        }
        self.last = Some(now);
        self.serial = self.serial.wrapping_add(1);
        let id = self.serial.to_string();
        let message = json!({"t": "reclaim", "id": id, "count": COUNT, "reason": reason});
        message.as_object().cloned()
    }

    /// DeckardSystem's `reclaimed` answer: a reclaim that ended nothing backs
    /// off to `IDLE_GAP`.
    pub fn answered(&mut self, ok: bool, ended: usize) {
        self.idle = ok && ended == 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_high_and_max() {
        assert_eq!(over_budget("low 4\nhigh 74\nmax 3\noom 1\n"), 77);
        assert_eq!(over_budget(""), 0);
    }

    #[test]
    fn asks_at_most_once_per_gap_and_backs_off_when_idle() {
        let start = Instant::now();
        let mut policy = ReclaimPolicy::default();
        assert!(policy.request(Reason::ContainerHigh, start).is_none());

        policy.set_connected(true);
        let Some(first) = policy.request(Reason::ContainerHigh, start) else {
            panic!("a connected policy asks");
        };
        assert_eq!(first["t"], "reclaim");
        assert_eq!(first["id"], "1");
        assert_eq!(first["count"], COUNT);
        assert_eq!(first["reason"], "container-high");

        assert!(
            policy
                .request(Reason::PhoneStall, start + GAP / 2)
                .is_none()
        );
        let Some(second) = policy.request(Reason::PhoneStall, start + GAP) else {
            panic!("the gap has passed");
        };
        assert_eq!(second["id"], "2");
        assert_eq!(second["reason"], "phone-stall");

        policy.answered(true, 0);
        assert!(
            policy
                .request(Reason::ContainerStall, start + GAP * 3)
                .is_none()
        );
        policy.answered(true, 1);
        assert!(
            policy
                .request(Reason::ContainerStall, start + GAP * 3)
                .is_some()
        );

        policy.set_connected(false);
        assert!(
            policy
                .request(Reason::ContainerHigh, start + IDLE_GAP * 3)
                .is_none()
        );
    }
}
