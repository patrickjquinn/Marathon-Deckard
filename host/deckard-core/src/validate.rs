//! Bounds and cleaning for text and images from Android.

use std::sync::LazyLock;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use regex::Regex;
use unicode_general_category::GeneralCategory;

/// The longest prefix of `text` within `max_units` UTF-16 units.
#[must_use]
pub fn truncate(text: &str, max_units: usize) -> String {
    let mut units = 0;
    text.chars()
        .take_while(|character| {
            units += character.len_utf16();
            units <= max_units
        })
        .collect()
}

/// `value` without control and format characters other than newline and
/// tab, truncated to `max_units` UTF-16 units.
#[must_use]
pub fn clean_text(value: &str, max_units: usize) -> String {
    let printable = |character: &char| {
        let category = unicode_general_category::get_general_category(*character);
        matches!(character, '\n' | '\t')
            || !matches!(category, GeneralCategory::Control | GeneralCategory::Format)
    };
    let kept: String = value.chars().filter(printable).collect();
    truncate(&kept, max_units)
}

static RUNTIME_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?-u:\b)(?:waydroid|lineageos|lineage os)(?-u:\b)")
        .unwrap_or_else(|error| panic!("invalid fixed runtime-name pattern: {error}"))
});

/// Collapses every run of whitespace to one space and trims the ends.
#[must_use]
pub fn simplified(value: &str) -> String {
    let words: Vec<&str> = value.split_whitespace().collect();
    words.join(" ")
}

/// An Android app label: clean, at most 128 UTF-16 units, the runtime named
/// "Android", whitespace simplified. Empty when nothing usable is left.
#[must_use]
pub fn label(value: &str) -> String {
    simplified(&RUNTIME_NAME.replace_all(&clean_text(value, 128), "Android"))
}

/// Whether `value` is a usable Android notification key.
#[must_use]
pub fn is_notification_key(value: &str) -> bool {
    !value.is_empty() && value.encode_utf16().count() <= 512 && clean_text(value, 512) == value
}

/// Decodes standard base64 of at most `max_bytes`; `None` when it is
/// malformed or too large.
#[must_use]
pub fn base64_png(value: &str, max_bytes: usize) -> Option<Vec<u8>> {
    let max_encoded = max_bytes.saturating_add(2) / 3 * 4;
    if value.len() > max_encoded {
        return None;
    }
    BASE64_STANDARD
        .decode(value)
        .ok()
        .filter(|bytes| bytes.len() <= max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_format_and_control_characters_without_splitting_emoji() {
        assert_eq!(clean_text("a\0\u{202e}😀\n\tend", 5), "a😀\n\t");
        assert_eq!(clean_text("😀", 1), "");
        assert_eq!(truncate("a😀b", 3), "a😀");
        assert!(!is_notification_key("key\0"));
        assert!(!is_notification_key(""));
        assert!(is_notification_key("key"));
    }

    #[test]
    fn base64_is_strict_and_decoded_size_is_bounded() {
        assert_eq!(base64_png("YWJj", 3), Some(b"abc".to_vec()));
        assert_eq!(base64_png("YWJj", 2), None);
        assert!(base64_png("YW Jj", 8).is_none());
        assert!(base64_png("!!!!", 8).is_none());
    }
}
