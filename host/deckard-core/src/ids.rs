//! Android package names.

use crate::Error;

/// Whether `value` is a dotted Android package name of at most 255 bytes.
#[must_use]
pub fn is_package_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('.').all(|part| {
            part.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

/// A validated Android package name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PackageName(String);

impl PackageName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for PackageName {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Error> {
        if is_package_name(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::InvalidIdentity)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_reject_invalid_components_and_lengths() {
        for invalid in [
            "", ".app", "app.", "a..b", "9app", "a.9b", "app/name", "app\0x", "ápp",
        ] {
            assert!(PackageName::try_from(invalid).is_err(), "{invalid:?}");
        }
        assert!(!is_package_name(&"a".repeat(256)));
        assert!(is_package_name(&"a".repeat(255)));
        assert!(is_package_name("org.example.App_1"));
    }
}
