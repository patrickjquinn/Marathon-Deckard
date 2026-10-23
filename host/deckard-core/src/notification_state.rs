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

/// A tracked row; `id` is 0 while its Notify is pending.
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
    pending: HashMap<u64, (String, Entry)>,
    pending_by_key: HashMap<String, u64>,
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

    /// Returns a Notify command only for a new or changed display payload.
    /// The adapter must call `notified` or `notify_failed` for each request,
    /// and pass Unix milliseconds to the action and timer methods.
    #[must_use]
    pub fn prepare_post(&mut self, post: &Post, display_digest: &[u8]) -> Vec<Command> {
        let key = &post.key;
        let known = self.entries.contains_key(key) || self.pending_by_key.contains_key(key);
        if !known && self.entries.len() + self.pending_by_key.len() >= MAX_TRACKED {
            return Vec::new();
        }
        let pending = self
            .pending_by_key
            .get(key)
            .and_then(|request| self.pending.get(request));
        if pending.is_some_and(|(_, pending)| pending.digest == display_digest) {
            return Vec::new();
        }
        if let Some(old) = self.pending_by_key.remove(key) {
            self.pending.remove(&old);
        }
        let current = self.entries.get(key);
        if current.is_some_and(|entry| entry.digest == display_digest) {
            return Vec::new();
        }
        let replaces_id = current.map_or(0, |entry| entry.id);
        let request = self.token();
        let entry = Entry {
            id: 0,
            digest: display_digest.to_vec(),
            reply_action: post.reply_action.clone(),
            auto_cancel: post.auto_cancel,
            invoked_at: 0,
        };
        self.pending_by_key.insert(key.clone(), request);
        self.pending.insert(request, (key.clone(), entry));
        vec![Command::Notify {
            request,
            post: Box::new(post.clone()),
            replaces_id,
        }]
    }

    /// Commit only a successful, current Notify reply. A stale reply is closed
    /// unless its id is already the currently tracked notification.
    #[must_use]
    pub fn notified(&mut self, request: u64, id: u32) -> Vec<Command> {
        let Some((key, mut entry)) = self.pending.remove(&request) else {
            return self.close_untracked(id);
        };
        if self.pending_by_key.get(&key) != Some(&request) {
            return self.close_untracked(id);
        }
        self.pending_by_key.remove(&key);
        if id == 0 {
            return Vec::new();
        }
        if let Some(previous) = self.entries.get(&key)
            && previous.id != id
        {
            self.keys.remove(&previous.id);
        }
        entry.id = id;
        self.entries.insert(key.clone(), entry);
        self.keys.insert(id, key);
        vec![Command::Persist(self.snapshot())]
    }

    pub fn notify_failed(&mut self, request: u64) {
        if let Some((key, _)) = self.pending.remove(&request)
            && self.pending_by_key.get(&key) == Some(&request)
        {
            self.pending_by_key.remove(&key);
        }
    }

    /// Remove local mappings before asking the Hub to close the row.
    #[must_use]
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
    #[must_use]
    pub fn sync(&mut self, live_keys: &[String]) -> Vec<Command> {
        let live: HashSet<&str> = live_keys.iter().map(String::as_str).collect();
        let tracked = self.entries.keys().chain(self.pending_by_key.keys());
        let stale: Vec<String> = tracked
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
            serde_json::to_value(BridgeEvent::Invoke {
                key: "key".into(),
                action: "default".into()
            })
            .unwrap(),
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
            serde_json::to_value(BridgeEvent::Dismiss { key: "key".into() }).unwrap(),
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
            serde_json::to_value(BridgeEvent::Reply {
                key: "key".into(),
                action: "a1".into(),
                text: "hello".into()
            })
            .unwrap(),
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
        let _ = state.action_invoked(8, "a1", 10_100);
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
        let _ = state.action_invoked(7, "default", 10_000);
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
        assert_eq!(state.snapshot().as_object().unwrap().len(), 1);
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
