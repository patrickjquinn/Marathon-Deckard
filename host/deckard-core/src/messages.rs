//! Bounded, typed payloads received from Android's untrusted bridge sockets.

use serde_json::{Map, Value, json};

use crate::ids::PackageName;
use crate::json;
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

fn integer(message: &Map<String, Value>, name: &str, min: i64, max: i64, fallback: i64) -> i64 {
    match message.get(name).and_then(Value::as_f64) {
        Some(value) => value.clamp(min as f64, max as f64) as i64,
        None => fallback,
    }
}

fn label(message: &Map<String, Value>, package: &PackageName) -> String {
    let label = validate::label(json::string(message, "label"));
    if label.is_empty() {
        package.as_str().to_owned()
    } else {
        label
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
    let custom = (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
    if !known.is_empty() {
        known.to_owned()
    } else if custom {
        format!("x-android.{value}")
    } else {
        String::new()
    }
}

fn package(message: &Map<String, Value>) -> Result<PackageName, MessageError> {
    PackageName::try_from(json::string(message, "pkg")).map_err(|_| MessageError::Package)
}

fn image(message: &Map<String, Value>, name: &str, max_bytes: usize) -> Vec<u8> {
    validate::base64_png(json::string(message, name), max_bytes).unwrap_or_default()
}

fn action_id(value: &str) -> bool {
    value.strip_prefix('a').is_some_and(|suffix| {
        (1..=2).contains(&suffix.len()) && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn conversation(object: &Map<String, Value>) -> Value {
    let messages = json::array(object, "messages");
    let empty = Map::new();
    let mut recent = Vec::new();
    for entry in &messages[messages.len().saturating_sub(10)..] {
        let item = entry.as_object().unwrap_or(&empty);
        recent.push(json!({
            "sender": json::text(item, "sender", 128),
            "text": json::text(item, "text", 1024),
            "time": integer(item, "time", 0, 1_i64 << 50, 0),
        }));
    }
    json!({
        "title": json::text(object, "title", 256),
        "self": json::text(object, "self", 128),
        "group": json::boolean(object, "group"),
        "messages": recent,
    })
}

fn progress(object: &Map<String, Value>) -> Option<i32> {
    let max = integer(object, "max", 0, i32::MAX.into(), 0);
    let current = integer(object, "current", 0, i32::MAX.into(), 0);
    if json::boolean(object, "indeterminate") {
        Some(-1)
    } else if max > 0 {
        Some((current * 100 / max).clamp(0, 100) as i32)
    } else {
        None
    }
}

/// Validates a `post`.
///
/// # Errors
/// The key or package is invalid.
pub fn post(message: &Map<String, Value>) -> Result<Post, MessageError> {
    let key = json::string(message, "key");
    if !validate::is_notification_key(key) {
        return Err(MessageError::Key);
    }
    let package = package(message)?;
    let empty = Map::new();
    let reply = json::object(message, "reply").unwrap_or(&empty);
    let reply_action = Some(json::string(reply, "action")).filter(|action| action_id(action));
    let reply_action = reply_action.unwrap_or("").to_owned();
    let mut reply_placeholder = String::new();
    let mut actions = Vec::new();
    if json::boolean(message, "content") {
        actions.push(("default".to_owned(), "Open".to_owned()));
    }
    if !reply_action.is_empty() {
        reply_placeholder = json::text(reply, "placeholder", 128);
        let label = json::text(reply, "label", 64);
        let label = if label.trim().is_empty() {
            "Reply".to_owned()
        } else {
            label
        };
        actions.push(("inline-reply".to_owned(), label));
    }
    let limit = actions.len() + 5;
    let extras = json::array(message, "actions")
        .iter()
        .filter_map(Value::as_object);
    for item in extras {
        if actions.len() >= limit {
            break;
        }
        let id = json::string(item, "id");
        let name = json::text(item, "label", 64);
        let taken = id == reply_action || actions.iter().any(|(existing, _)| existing == id);
        if action_id(id) && !taken && !name.trim().is_empty() {
            actions.push((id.to_owned(), name));
        }
    }
    Ok(Post {
        key: key.to_owned(),
        label: label(message, &package),
        package,
        title: json::text(message, "title", 256),
        body: json::text(message, "text", 4096),
        category: category(&json::text(message, "category", 64)),
        actions,
        reply_action,
        reply_placeholder,
        conversation: json::object(message, "conversation").map(conversation),
        image: image(message, "image", 512 * 1024),
        ongoing: json::boolean(message, "ongoing"),
        auto_cancel: json::boolean(message, "autoCancel"),
        silent: json::boolean(message, "silent"),
        urgency: integer(message, "urgency", 0, 2, 1) as u8,
        progress: json::object(message, "progress").and_then(progress),
    })
}

/// Validates a `media` update.
///
/// # Errors
/// The session id or package is invalid.
pub fn media(message: &Map<String, Value>) -> Result<Media, MessageError> {
    let session_id = integer(message, "sid", 0, i32::MAX.into(), 0) as i32;
    if session_id == 0 {
        return Err(MessageError::Session);
    }
    let package = package(message)?;
    let speed = message.get("speed").and_then(Value::as_f64).unwrap_or(1.0);
    let status = match json::string(message, "state") {
        "Playing" => "Playing",
        "Paused" => "Paused",
        _ => "Stopped",
    };
    let empty = Map::new();
    let can = json::object(message, "can").unwrap_or(&empty);
    Ok(Media {
        session_id,
        label: label(message, &package),
        package,
        status: status.to_owned(),
        position_ms: integer(message, "position", 0, 100_000_000_000, 0),
        duration_ms: integer(message, "duration", 0, 100_000_000_000, 0),
        speed: speed.clamp(-16.0, 16.0),
        title: json::text(message, "title", 256),
        artist: json::text(message, "artist", 256),
        album: json::text(message, "album", 256),
        art: message
            .contains_key("art")
            .then(|| image(message, "art", 768 * 1024)),
        can_play: json::boolean(can, "play"),
        can_pause: json::boolean(can, "pause"),
        can_next: json::boolean(can, "next"),
        can_previous: json::boolean(can, "previous"),
        can_seek: json::boolean(can, "seek"),
        can_stop: json::boolean(can, "stop"),
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
