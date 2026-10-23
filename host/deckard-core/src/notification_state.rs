//! Pure notification reconciliation. The adapter owns D-Bus, timers, and atomic file writes.
//!
//! `display_digest` must be the SHA-256 of the C++ Hub-facing QDataStream fields
//! (excluding `suppress-sound`) while migrating existing notification state.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::messages::Post;

pub const MAX_TRACKED: usize = 200;
pub const DISMISS_DELAY_MS: u64 = 500;
pub const TAP_GRACE_MS: i64 = 3_000;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Notify {
        request: u64,
        post: Box<Post>,
        replaces_id: u32,
    },
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

#[derive(Clone, Debug, PartialEq, Eq)]
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

impl BridgeEvent {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Invoke { key, action } => json!({"t": "invoke", "key": key, "action": action}),
            Self::Reply { key, action, text } => {
                json!({"t": "reply", "key": key, "action": action, "text": text})
            }
            Self::Dismiss { key } => json!({"t": "dismiss", "key": key}),
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    id: u32,
    digest: Vec<u8>,
    reply_action: String,
    auto_cancel: bool,
    invoked_at: i64,
}

struct Pending {
    key: String,
    digest: Vec<u8>,
    reply_action: String,
    auto_cancel: bool,
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
    pending: HashMap<u64, Pending>,
    pending_by_key: HashMap<String, u64>,
    closing: HashMap<u32, Closing>,
    next_token: u64,
}

impl NotificationState {
    /// Accepts the legacy object keyed by Android notification key. Malformed rows are ignored.
    pub fn restore(state: &Value) -> Self {
        let mut result = Self::default();
        let Some(object) = state.as_object() else {
            return result;
        };
        for (key, row) in object {
            let Some(row) = row.as_object() else {
                continue;
            };
            let Some(id) = row.get("id").and_then(legacy_id) else {
                continue;
            };
            let digest = row
                .get("digest")
                .and_then(Value::as_str)
                .and_then(decode_hex)
                .unwrap_or_default();
            let entry = Entry {
                id,
                digest,
                reply_action: row
                    .get("reply")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                auto_cancel: row
                    .get("autoCancel")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                invoked_at: 0,
            };
            result.keys.insert(id, key.clone());
            result.entries.insert(key.clone(), entry);
        }
        result
    }

    pub fn snapshot(&self) -> Value {
        let mut state = Map::new();
        for (key, entry) in &self.entries {
            state.insert(
                key.clone(),
                json!({
                    "id": entry.id,
                    "digest": encode_hex(&entry.digest),
                    "reply": entry.reply_action,
                    "autoCancel": entry.auto_cancel,
                }),
            );
        }
        Value::Object(state)
    }

    pub fn tracked_len(&self) -> usize {
        self.entries.len()
    }

    /// Returns a Notify command only for a new or changed display payload.
    /// The adapter must call `notified` or `notify_failed` for each request,
    /// and pass Unix milliseconds to the action and timer methods.
    pub fn prepare_post(&mut self, post: &Post, display_digest: &[u8]) -> Vec<Command> {
        let key = &post.key;
        if !self.entries.contains_key(key)
            && !self.pending_by_key.contains_key(key)
            && self.entries.len() + self.pending_by_key.len() >= MAX_TRACKED
        {
            return Vec::new();
        }
        if let Some(request) = self.pending_by_key.get(key)
            && self
                .pending
                .get(request)
                .is_some_and(|pending| pending.digest == display_digest)
        {
            return Vec::new();
        }
        if let Some(old) = self.pending_by_key.remove(key) {
            self.pending.remove(&old);
        }
        if self
            .entries
            .get(key)
            .is_some_and(|entry| entry.digest == display_digest)
        {
            return Vec::new();
        }
        let request = self.token();
        self.pending_by_key.insert(key.clone(), request);
        self.pending.insert(
            request,
            Pending {
                key: key.clone(),
                digest: display_digest.to_vec(),
                reply_action: post.reply_action.clone(),
                auto_cancel: post.auto_cancel,
            },
        );
        vec![Command::Notify {
            request,
            post: Box::new(post.clone()),
            replaces_id: self.entries.get(key).map_or(0, |entry| entry.id),
        }]
    }

    /// Commit only a successful, current Notify reply. A stale reply is closed
    /// unless its id is already the currently tracked notification.
    pub fn notified(&mut self, request: u64, id: u32) -> Vec<Command> {
        let Some(pending) = self.pending.remove(&request) else {
            return self.close_untracked(id);
        };
        if self.pending_by_key.get(&pending.key) != Some(&request) {
            return self.close_untracked(id);
        }
        self.pending_by_key.remove(&pending.key);
        if id == 0 {
            return Vec::new();
        }
        if let Some(previous) = self.entries.get(&pending.key)
            && previous.id != id
        {
            self.keys.remove(&previous.id);
        }
        self.entries.insert(
            pending.key.clone(),
            Entry {
                id,
                digest: pending.digest,
                reply_action: pending.reply_action,
                auto_cancel: pending.auto_cancel,
                invoked_at: 0,
            },
        );
        self.keys.insert(id, pending.key);
        vec![Command::Persist(self.snapshot())]
    }

    pub fn notify_failed(&mut self, request: u64) {
        if let Some(pending) = self.pending.remove(&request)
            && self.pending_by_key.get(&pending.key) == Some(&request)
        {
            self.pending_by_key.remove(&pending.key);
        }
    }

    /// Remove local mappings before asking the Hub to close the row.
    pub fn remove(&mut self, key: &str) -> Vec<Command> {
        if let Some(request) = self.pending_by_key.remove(key) {
            self.pending.remove(&request);
        }
        let Some(entry) = self.entries.remove(key) else {
            return Vec::new();
        };
        self.keys.remove(&entry.id);
        vec![Command::Close(entry.id), Command::Persist(self.snapshot())]
    }

    /// Reconcile a complete Android key set. Removals are emitted in key order.
    pub fn sync(&mut self, live_keys: &[String]) -> Vec<Command> {
        let live: HashSet<&str> = live_keys.iter().map(String::as_str).collect();
        let stale: Vec<String> = self
            .entries
            .keys()
            .chain(self.pending_by_key.keys())
            .filter(|key| !live.contains(key.as_str()))
            .cloned()
            .collect();
        let mut commands = Vec::new();
        for key in stale {
            commands.extend(self.remove(&key));
        }
        commands
    }

    pub fn action_invoked(&mut self, id: u32, action: &str, now_ms: i64) -> Vec<Command> {
        if action == "inline-reply" {
            return Vec::new();
        }
        let key = if let Some(key) = self.keys.get(&id) {
            if let Some(entry) = self.entries.get_mut(key) {
                entry.invoked_at = now_ms;
            }
            Some(key.clone())
        } else if let Some(closing) = self.closing.get_mut(&id) {
            closing.entry.invoked_at = now_ms;
            Some(closing.key.clone())
        } else {
            None
        };
        key.map_or_else(Vec::new, |key| {
            vec![Command::Send(BridgeEvent::Invoke {
                key,
                action: left_utf16(action, 16),
            })]
        })
    }

    /// Replies are accepted only while the notification remains active.
    pub fn notification_replied(&self, id: u32, text: &str) -> Vec<Command> {
        let Some(key) = self.keys.get(&id) else {
            return Vec::new();
        };
        let Some(entry) = self.entries.get(key) else {
            return Vec::new();
        };
        if entry.reply_action.is_empty() {
            return Vec::new();
        }
        vec![Command::Send(BridgeEvent::Reply {
            key: key.clone(),
            action: entry.reply_action.clone(),
            text: left_utf16(text, 4096),
        })]
    }

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

    pub fn dismiss_due(&mut self, id: u32, token: u64, now_ms: i64) -> Vec<Command> {
        if self
            .closing
            .get(&id)
            .is_none_or(|closing| closing.token != token)
        {
            return Vec::new();
        }
        let Some(closing) = self.closing.remove(&id) else {
            return Vec::new();
        };
        if !closing.entry.auto_cancel
            && now_ms.saturating_sub(closing.entry.invoked_at) < TAP_GRACE_MS
        {
            return Vec::new();
        }
        vec![Command::Send(BridgeEvent::Dismiss { key: closing.key })]
    }

    fn close_untracked(&self, id: u32) -> Vec<Command> {
        if id != 0 && !self.keys.contains_key(&id) {
            vec![Command::Close(id)]
        } else {
            Vec::new()
        }
    }

    fn token(&mut self) -> u64 {
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        token
    }
}

fn legacy_id(value: &Value) -> Option<u32> {
    let number = value.as_f64()?;
    if number.is_finite() && number.fract() == 0.0 && (1.0..=u32::MAX as f64).contains(&number) {
        Some(number as u32)
    } else {
        None
    }
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)? as u8;
            let low = (pair[1] as char).to_digit(16)? as u8;
            Some((high << 4) | low)
        })
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0xf) as usize] as char);
    }
    result
}

fn left_utf16(value: &str, max_units: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|character| {
            units += character.len_utf16();
            units <= max_units
        })
        .collect()
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

    fn request(commands: &[Command]) -> u64 {
        match &commands[0] {
            Command::Notify { request, .. } => *request,
            other => panic!("expected Notify, got {other:?}"),
        }
    }

    fn tracked(state: &mut NotificationState, key: &str, id: u32, auto_cancel: bool) {
        let command = state.prepare_post(&post(key, auto_cancel, true), &[1, 2]);
        assert_eq!(state.notified(request(&command), id).len(), 1);
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
        assert!(
            state
                .prepare_post(&post("key", true, true), &[1, 2, 255])
                .is_empty()
        );
        let commands = state.prepare_post(&post("key", false, true), &[3]);
        assert!(matches!(
            &commands[..],
            [Command::Notify {
                replaces_id: 42,
                ..
            }]
        ));
        assert_eq!(
            state.notified(request(&commands), 43),
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
        let commands = state.sync(&["b".into()]);
        assert_eq!(
            commands,
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
        assert_eq!(
            action[0],
            Command::Send(BridgeEvent::Invoke {
                key: "key".into(),
                action: "😀".repeat(8)
            })
        );
        assert_eq!(
            BridgeEvent::Invoke {
                key: "key".into(),
                action: "default".into()
            }
            .to_json(),
            json!({"t": "invoke", "key": "key", "action": "default"})
        );
        let reply = state.notification_replied(7, &("x".repeat(4095) + "😀more"));
        assert_eq!(
            reply,
            vec![Command::Send(BridgeEvent::Reply {
                key: "key".into(),
                action: "a1".into(),
                text: "x".repeat(4095)
            })]
        );
        assert_eq!(
            BridgeEvent::Dismiss { key: "key".into() }.to_json(),
            json!({"t": "dismiss", "key": "key"})
        );
        assert!(state.notification_replied(99, "text").is_empty());
    }

    #[test]
    fn notify_preserves_typed_action_order_and_reply_wire_shape() {
        let message = json!({
            "key": "key",
            "pkg": "org.example.app",
            "content": true,
            "reply": {"action": "a1", "label": "Reply"},
            "actions": [
                {"id": "a2", "label": "Second"},
                {"id": "a3", "label": "Third"},
            ],
        });
        let post = messages::post(message.as_object().unwrap()).unwrap();
        let mut state = NotificationState::default();
        let commands = state.prepare_post(&post, &[1]);
        let Command::Notify { post, .. } = &commands[0] else {
            panic!("expected Notify")
        };
        assert_eq!(
            post.actions
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["default", "inline-reply", "a2", "a3"]
        );
        assert_eq!(
            BridgeEvent::Reply {
                key: "key".into(),
                action: "a1".into(),
                text: "hello".into()
            }
            .to_json(),
            json!({"t": "reply", "key": "key", "action": "a1", "text": "hello"})
        );
    }

    #[test]
    fn close_before_tap_suppresses_dismiss_for_non_auto_cancel() {
        let mut state = NotificationState::default();
        tracked(&mut state, "key", 7, false);
        let commands = state.notification_closed(7, 2);
        assert!(matches!(commands[0], Command::Persist(_)));
        let Command::ScheduleDismiss {
            id,
            token,
            delay_ms,
        } = commands[1]
        else {
            panic!("expected timer")
        };
        assert_eq!((id, delay_ms), (7, 500));
        assert_eq!(state.snapshot(), json!({}));
        assert_eq!(
            state.action_invoked(7, "default", 1_100),
            vec![Command::Send(BridgeEvent::Invoke {
                key: "key".into(),
                action: "default".into()
            })]
        );
        assert!(state.dismiss_due(7, token, 1_500).is_empty());
        assert!(state.dismiss_due(7, token, 4_500).is_empty());
    }

    #[test]
    fn dismissal_without_tap_and_auto_cancel_tap() {
        let mut state = NotificationState::default();
        tracked(&mut state, "plain", 7, false);
        let commands = state.notification_closed(7, 2);
        let Command::ScheduleDismiss { token, .. } = commands[1] else {
            panic!("expected timer")
        };
        assert_eq!(
            state.dismiss_due(7, token, 10_000),
            vec![Command::Send(BridgeEvent::Dismiss {
                key: "plain".into()
            })]
        );

        tracked(&mut state, "auto", 8, true);
        state.action_invoked(8, "a1", 10_100);
        let commands = state.notification_closed(8, 2);
        let Command::ScheduleDismiss { token, .. } = commands[1] else {
            panic!("expected timer")
        };
        assert_eq!(
            state.dismiss_due(8, token, 10_500),
            vec![Command::Send(BridgeEvent::Dismiss { key: "auto".into() })]
        );
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
    fn stale_notify_results_do_not_resurrect_removed_or_superseded_rows() {
        let mut state = NotificationState::default();
        let first = state.prepare_post(&post("key", false, false), &[1]);
        let second = state.prepare_post(&post("key", false, false), &[2]);
        assert_eq!(
            state.notified(request(&first), 11),
            vec![Command::Close(11)]
        );
        assert_eq!(state.notified(request(&second), 12).len(), 1);
        let third = state.prepare_post(&post("key", false, false), &[3]);
        assert_eq!(
            state.remove("key"),
            vec![Command::Close(12), Command::Persist(json!({}))]
        );
        assert_eq!(
            state.notified(request(&third), 13),
            vec![Command::Close(13)]
        );
        assert_eq!(state.snapshot(), json!({}));
    }

    #[test]
    fn failed_posts_preserve_old_state_and_allow_retry() {
        let mut state = NotificationState::default();
        tracked(&mut state, "key", 7, false);
        let next = state.prepare_post(&post("key", true, true), &[9]);
        state.notify_failed(request(&next));
        assert_eq!(state.snapshot()["key"]["id"], 7);
        assert!(matches!(
            state.prepare_post(&post("key", true, true), &[9])[0],
            Command::Notify { replaces_id: 7, .. }
        ));
    }

    #[test]
    fn reverting_a_pending_update_to_active_digest_needs_no_notify() {
        let mut state = NotificationState::default();
        tracked(&mut state, "key", 7, false);
        let pending = state.prepare_post(&post("key", true, true), &[9]);
        assert!(
            state
                .prepare_post(&post("key", false, true), &[1, 2])
                .is_empty()
        );
        assert_eq!(
            state.notified(request(&pending), 8),
            vec![Command::Close(8)]
        );
        assert_eq!(state.snapshot()["key"]["id"], 7);
    }

    #[test]
    fn tap_grace_uses_strict_three_second_boundary() {
        let mut state = NotificationState::default();
        tracked(&mut state, "key", 7, false);
        state.action_invoked(7, "default", 10_000);
        let commands = state.notification_closed(7, 2);
        let Command::ScheduleDismiss { token, .. } = commands[1] else {
            panic!("expected timer")
        };
        assert_eq!(
            state.dismiss_due(7, token, 13_000),
            vec![Command::Send(BridgeEvent::Dismiss { key: "key".into() })]
        );
    }

    #[test]
    fn cap_applies_to_new_rows_but_allows_updates() {
        let mut state = NotificationState::default();
        for n in 0..MAX_TRACKED {
            let key = format!("key{n}");
            tracked(&mut state, &key, n as u32 + 1, false);
        }
        assert!(
            state
                .prepare_post(&post("extra", false, false), &[1])
                .is_empty()
        );
        assert!(
            !state
                .prepare_post(&post("key0", false, false), &[9])
                .is_empty()
        );
    }

    #[test]
    fn invalid_restore_rows_and_duplicate_timer_are_ignored() {
        let mut state = NotificationState::restore(&json!({
            "zero": {"id": 0}, "negative": {"id": -1}, "fraction": {"id": 2.5},
            "valid": {"id": 4, "digest": "invalid", "reply": "a1"}
        }));
        assert_eq!(state.tracked_len(), 1);
        assert_eq!(state.snapshot()["valid"]["digest"], "");
        let commands = state.notification_closed(4, 2);
        let Command::ScheduleDismiss { token, .. } = commands[1] else {
            panic!("expected timer")
        };
        assert!(state.dismiss_due(4, token + 1, 10_000).is_empty());
        assert_eq!(
            state.dismiss_due(4, token, 10_000),
            vec![Command::Send(BridgeEvent::Dismiss {
                key: "valid".into()
            })]
        );
        assert!(state.dismiss_due(4, token, 10_000).is_empty());
    }
}
