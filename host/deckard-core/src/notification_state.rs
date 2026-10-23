//! Pure notification reconciliation. The adapter owns D-Bus, timers, and
//! atomic file writes.
//!
//! `display_digest` must be the SHA-256 of the C++ Hub-facing QDataStream
//! fields (excluding `suppress-sound`) while migrating notification state.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::messages::Post;
use crate::validate;

pub const MAX_TRACKED: usize = 200;
pub const DISMISS_DELAY_MS: u64 = 500;
pub const TAP_GRACE_MS: i64 = 3_000;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Close(u32),
    /// Write this entire snapshot atomically to the existing notifications.json path.
    Persist(Value),
    ScheduleDismiss {
        id: u32,
        token: u64,
        delay_ms: u64,
    },
    Send(BridgeEvent),
}

/// A message for DeckardBridge; serialises as its wire object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum BridgeEvent {
    Invoke {
        key: String,
        action: String,
    },
    Reply {
        key: String,
        action: String,
        text: String,
    },
    Dismiss {
        key: String,
    },
}

#[derive(Clone, Debug)]
struct Entry {
    id: u32,
    digest: Vec<u8>,
    reply_action: String,
    auto_cancel: bool,
    invoked_at: i64,
}

struct Closing {
    key: String,
    entry: Entry,
    token: u64,
}

#[derive(Default)]
pub struct NotificationState {
    entries: BTreeMap<String, Entry>,
    keys: HashMap<u32, String>,
    closing: HashMap<u32, Closing>,
    next_token: u64,
}

impl NotificationState {
    /// Accepts the legacy object keyed by Android notification key.
    /// Malformed rows are ignored.
    #[must_use]
    pub fn restore(state: &Value) -> Self {
        let mut result = Self::default();
        let rows = state.as_object().into_iter().flatten();
        for (key, row) in rows {
            let field = |name: &str| row.get(name).unwrap_or(&Value::Null);
            let Some(id) = field("id").as_f64().and_then(legacy_id) else {
                continue;
            };
            let digest = field("digest")
                .as_str()
                .and_then(|hex| hex::decode(hex).ok());
            let entry = Entry {
                id,
                digest: digest.unwrap_or_default(),
                reply_action: field("reply").as_str().unwrap_or_default().to_owned(),
                auto_cancel: field("autoCancel").as_bool().unwrap_or(false),
                invoked_at: 0,
            };
            result.keys.insert(id, key.clone());
            result.entries.insert(key.clone(), entry);
        }
        result
    }

    #[must_use]
    pub fn snapshot(&self) -> Value {
        let mut state = Map::new();
        for (key, entry) in &self.entries {
            let row = json!({
                "id": entry.id,
                "digest": hex::encode(&entry.digest),
                "reply": entry.reply_action,
                "autoCancel": entry.auto_cancel,
            });
            state.insert(key.clone(), row);
        }
        Value::Object(state)
    }

    /// The Hub id a Notify for `post` replaces, 0 for none; `None` when the
    /// Hub already shows this payload or too many rows are tracked. The
    /// adapter passes Unix milliseconds to the action and timer methods.
    #[must_use]
    pub fn replaces(&self, post: &Post, display_digest: &[u8]) -> Option<u32> {
        match self.entries.get(&post.key) {
            Some(entry) if entry.digest == display_digest => None,
            Some(entry) => Some(entry.id),
            None if self.entries.len() >= MAX_TRACKED => None,
            None => Some(0),
        }
    }

    /// Tracks `post` as the Hub's notification `id`; 0 is a refusal.
    #[must_use]
    pub fn notified(&mut self, post: &Post, display_digest: &[u8], id: u32) -> Vec<Command> {
        if id == 0 {
            return Vec::new();
        }
        let entry = Entry {
            id,
            digest: display_digest.to_vec(),
            reply_action: post.reply_action.clone(),
            auto_cancel: post.auto_cancel,
            invoked_at: 0,
        };
        if let Some(previous) = self.entries.insert(post.key.clone(), entry)
            && previous.id != id
        {
            self.keys.remove(&previous.id);
        }
        self.keys.insert(id, post.key.clone());
        vec![Command::Persist(self.snapshot())]
    }

    /// Remove local mappings before asking the Hub to close the row.
    #[must_use]
    pub fn remove(&mut self, key: &str) -> Vec<Command> {
        let Some(entry) = self.entries.remove(key) else {
            return Vec::new();
        };
        self.keys.remove(&entry.id);
        vec![Command::Close(entry.id), Command::Persist(self.snapshot())]
    }

    /// Reconcile a complete Android key set. Removals are emitted in key order.
    #[must_use]
    pub fn sync(&mut self, live_keys: &[String]) -> Vec<Command> {
        let live: HashSet<&str> = live_keys.iter().map(String::as_str).collect();
        let stale: Vec<String> = self
            .entries
            .keys()
            .filter(|key| !live.contains(key.as_str()))
            .cloned()
            .collect();
        let mut commands = Vec::new();
        for key in stale {
            commands.extend(self.remove(&key));
        }
        commands
    }

    #[must_use]
    pub fn action_invoked(&mut self, id: u32, action: &str, now_ms: i64) -> Vec<Command> {
        if action == "inline-reply" {
            return Vec::new();
        }
        let tracked = self
            .keys
            .get(&id)
            .and_then(|key| Some((key, self.entries.get_mut(key)?)));
        let (key, entry) = match tracked {
            Some((key, entry)) => (key, entry),
            None => match self.closing.get_mut(&id) {
                Some(closing) => (&closing.key, &mut closing.entry),
                None => return Vec::new(),
            },
        };
        entry.invoked_at = now_ms;
        vec![Command::Send(BridgeEvent::Invoke {
            key: key.clone(),
            action: validate::truncate(action, 16),
        })]
    }

    /// Replies are accepted only while the notification remains active.
    #[must_use]
    pub fn notification_replied(&self, id: u32, text: &str) -> Vec<Command> {
        let Some(key) = self.keys.get(&id) else {
            return Vec::new();
        };
        let Some(entry) = self
            .entries
            .get(key)
            .filter(|entry| !entry.reply_action.is_empty())
        else {
            return Vec::new();
        };
        vec![Command::Send(BridgeEvent::Reply {
            key: key.clone(),
            action: entry.reply_action.clone(),
            text: validate::truncate(text, 4096),
        })]
    }

    #[must_use]
    pub fn notification_closed(&mut self, id: u32, reason: u32) -> Vec<Command> {
        let Some(key) = self.keys.remove(&id) else {
            return Vec::new();
        };
        let Some(entry) = self.entries.remove(&key) else {
            return Vec::new();
        };
        let mut commands = vec![Command::Persist(self.snapshot())];
        if reason == 2 {
            let token = self.token();
            self.closing.insert(id, Closing { key, entry, token });
            commands.push(Command::ScheduleDismiss {
                id,
                token,
                delay_ms: DISMISS_DELAY_MS,
            });
        }
        commands
    }

    #[must_use]
    pub fn dismiss_due(&mut self, id: u32, token: u64, now_ms: i64) -> Vec<Command> {
        if self
            .closing
            .get(&id)
            .is_none_or(|closing| closing.token != token)
        {
            return Vec::new();
        }
        let Some(Closing { key, entry, .. }) = self.closing.remove(&id) else {
            return Vec::new();
        };
        if !entry.auto_cancel && now_ms.saturating_sub(entry.invoked_at) < TAP_GRACE_MS {
            return Vec::new();
        }
        vec![Command::Send(BridgeEvent::Dismiss { key })]
    }

    fn token(&mut self) -> u64 {
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        token
    }
}

fn legacy_id(number: f64) -> Option<u32> {
    let whole = number.fract() == 0.0 && (1.0..=f64::from(u32::MAX)).contains(&number);
    whole.then_some(number as u32)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests assert valid typed messages")]
mod tests {
    use super::*;
    use crate::messages;

    fn post(key: &str, auto_cancel: bool, reply: bool) -> Post {
        let mut message = json!({
            "key": key,
            "pkg": "org.example.app",
            "title": "Title",
            "content": true,
            "autoCancel": auto_cancel,
        });
        if reply {
            message["reply"] = json!({"action": "a1", "label": "Reply"});
        }
        messages::post(message.as_object().unwrap()).unwrap()
    }

    fn tracked(state: &mut NotificationState, key: &str, id: u32, auto_cancel: bool) {
        let post = post(key, auto_cancel, true);
        assert!(state.replaces(&post, &[1, 2]).is_some());
        assert_eq!(state.notified(&post, &[1, 2], id).len(), 1);
    }

    fn dismiss_token(commands: &[Command]) -> u64 {
        match commands {
            [Command::Persist(_), Command::ScheduleDismiss { token, .. }] => *token,
            other => panic!("expected a dismissal timer, got {other:?}"),
        }
    }

    #[test]
    fn legacy_json_round_trip_and_digest_reconciliation() {
        let legacy =
            json!({"key": {"id": 42.0, "digest": "0102ff", "reply": "a1", "autoCancel": true}});
        let mut state = NotificationState::restore(&legacy);
        assert_eq!(
            state.snapshot(),
            json!({"key": {"id": 42, "digest": "0102ff", "reply": "a1", "autoCancel": true}})
        );
        let changed = post("key", false, true);
        assert_eq!(state.replaces(&changed, &[1, 2, 255]), None);
        assert_eq!(state.replaces(&changed, &[3]), Some(42));
        assert_eq!(
            state.notified(&changed, &[3], 43),
            vec![Command::Persist(
                json!({"key": {"id": 43, "digest": "03", "reply": "a1", "autoCancel": false}})
            )]
        );
        assert!(state.action_invoked(42, "default", 1_000).is_empty());
        assert_eq!(
            state.action_invoked(43, "default", 1_000),
            vec![Command::Send(BridgeEvent::Invoke {
                key: "key".into(),
                action: "default".into()
            })]
        );
    }

    #[test]
    fn removal_and_sync_close_before_persist_in_key_order() {
        let mut state = NotificationState::default();
        tracked(&mut state, "b", 2, false);
        tracked(&mut state, "a", 1, false);
        assert_eq!(
            state.sync(&["b".into()]),
            vec![
                Command::Close(1),
                Command::Persist(
                    json!({"b": {"id": 2, "digest": "0102", "reply": "a1", "autoCancel": false}})
                )
            ]
        );
        assert!(state.notification_closed(1, 2).is_empty());
        assert!(state.remove("missing").is_empty());
        assert_eq!(
            state.remove("b"),
            vec![Command::Close(2), Command::Persist(json!({}))]
        );
    }

    #[test]
    fn reply_and_action_wire_shapes_and_utf16_limits() {
        let mut state = NotificationState::default();
        tracked(&mut state, "key", 7, false);
        assert!(state.action_invoked(7, "inline-reply", 100).is_empty());
        let action = state.action_invoked(7, "😀".repeat(9).as_str(), 100);
        let invoke = BridgeEvent::Invoke {
            key: "key".into(),
            action: "😀".repeat(8),
        };
        assert_eq!(action, vec![Command::Send(invoke)]);
        let reply = state.notification_replied(7, &("x".repeat(4095) + "😀more"));
        let Command::Send(event) = &reply[0] else {
            panic!("expected a reply");
        };
        let mut expected = json!({"t": "reply", "key": "key", "action": "a1"});
        expected["text"] = json!("x".repeat(4095));
        assert_eq!(serde_json::to_value(event).unwrap(), expected);
        assert_eq!(
            serde_json::to_value(BridgeEvent::Dismiss { key: "key".into() }).unwrap(),
            json!({"t": "dismiss", "key": "key"})
        );
        assert!(state.notification_replied(99, "text").is_empty());
    }

    #[test]
    fn close_before_tap_suppresses_dismiss_for_non_auto_cancel() {
        let mut state = NotificationState::default();
        tracked(&mut state, "key", 7, false);
        let commands = state.notification_closed(7, 2);
        let token = dismiss_token(&commands);
        assert_eq!(state.snapshot(), json!({}));
        assert_eq!(state.action_invoked(7, "default", 1_100).len(), 1);
        assert!(state.dismiss_due(7, token, 1_500).is_empty());
        assert!(state.dismiss_due(7, token, 4_500).is_empty());
    }

    #[test]
    fn dismissal_without_tap_auto_cancel_tap_and_grace_boundary() {
        let mut state = NotificationState::default();
        tracked(&mut state, "plain", 7, false);
        let token = dismiss_token(&state.notification_closed(7, 2));
        let dismiss = |key: &str| vec![Command::Send(BridgeEvent::Dismiss { key: key.into() })];
        assert_eq!(state.dismiss_due(7, token, 10_000), dismiss("plain"));

        tracked(&mut state, "auto", 8, true);
        let _ = state.action_invoked(8, "a1", 10_100);
        let token = dismiss_token(&state.notification_closed(8, 2));
        assert_eq!(state.dismiss_due(8, token, 10_500), dismiss("auto"));

        tracked(&mut state, "key", 9, false);
        let _ = state.action_invoked(9, "default", 10_000);
        let token = dismiss_token(&state.notification_closed(9, 2));
        assert_eq!(state.dismiss_due(9, token, 13_000), dismiss("key"));
    }

    #[test]
    fn non_user_close_does_not_dismiss_or_accept_late_actions() {
        let mut state = NotificationState::default();
        tracked(&mut state, "key", 7, false);
        assert_eq!(
            state.notification_closed(7, 1),
            vec![Command::Persist(json!({}))]
        );
        assert!(state.action_invoked(7, "default", 100).is_empty());
        assert!(state.notification_replied(7, "hello").is_empty());
    }

    #[test]
    fn cap_applies_to_new_rows_but_allows_updates() {
        let mut state = NotificationState::default();
        for n in 0..MAX_TRACKED {
            tracked(&mut state, &format!("key{n}"), n as u32 + 1, false);
        }
        assert_eq!(state.replaces(&post("extra", false, false), &[1]), None);
        assert_eq!(state.replaces(&post("key0", false, false), &[9]), Some(1));
    }

    #[test]
    fn invalid_restore_rows_and_stale_timers_are_ignored() {
        let mut state = NotificationState::restore(&json!({
            "zero": {"id": 0}, "negative": {"id": -1}, "fraction": {"id": 2.5},
            "valid": {"id": 4, "digest": "invalid", "reply": "a1"}
        }));
        assert_eq!(
            state.snapshot(),
            json!({"valid": {"id": 4, "digest": "", "reply": "a1", "autoCancel": false}})
        );
        let token = dismiss_token(&state.notification_closed(4, 2));
        assert!(state.dismiss_due(4, token + 1, 10_000).is_empty());
        assert_eq!(state.dismiss_due(4, token, 10_000).len(), 1);
        assert!(state.dismiss_due(4, token, 10_000).is_empty());
    }
}
