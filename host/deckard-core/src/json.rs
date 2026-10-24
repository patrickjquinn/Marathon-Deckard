//! Lenient field access on JSON objects from Android: a missing or
//! mistyped field reads as empty.

use serde_json::{Map, Value};

use crate::validate;

/// The string at `key`.
#[must_use]
pub fn string<'a>(object: &'a Map<String, Value>, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The string at `key`, cleaned and bounded by [`validate::clean_text`].
#[must_use]
pub fn text(object: &Map<String, Value>, key: &str, max_units: usize) -> String {
    validate::clean_text(string(object, key), max_units)
}

/// The boolean at `key`.
#[must_use]
pub fn boolean(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The array at `key`.
#[must_use]
pub fn array<'a>(object: &'a Map<String, Value>, key: &str) -> &'a [Value] {
    object
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The object at `key`; `None` when there is none.
#[must_use]
pub fn object<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    object.get(key).and_then(Value::as_object)
}
