use serde::{Deserialize, Serialize};

use crate::Error;

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

macro_rules! identity {
    ($name:ident, $valid:expr) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = Error;

            fn try_from(value: String) -> Result<Self, Error> {
                if ($valid)(&value) {
                    Ok(Self(value))
                } else {
                    Err(Error::InvalidIdentity)
                }
            }
        }

        impl TryFrom<&str> for $name {
            type Error = Error;

            fn try_from(value: &str) -> Result<Self, Error> {
                Self::try_from(value.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

identity!(PackageName, is_package_name);
identity!(AppId, |value: &str| value
    .strip_prefix("deckard.")
    .is_some_and(is_package_name));
identity!(StoreAppId, |value: &str| value
    .strip_prefix("android:")
    .is_some_and(is_package_name));
identity!(TransferId, |value: &str| value.len() == 16
    && value.bytes().all(
        |byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
    ));

impl From<&PackageName> for AppId {
    fn from(package: &PackageName) -> Self {
        Self(format!("deckard.{}", package.as_str()))
    }
}

impl From<&PackageName> for StoreAppId {
    fn from(package: &PackageName) -> Self {
        Self(format!("android:{}", package.as_str()))
    }
}

impl AppId {
    pub fn package(&self) -> PackageName {
        PackageName(self.0["deckard.".len()..].to_owned())
    }

    pub fn from_legacy(value: &str) -> Result<Self, Error> {
        let package = value
            .strip_prefix("waydroid.")
            .ok_or(Error::InvalidIdentity)?;
        Ok(Self::from(&PackageName::try_from(package)?))
    }

    pub fn legacy(&self) -> String {
        format!("waydroid.{}", self.package().as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AndroidUid(pub u32);

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert construction and wire validation"
)]
mod tests {
    use super::*;

    #[test]
    fn packages_reject_invalid_components_and_lengths() {
        for invalid in [
            "", ".app", "app.", "a..b", "9app", "a.9b", "app/name", "app\0x", "ápp",
        ] {
            assert!(PackageName::try_from(invalid).is_err(), "{invalid:?}");
        }
        assert!(PackageName::try_from("a".repeat(256)).is_err());
        assert!(PackageName::try_from("a".repeat(255)).is_ok());
        assert!(PackageName::try_from("org.example.App_1").is_ok());
    }

    #[test]
    fn serde_cannot_bypass_validation() {
        assert!(serde_json::from_str::<PackageName>("\"a/../b\"").is_err());
        assert!(serde_json::from_str::<AppId>("\"waydroid.org.example\"").is_err());
        let package = PackageName::try_from("org.example").unwrap();
        assert_eq!(serde_json::to_string(&package).unwrap(), "\"org.example\"");
    }

    #[test]
    fn identities_migrate_without_mixing_namespaces() {
        let package = PackageName::try_from("org.example").unwrap();
        let app = AppId::from(&package);
        assert_eq!(app.as_str(), "deckard.org.example");
        assert_eq!(app.package(), package);
        assert_eq!(AppId::from_legacy(&app.legacy()).unwrap(), app);
        assert_eq!(StoreAppId::from(&package).as_str(), "android:org.example");
        assert!(AppId::from_legacy("waydroid.a/../b").is_err());
    }

    #[test]
    fn transfer_ids_are_exact_lowercase_hex() {
        assert!(TransferId::try_from("0123456789abcdef").is_ok());
        for invalid in [
            "0123456789ABCDEF",
            "123",
            "../0123456789abc",
            "0123456789abcdeg",
        ] {
            assert!(TransferId::try_from(invalid).is_err());
        }
    }
}
