//! Marathon's permission groups as Android's permissions, and DeckardSystem's
//! manage replies as Marathon Settings takes them.

use std::collections::{HashMap, HashSet};

use deckard_core::{ids, json, validate};
pub use deckard_packages::VariantMap;
use serde_json::{Map, Value};

/// The most entries read from one Android list.
const MAX_ENTRIES: usize = 4096;

#[rustfmt::skip]
const PERMISSIONS: &[(&str, &[&str])] = &[
    ("location", &["ACCESS_FINE_LOCATION", "ACCESS_COARSE_LOCATION",
        "ACCESS_BACKGROUND_LOCATION"]),
    ("camera", &["CAMERA"]),
    ("microphone", &["RECORD_AUDIO"]),
    ("contacts", &["READ_CONTACTS", "WRITE_CONTACTS", "GET_ACCOUNTS"]),
    ("calendar", &["READ_CALENDAR", "WRITE_CALENDAR"]),
    ("storage", &["READ_EXTERNAL_STORAGE", "WRITE_EXTERNAL_STORAGE", "READ_MEDIA_IMAGES",
        "READ_MEDIA_VIDEO", "READ_MEDIA_AUDIO"]),
    ("notifications", &["POST_NOTIFICATIONS"]),
    ("telephony", &["READ_PHONE_STATE", "READ_PHONE_NUMBERS", "CALL_PHONE",
        "ANSWER_PHONE_CALLS", "ADD_VOICEMAIL", "USE_SIP", "ACCEPT_HANDOVER", "READ_CALL_LOG",
        "WRITE_CALL_LOG", "PROCESS_OUTGOING_CALLS"]),
    ("sms", &["SEND_SMS", "RECEIVE_SMS", "READ_SMS", "RECEIVE_WAP_PUSH", "RECEIVE_MMS"]),
    ("bluetooth", &["BLUETOOTH_SCAN", "BLUETOOTH_CONNECT", "BLUETOOTH_ADVERTISE",
        "NEARBY_WIFI_DEVICES", "UWB_RANGING"]),
    ("sensors", &["BODY_SENSORS", "BODY_SENSORS_BACKGROUND", "ACTIVITY_RECOGNITION"]),
];

/// The kinds of Android special access Settings manages.
#[rustfmt::skip]
pub const SPECIAL_KINDS: &[&str] = &[
    "all_files", "install_apps", "overlay", "write_settings", "usage", "notification_listener",
    "dnd", "alarms", "pip", "battery", "accessibility", "device_admin",
];

/// Whether an app list says which grants Android fixed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fixed {
    Shown,
    Hidden,
}

/// The Android permissions of the Marathon permission `name`.
#[must_use]
pub fn permission(name: &str) -> Option<Vec<String>> {
    let (_, values) = PERMISSIONS.iter().find(|(key, _)| *key == name)?;
    let mut out = Vec::new();
    for value in *values {
        out.push(format!("android.permission.{value}"));
    }
    Some(out)
}

/// The Marathon permission an Android permission belongs to.
#[must_use]
pub fn marathon_permission(android: &str) -> Option<&'static str> {
    let short = android.strip_prefix("android.permission.")?;
    let found = PERMISSIONS
        .iter()
        .find(|(_, values)| values.contains(&short));
    found.map(|(name, _)| *name)
}

fn flag(value: &Map<String, Value>, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

fn number(value: &Map<String, Value>, key: &str, max: i64) -> i64 {
    let number = value.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    number.clamp(0.0, max as f64) as i64
}

/// The objects of the list at `key`, at most `MAX_ENTRIES` read.
fn objects<'a>(value: &'a Map<String, Value>, key: &str) -> Vec<&'a Map<String, Value>> {
    let entries = json::array(value, key).iter().take(MAX_ENTRIES);
    entries.filter_map(Value::as_object).collect()
}

/// An app's settings page, or an empty map for a bad package.
#[must_use]
pub fn app(value: &Map<String, Value>) -> VariantMap {
    let pkg = json::string(value, "pkg");
    if !ids::is_package_name(pkg) {
        return VariantMap::new();
    }
    let battery = match json::string(value, "battery") {
        mode @ ("unrestricted" | "optimized") => mode,
        _ => "optimized",
    };
    deckard_packages::dict([
        ("pkg", pkg.into()),
        (
            "label",
            validate::label(json::string(value, "label"), pkg).into(),
        ),
        ("versionName", json::text(value, "versionName", 64).into()),
        ("versionCode", number(value, "versionCode", 1 << 53).into()),
        ("system", flag(value, "system").into()),
        ("appBytes", number(value, "appBytes", 1 << 50).into()),
        ("dataBytes", number(value, "dataBytes", 1 << 50).into()),
        ("cacheBytes", number(value, "cacheBytes", 1 << 50).into()),
        ("notifications", flag(value, "notifications").into()),
        ("links", flag(value, "links").into()),
        ("domains", domains(value).into()),
        ("battery", battery.into()),
        ("permissions", permissions(value).into()),
    ])
}

/// At most 50 distinct link domains, each `host` or `*.host`.
fn domains(value: &Map<String, Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let entries = json::array(value, "domains").iter().take(MAX_ENTRIES);
    for domain in entries {
        let domain = domain.as_str().unwrap_or("");
        let host = domain.strip_prefix("*.").unwrap_or(domain);
        let part = |part: &str| {
            let allowed = |b: u8| b.is_ascii_alphanumeric() || b == b'-';
            !part.is_empty() && part.bytes().all(allowed)
        };
        let valid = domain.len() <= 253 && host.contains('.') && host.split('.').all(part);
        if valid && out.len() < 50 && !out.iter().any(|known| known == domain) {
            out.push(domain.to_owned());
        }
    }
    out
}

/// Each Marathon permission the app asks for, granted when any of its
/// Android permissions is and fixed when all are.
fn permissions(value: &Map<String, Value>) -> Vec<VariantMap> {
    let mut states: HashMap<&str, (bool, bool)> = HashMap::new();
    for entry in objects(value, "perms") {
        let name = marathon_permission(json::string(entry, "name"));
        if let Some(name) = name.filter(|name| *name != "notifications") {
            let state = states.entry(name).or_insert((false, true));
            state.0 |= flag(entry, "granted");
            state.1 &= flag(entry, "fixed");
        }
    }
    let mut out = Vec::new();
    for (name, _) in PERMISSIONS {
        if let Some(&(granted, fixed)) = states.get(name) {
            let (granted, fixed) = (granted.into(), fixed.into());
            out.push(deckard_packages::dict([
                ("name", (*name).into()),
                ("granted", granted),
                ("fixed", fixed),
            ]));
        }
    }
    out
}

/// At most 500 apps, each package once.
#[must_use]
pub fn apps(value: &Map<String, Value>, fixed: Fixed) -> Vec<VariantMap> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in objects(value, "apps") {
        let pkg = json::string(item, "pkg");
        if !ids::is_package_name(pkg) || !seen.insert(pkg) {
            continue;
        }
        let mut map = deckard_packages::dict([
            ("pkg", pkg.into()),
            (
                "label",
                validate::label(json::string(item, "label"), pkg).into(),
            ),
            ("granted", flag(item, "granted").into()),
        ]);
        if fixed == Fixed::Shown {
            map.extend(deckard_packages::dict([(
                "fixed",
                flag(item, "fixed").into(),
            )]));
        }
        out.push(map);
        if out.len() == 500 {
            break;
        }
    }
    out
}

/// microG's state; push only while it is enabled, at most 10 accounts.
#[must_use]
pub fn google(value: &Map<String, Value>) -> VariantMap {
    let mut accounts = Vec::new();
    let entries = json::array(value, "accounts").iter().take(MAX_ENTRIES);
    for account in entries {
        let account = validate::clean_text(account.as_str().unwrap_or(""), 254)
            .trim()
            .to_owned();
        if accounts.len() < 10 && !account.is_empty() && !account.contains('\n') {
            accounts.push(account);
        }
    }
    let enabled = flag(value, "enabled");
    deckard_packages::dict([
        ("installed", flag(value, "installed").into()),
        ("enabled", enabled.into()),
        ("push", (enabled && flag(value, "push")).into()),
        ("accounts", accounts.into()),
    ])
}
