//! Bounded, typed payloads received from Android's untrusted bridge sockets.

use serde_json::{Map, Value, json};

use crate::ids::PackageName;
use crate::validate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageError {
    Key,
    Package,
    Session,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Post {
    pub key: String,
    pub package: PackageName,
    pub label: String,
    pub title: String,
    pub body: String,
    pub category: String,
    pub actions: Vec<(String, String)>,
    pub reply_action: String,
    pub reply_placeholder: String,
    pub conversation: Option<Value>,
    pub image: Vec<u8>,
    pub ongoing: bool,
    pub auto_cancel: bool,
    pub silent: bool,
    pub urgency: u8,
    /// `None` is no progress, `Some(-1)` is indeterminate.
    pub progress: Option<i32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    pub session_id: i32,
    pub package: PackageName,
    pub label: String,
    pub status: String,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub speed: f64,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Presence matters: `Some(empty)` means the sender explicitly cleared art.
    pub art: Option<Vec<u8>>,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    pub can_stop: bool,
}

fn field<'a>(message: &'a Map<String, Value>, name: &str) -> &'a Value {
    message.get(name).unwrap_or(&Value::Null)
}

fn string<'a>(message: &'a Map<String, Value>, name: &str) -> &'a str {
    field(message, name).as_str().unwrap_or("")
}

fn boolean(message: &Map<String, Value>, name: &str) -> bool {
    field(message, name).as_bool().unwrap_or(false)
}

fn text(message: &Map<String, Value>, name: &str, max_units: usize) -> String {
    validate::clean_text(string(message, name), max_units)
}

fn integer(message: &Map<String, Value>, name: &str, min: i64, max: i64, fallback: i64) -> i64 {
    let value = field(message, name).as_f64().unwrap_or(fallback as f64);
    if !value.is_finite() {
        return fallback;
    }
    value.clamp(min as f64, max as f64) as i64
}

fn label(message: &Map<String, Value>, package: &PackageName) -> String {
    let compact = validate::label(string(message, "label"));
    if compact.is_empty() {
        package.as_str().to_owned()
    } else {
        compact
    }
}

fn category(value: &str) -> String {
    let known = match value {
        "msg" => "im.received",
        "email" => "email.arrived",
        "call" => "call.incoming",
        "missed_call" => "call.unanswered",
        "progress" => "transfer",
        "social" => "presence",
        "err" => "device.error",
        "sys" => "device",
        _ => "",
    };
    if !known.is_empty() {
        known.to_owned()
    } else if (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        format!("x-android.{value}")
    } else {
        String::new()
    }
}

fn package(message: &Map<String, Value>) -> Result<PackageName, MessageError> {
    PackageName::try_from(string(message, "pkg")).map_err(|_| MessageError::Package)
}

fn image(message: &Map<String, Value>, name: &str, max_bytes: usize) -> Vec<u8> {
    validate::base64_png(string(message, name), max_bytes).unwrap_or_default()
}

fn conversation(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let messages = field(object, "messages")
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let recent = messages.iter().rev().take(10).rev().map(|entry| {
        let empty = Map::new();
        let item = entry.as_object().unwrap_or(&empty);
        json!({
            "sender": text(item, "sender", 128),
            "text": text(item, "text", 1024),
            "time": integer(item, "time", 0, 1_i64 << 50, 0),
        })
    });
    Some(json!({
        "title": text(object, "title", 256),
        "self": text(object, "self", 128),
        "group": boolean(object, "group"),
        "messages": recent.collect::<Vec<_>>(),
    }))
}

fn action_id(value: &str) -> bool {
    value.strip_prefix('a').is_some_and(|suffix| {
        (1..=2).contains(&suffix.len()) && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

pub fn post(message: &Map<String, Value>) -> Result<Post, MessageError> {
    let key = string(message, "key");
    if !validate::is_notification_key(key) {
        return Err(MessageError::Key);
    }
    let package = package(message)?;
    let reply = field(message, "reply").as_object();
    let reply_action = reply
        .map(|item| string(item, "action"))
        .filter(|action| action_id(action))
        .unwrap_or("")
        .to_owned();
    let reply_placeholder = if reply_action.is_empty() {
        String::new()
    } else {
        reply
            .map(|item| text(item, "placeholder", 128))
            .unwrap_or_default()
    };
    let mut actions = Vec::new();
    if boolean(message, "content") {
        actions.push(("default".to_owned(), "Open".to_owned()));
    }
    if !reply_action.is_empty() {
        let reply_label = reply
            .map(|item| text(item, "label", 64))
            .unwrap_or_default();
        actions.push((
            "inline-reply".to_owned(),
            if reply_label.trim().is_empty() {
                "Reply".to_owned()
            } else {
                reply_label
            },
        ));
    }
    if let Some(extras) = field(message, "actions").as_array() {
        for item in extras {
            let Some(item) = item.as_object() else {
                continue;
            };
            let id = string(item, "id");
            let name = text(item, "label", 64);
            if !action_id(id)
                || id == reply_action
                || name.trim().is_empty()
                || actions.iter().any(|(existing, _)| existing == id)
            {
                continue;
            }
            actions.push((id.to_owned(), name));
            if actions.len()
                >= 5 + usize::from(boolean(message, "content"))
                    + usize::from(!reply_action.is_empty())
            {
                break;
            }
        }
    }
    let progress = field(message, "progress")
        .as_object()
        .map(|p| {
            let max = integer(p, "max", 0, i32::MAX as i64, 0);
            let current = integer(p, "current", 0, i32::MAX as i64, 0);
            if boolean(p, "indeterminate") {
                -1
            } else if max > 0 {
                (current * 100 / max).clamp(0, 100) as i32
            } else {
                -2
            }
        })
        .filter(|value| *value != -2);
    Ok(Post {
        key: key.to_owned(),
        label: label(message, &package),
        package,
        title: text(message, "title", 256),
        body: text(message, "text", 4096),
        category: category(&text(message, "category", 64)),
        actions,
        reply_action,
        reply_placeholder,
        conversation: conversation(field(message, "conversation")),
        image: image(message, "image", 512 * 1024),
        ongoing: boolean(message, "ongoing"),
        auto_cancel: boolean(message, "autoCancel"),
        silent: boolean(message, "silent"),
        urgency: integer(message, "urgency", 0, 2, 1) as u8,
        progress,
    })
}

pub fn media(message: &Map<String, Value>) -> Result<Media, MessageError> {
    let session_id = integer(message, "sid", 0, i32::MAX as i64, 0) as i32;
    if session_id == 0 {
        return Err(MessageError::Session);
    }
    let package = package(message)?;
    let speed = field(message, "speed").as_f64().unwrap_or(1.0);
    let speed = if speed.is_finite() {
        speed.clamp(-16.0, 16.0)
    } else {
        1.0
    };
    let status = match string(message, "state") {
        "Playing" => "Playing",
        "Paused" => "Paused",
        _ => "Stopped",
    };
    let can = field(message, "can").as_object();
    let capability = |name| can.is_some_and(|values| boolean(values, name));
    Ok(Media {
        session_id,
        label: label(message, &package),
        package,
        status: status.to_owned(),
        position_ms: integer(message, "position", 0, 100_000_000_000, 0),
        duration_ms: integer(message, "duration", 0, 100_000_000_000, 0),
        speed,
        title: text(message, "title", 256),
        artist: text(message, "artist", 256),
        album: text(message, "album", 256),
        art: message
            .contains_key("art")
            .then(|| image(message, "art", 768 * 1024)),
        can_play: capability("play"),
        can_pause: capability("pause"),
        can_next: capability("next"),
        can_previous: capability("previous"),
        can_seek: capability("seek"),
        can_stop: capability("stop"),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests construct fixed JSON messages")]
mod tests {
    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn post_matches_action_progress_and_category_contract() {
        let actions = (0..9)
            .map(|id| json!({"id": format!("a{id}"), "label": format!("Action {id}")}))
            .collect::<Vec<_>>();
        let message = object(json!({
            "key": "0|org.lineageos.jelly|1|null|10123",
            "pkg": "org.lineageos.jelly",
            "label": "Jelly",
            "title": "file.zip",
            "text": "Downloading",
            "content": true,
            "urgency": 7,
            "category": "progress",
            "reply": {"action": "a1", "placeholder": "Message"},
            "actions": actions,
            "progress": {"max": 200, "current": 50},
        }));
        let parsed = post(&message).unwrap();
        assert_eq!(parsed.urgency, 2);
        assert_eq!(parsed.progress, Some(25));
        assert_eq!(parsed.category, "transfer");
        assert_eq!(parsed.reply_action, "a1");
        assert_eq!(parsed.reply_placeholder, "Message");
        assert_eq!(parsed.actions.len(), 7);
        assert_eq!(parsed.actions[0], ("default".to_owned(), "Open".to_owned()));
        assert_eq!(
            parsed.actions[1],
            ("inline-reply".to_owned(), "Reply".to_owned())
        );
        assert!(!parsed.actions.iter().any(|(id, _)| id == "a1"));

        let mut changed = message.clone();
        changed.insert("progress".to_owned(), json!({"indeterminate": true}));
        assert_eq!(post(&changed).unwrap().progress, Some(-1));
        changed.insert("progress".to_owned(), json!({"max": 0}));
        assert_eq!(post(&changed).unwrap().progress, None);
        changed.insert("pkg".to_owned(), json!("../../x"));
        assert_eq!(post(&changed), Err(MessageError::Package));
        changed.insert("pkg".to_owned(), json!("org.example"));
        changed.insert("key".to_owned(), json!("k".repeat(600)));
        assert_eq!(post(&changed), Err(MessageError::Key));
    }

    #[test]
    fn conversation_keeps_only_bounded_recent_messages() {
        let messages = (0..15)
            .map(|index| json!({"sender": "Ann", "text": format!("m{index}"), "time": 1000 + index, "extra": "drop"}))
            .collect::<Vec<_>>();
        let parsed = post(&object(json!({
            "key": "k",
            "pkg": "com.example",
            "conversation": {"title": "Group", "group": true, "messages": messages},
        })))
        .unwrap();
        let conversation = parsed.conversation.unwrap();
        let recent = conversation["messages"].as_array().unwrap();
        assert_eq!(recent.len(), 10);
        assert_eq!(recent[0]["text"], "m5");
        assert!(recent[0].get("extra").is_none());
        assert_eq!(parsed.label, "com.example");
    }

    #[test]
    fn media_bounds_and_art_presence_match_bridge_contract() {
        let message = object(json!({
            "sid": 3,
            "pkg": "org.lineageos.eleven",
            "state": "Dancing",
            "position": -5,
            "speed": 1e9,
            "can": {"next": true},
        }));
        let parsed = media(&message).unwrap();
        assert_eq!(parsed.status, "Stopped");
        assert_eq!(parsed.position_ms, 0);
        assert_eq!(parsed.speed, 16.0);
        assert!(parsed.can_next && !parsed.can_play && parsed.art.is_none());
        let mut changed = message;
        changed.insert("art".to_owned(), json!("!!!!"));
        assert_eq!(media(&changed).unwrap().art, Some(Vec::new()));
        changed.insert("sid".to_owned(), json!(0));
        assert_eq!(media(&changed), Err(MessageError::Session));
    }

    #[test]
    fn labels_categories_and_malformed_fields_are_bounded() {
        let parsed = post(&object(json!({
            "key": "key",
            "pkg": "org.example",
            "label": "LineageOS  Settings",
            "category": "alarm",
            "title": "a\u{0}b\u{202e}c",
            "reply": {"action": "not-valid", "placeholder": "ignore"},
        })))
        .unwrap();
        assert_eq!(parsed.label, "Android Settings");
        assert_eq!(parsed.category, "x-android.alarm");
        assert_eq!(parsed.title, "abc");
        assert!(parsed.reply_action.is_empty());
        assert!(parsed.reply_placeholder.is_empty());
    }
}
