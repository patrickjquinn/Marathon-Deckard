//! Qt 6 QDataStream layout used by the previous C++ notification handler.

use deckard_core::messages::Post;
use sha2::{Digest, Sha256};

enum Hint<'a> {
    Text(&'a str),
    Bool(bool),
    Byte(u8),
    Int(i32),
}

fn u32_be(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn qstring(bytes: &mut Vec<u8>, value: &str) {
    let utf16: Vec<u16> = value.encode_utf16().collect();
    u32_be(bytes, (utf16.len() * 2) as u32);
    for unit in utf16 {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
}

fn qbytearray(bytes: &mut Vec<u8>, value: &[u8]) {
    if value.is_empty() {
        u32_be(bytes, u32::MAX); // The old handler's default QByteArray is null.
        return;
    }
    u32_be(bytes, value.len() as u32);
    bytes.extend_from_slice(value);
}

fn qstringlist(bytes: &mut Vec<u8>, value: &[String]) {
    u32_be(bytes, value.len() as u32);
    for item in value {
        qstring(bytes, item);
    }
}

fn qvariant(bytes: &mut Vec<u8>, value: Hint<'_>) {
    let type_id = match value {
        Hint::Bool(_) => 1,
        Hint::Int(_) => 2,
        Hint::Text(_) => 10,
        Hint::Byte(_) => 37,
    };
    u32_be(bytes, type_id);
    bytes.push(0); // QVariant::isNull()
    match value {
        Hint::Text(value) => qstring(bytes, value),
        Hint::Bool(value) => bytes.push(u8::from(value)),
        Hint::Byte(value) => bytes.push(value),
        Hint::Int(value) => bytes.extend_from_slice(&value.to_be_bytes()),
    }
}

pub(crate) fn display_digest(post: &Post) -> [u8; 32] {
    let mut bytes = Vec::new();
    qstring(&mut bytes, &post.label);
    qstring(&mut bytes, &post.title);
    qstring(&mut bytes, &post.body);
    let actions: Vec<String> = post
        .actions
        .iter()
        .flat_map(|(id, label)| [id.clone(), label.clone()])
        .collect();
    qstringlist(&mut bytes, &actions);

    // QVariantMap uses sorted QString keys; suppress-sound is deliberately
    // excluded so a silent-only update does not create another banner.
    let desktop_entry = format!("waydroid.{}", post.package.as_str());
    let conversation = post.conversation.as_ref().map(serde_json::Value::to_string);
    let mut hints = vec![
        ("desktop-entry", Hint::Text(&desktop_entry)),
        ("resident", Hint::Bool(post.ongoing)),
        ("urgency", Hint::Byte(post.urgency)),
    ];
    if !post.category.is_empty() {
        hints.push(("category", Hint::Text(&post.category)));
    }
    if !post.reply_action.is_empty() && !post.reply_placeholder.is_empty() {
        hints.push((
            "x-kde-reply-placeholder-text",
            Hint::Text(&post.reply_placeholder),
        ));
    }
    if let Some(conversation) = conversation.as_deref() {
        hints.push(("x-marathon-conversation", Hint::Text(conversation)));
    }
    if let Some(progress) = post.progress {
        hints.push(("value", Hint::Int(progress)));
    }
    hints.sort_by_key(|(key, _)| *key);
    u32_be(&mut bytes, hints.len() as u32);
    for (key, value) in hints {
        qstring(&mut bytes, key);
        qvariant(&mut bytes, value);
    }
    qbytearray(&mut bytes, &post.image);
    Sha256::digest(bytes).into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixed test payloads")]
mod tests {
    use super::*;
    use deckard_core::messages;
    use serde_json::json;

    #[test]
    fn known_qt6_digest() {
        let value = json!({"key":"k","pkg":"org.example.App","label":"Example","title":"Title","text":"Body","content":true,"urgency":1});
        let post = messages::post(value.as_object().unwrap()).unwrap();
        assert_eq!(
            hex(&display_digest(&post)),
            "fafa37e96e7ab32bea1805a3e3ab37c19d596f7c21da3e0eb26c7998a2564db5"
        );
    }

    #[test]
    fn qt6_digest_with_optional_hints_and_image() {
        let value = json!({
            "key": "k", "pkg": "org.example.App", "label": "Example",
            "title": "Title", "text": "Body", "content": true,
            "reply": {"action": "a1", "label": "Reply", "placeholder": "Type here"},
            "category": "msg", "urgency": 2, "ongoing": true,
            "progress": {"max": 10, "current": 5},
            "conversation": {"title": "Thread", "self": "Me", "group": false, "messages": []},
        });
        let mut post = messages::post(value.as_object().unwrap()).unwrap();
        post.image = vec![1, 2, 3];
        assert_eq!(
            hex(&display_digest(&post)),
            "5ed0c73e352e201ac627d788a693d4345eaa01027ee499e4f215ff3d0fc7b12b"
        );
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
