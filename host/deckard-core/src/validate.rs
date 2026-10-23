use base64::{Engine, prelude::BASE64_STANDARD};
use unicode_general_category::{GeneralCategory, get_general_category};

use crate::Error;

pub fn clean_text(value: &str, max_units: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .filter(|character| {
            matches!(character, '\n' | '\t')
                || !matches!(
                    get_general_category(*character),
                    GeneralCategory::Control | GeneralCategory::Format
                )
        })
        .take_while(|character| {
            units += character.len_utf16();
            units <= max_units
        })
        .collect()
}

pub fn notification_key(value: &str) -> Result<&str, Error> {
    if value.is_empty() || value.encode_utf16().count() > 512 || clean_text(value, 512) != value {
        Err(Error::InvalidIdentity)
    } else {
        Ok(value)
    }
}

pub fn base64_png(value: &str, max_bytes: usize) -> Result<Vec<u8>, Error> {
    let max_encoded = max_bytes
        .saturating_add(2)
        .saturating_div(3)
        .saturating_mul(4);
    if value.len() > max_encoded {
        return Err(Error::ImageLimit);
    }
    let bytes = BASE64_STANDARD
        .decode(value)
        .map_err(|_| Error::InvalidImage)?;
    if bytes.len() > max_bytes {
        return Err(Error::ImageLimit);
    }
    Ok(bytes)
}

pub fn action_id(value: &str) -> bool {
    value.strip_prefix('a').is_some_and(|suffix| {
        (1..=2).contains(&suffix.len()) && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_format_and_control_characters_without_splitting_emoji() {
        assert_eq!(clean_text("a\0\u{202e}😀\n\tend", 5), "a😀\n\t");
        assert_eq!(clean_text("😀", 1), "");
        assert!(notification_key("key\0").is_err());
        assert!(notification_key("").is_err());
        assert!(notification_key("key").is_ok());
    }

    #[test]
    fn base64_is_strict_and_decoded_size_is_bounded() {
        assert_eq!(base64_png("YWJj", 3), Ok(b"abc".to_vec()));
        assert_eq!(base64_png("YWJj", 2), Err(Error::ImageLimit));
        assert!(base64_png("YW Jj", 8).is_err());
        assert!(base64_png("!!!!", 8).is_err());
        assert!(action_id("a0"));
        assert!(action_id("a99"));
        assert!(!action_id("a100"));
        assert!(!action_id("default"));
    }
}
