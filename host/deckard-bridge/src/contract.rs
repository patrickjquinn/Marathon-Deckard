use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use deckard_core::ids::PackageName;
use deckard_core::validate::clean_text;
use regex::Regex;
use serde_json::{Map, Value};
use zbus::zvariant::{OwnedValue, Str, Value as Variant};

pub type VariantMap = HashMap<String, OwnedValue>;

static RUNTIME_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:waydroid|lineageos|lineage os)\b")
        .unwrap_or_else(|error| panic!("invalid fixed runtime-name pattern: {error}"))
});

const PERMISSIONS: &[(&str, &[&str])] = &[
    (
        "location",
        &[
            "ACCESS_FINE_LOCATION",
            "ACCESS_COARSE_LOCATION",
            "ACCESS_BACKGROUND_LOCATION",
        ],
    ),
    ("camera", &["CAMERA"]),
    ("microphone", &["RECORD_AUDIO"]),
    (
        "contacts",
        &["READ_CONTACTS", "WRITE_CONTACTS", "GET_ACCOUNTS"],
    ),
    ("calendar", &["READ_CALENDAR", "WRITE_CALENDAR"]),
    (
        "storage",
        &[
            "READ_EXTERNAL_STORAGE",
            "WRITE_EXTERNAL_STORAGE",
            "READ_MEDIA_IMAGES",
            "READ_MEDIA_VIDEO",
            "READ_MEDIA_AUDIO",
        ],
    ),
    ("notifications", &["POST_NOTIFICATIONS"]),
    (
        "telephony",
        &[
            "READ_PHONE_STATE",
            "READ_PHONE_NUMBERS",
            "CALL_PHONE",
            "ANSWER_PHONE_CALLS",
            "ADD_VOICEMAIL",
            "USE_SIP",
            "ACCEPT_HANDOVER",
            "READ_CALL_LOG",
            "WRITE_CALL_LOG",
            "PROCESS_OUTGOING_CALLS",
        ],
    ),
    (
        "sms",
        &[
            "SEND_SMS",
            "RECEIVE_SMS",
            "READ_SMS",
            "RECEIVE_WAP_PUSH",
            "RECEIVE_MMS",
        ],
    ),
    (
        "bluetooth",
        &[
            "BLUETOOTH_SCAN",
            "BLUETOOTH_CONNECT",
            "BLUETOOTH_ADVERTISE",
            "NEARBY_WIFI_DEVICES",
            "UWB_RANGING",
        ],
    ),
    (
        "sensors",
        &[
            "BODY_SENSORS",
            "BODY_SENSORS_BACKGROUND",
            "ACTIVITY_RECOGNITION",
        ],
    ),
];

pub const SPECIAL_KINDS: &[&str] = &[
    "all_files",
    "install_apps",
    "overlay",
    "write_settings",
    "usage",
    "notification_listener",
    "dnd",
    "alarms",
    "pip",
    "battery",
    "accessibility",
    "device_admin",
];

pub fn permission(name: &str) -> Option<Vec<String>> {
    PERMISSIONS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, values)| {
            values
                .iter()
                .map(|value| format!("android.permission.{value}"))
                .collect()
        })
}

/// The Marathon permission an Android permission belongs to.
#[must_use]
pub fn marathon_permission(android: &str) -> Option<&'static str> {
    let short = android.strip_prefix("android.permission.")?;
    PERMISSIONS
        .iter()
        .find(|(_, values)| values.contains(&short))
        .map(|(name, _)| *name)
}

fn str_field<'a>(value: &'a Map<String, Value>, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn bool_field(value: &Map<String, Value>, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn number(value: &Map<String, Value>, key: &str, max: i64) -> i64 {
    value
        .get(key)
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .clamp(0.0, max as f64) as i64
}

fn string(map: &mut VariantMap, key: &str, value: &str) {
    map.insert(key.into(), Str::from(value).into());
}

fn array<T>(map: &mut VariantMap, key: &str, value: Vec<T>)
where
    for<'a> Variant<'a>: From<Vec<T>>,
{
    if let Ok(value) = OwnedValue::try_from(Variant::from(value)) {
        map.insert(key.into(), value);
    }
}

fn label(value: &Map<String, Value>, pkg: &str) -> String {
    let raw = clean_text(str_field(value, "label"), 128);
    let renamed = RUNTIME_NAME.replace_all(&raw, "Android");
    let compact = renamed.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        pkg.into()
    } else {
        compact
    }
}

pub fn app(value: &Map<String, Value>) -> VariantMap {
    let pkg = str_field(value, "pkg");
    if PackageName::try_from(pkg).is_err() {
        return VariantMap::new();
    }
    let mut map = VariantMap::new();
    string(&mut map, "pkg", pkg);
    string(&mut map, "label", &label(value, pkg));
    string(
        &mut map,
        "versionName",
        &clean_text(str_field(value, "versionName"), 64),
    );
    map.insert(
        "versionCode".into(),
        number(value, "versionCode", 1 << 53).into(),
    );
    map.insert("system".into(), bool_field(value, "system").into());
    for key in ["appBytes", "dataBytes", "cacheBytes"] {
        map.insert(key.into(), number(value, key, 1 << 50).into());
    }
    for key in ["notifications", "links"] {
        map.insert(key.into(), bool_field(value, key).into());
    }
    let mut domains = Vec::new();
    for domain in value
        .get("domains")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(4096)
        .filter_map(Value::as_str)
    {
        if domain.len() <= 253
            && valid_domain(domain)
            && !domains.contains(&domain.to_owned())
            && domains.len() < 50
        {
            domains.push(domain.to_owned());
        }
    }
    array(&mut map, "domains", domains);
    let battery = str_field(value, "battery");
    string(
        &mut map,
        "battery",
        if matches!(battery, "unrestricted" | "optimized") {
            battery
        } else {
            "optimized"
        },
    );

    let mut states: HashMap<&str, (bool, bool)> = HashMap::new();
    for entry in value
        .get("perms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(4096)
    {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let android = str_field(entry, "name");
        for (name, permissions) in PERMISSIONS {
            if *name == "notifications"
                || !permissions
                    .iter()
                    .any(|p| android == format!("android.permission.{p}"))
            {
                continue;
            }
            let state = states.entry(name).or_insert((false, true));
            state.0 |= bool_field(entry, "granted");
            state.1 &= bool_field(entry, "fixed");
        }
    }
    let permissions = PERMISSIONS
        .iter()
        .filter_map(|(name, _)| {
            states.get(name).map(|(granted, fixed)| {
                let mut item = VariantMap::new();
                string(&mut item, "name", name);
                item.insert("granted".into(), (*granted).into());
                item.insert("fixed".into(), (*fixed).into());
                item
            })
        })
        .collect();
    array(&mut map, "permissions", permissions);
    map
}

fn valid_domain(domain: &str) -> bool {
    let domain = domain.strip_prefix("*.").unwrap_or(domain);
    domain.contains('.')
        && domain.split('.').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

pub fn apps(value: &Map<String, Value>, fixed: bool) -> Vec<VariantMap> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in value
        .get("apps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(4096)
    {
        let Some(item) = item.as_object() else {
            continue;
        };
        let pkg = str_field(item, "pkg");
        if PackageName::try_from(pkg).is_err() || !seen.insert(pkg.to_owned()) {
            continue;
        }
        let mut map = VariantMap::new();
        string(&mut map, "pkg", pkg);
        string(&mut map, "label", &label(item, pkg));
        map.insert("granted".into(), bool_field(item, "granted").into());
        if fixed {
            map.insert("fixed".into(), bool_field(item, "fixed").into());
        }
        out.push(map);
        if out.len() == 500 {
            break;
        }
    }
    out
}

pub fn google(value: &Map<String, Value>) -> VariantMap {
    let mut map = VariantMap::new();
    let enabled = bool_field(value, "enabled");
    map.insert("installed".into(), bool_field(value, "installed").into());
    map.insert("enabled".into(), enabled.into());
    map.insert("push".into(), (enabled && bool_field(value, "push")).into());
    let accounts = value
        .get("accounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(4096)
        .filter_map(Value::as_str)
        .map(|s| clean_text(s, 254).trim().to_owned())
        .filter(|s| !s.is_empty() && !s.contains('\n'))
        .take(10)
        .collect();
    array(&mut map, "accounts", accounts);
    map
}
