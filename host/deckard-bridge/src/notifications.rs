//! Android notification socket dispatch for the Rust Hub adapter.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use deckard_core::ids::PackageName;
use deckard_core::keel;
use deckard_core::messages;
use deckard_core::notification_state::BridgeEvent;
use deckard_core::png_guard;
use deckard_core::sync;
use deckard_core::validate::{base64_png, clean_text, notification_key};
use deckard_notifications::{NotificationAdapter, Result};
use regex::Regex;
use serde_json::{Map, Value};
use zbus::blocking::Connection;
use zbus::zvariant::Value as Variant;

const HUB: &str = "org.freedesktop.Notifications";
const HUB_PATH: &str = "/org/freedesktop/Notifications";
const MAX_ICON_BYTES: usize = 256 * 1024;
const MAX_SYNC_KEYS: usize = 4096;
const TOAST_INTERVAL: Duration = Duration::from_millis(500);
const TOAST_LIFETIME: Duration = Duration::from_secs(4);
static RUNTIME_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:waydroid|lineageos|lineage os)\b")
        .unwrap_or_else(|error| panic!("invalid fixed runtime-name pattern: {error}"))
});

#[derive(Clone)]
pub struct NotificationBridge {
    adapter: NotificationAdapter,
    bus: Connection,
    icon_dir: PathBuf,
    last_toast: Arc<Mutex<HashMap<String, Instant>>>,
}

impl NotificationBridge {
    /// Validated Hub actions go back to Android through `to_android`, which
    /// must not block.
    pub fn new(
        bus: Connection,
        state_path: impl Into<PathBuf>,
        icon_dir: impl Into<PathBuf>,
        to_android: impl Fn(BridgeEvent) + Send + Sync + 'static,
    ) -> Result<Self> {
        let icon_dir = icon_dir.into();
        let adapter =
            NotificationAdapter::new(bus.clone(), state_path, icon_dir.clone(), to_android)?;
        Ok(Self {
            adapter,
            bus,
            icon_dir,
            last_toast: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn listen_hub(&self) -> Result<JoinHandle<()>> {
        self.adapter.listen()
    }

    /// Call only on the authenticated app bridge, in wire order.
    pub fn handle(&self, message: &Map<String, Value>) -> Result<()> {
        match message.get("t").and_then(Value::as_str).unwrap_or("") {
            "post" => {
                if let Ok(post) = messages::post(message) {
                    self.adapter.post(&post)?;
                }
            }
            "remove" => {
                let key = keel::string(message, "key");
                if notification_key(key).is_ok() {
                    self.adapter.remove(key)?;
                }
            }
            "sync" => {
                let keys = live_keys(message);
                self.adapter.sync(&keys)?;
            }
            "icon" => self.icon(message)?,
            "toast" => self.toast(message)?,
            _ => {}
        }
        Ok(())
    }

    fn icon(&self, message: &Map<String, Value>) -> Result<()> {
        let Some(package) = package(message) else {
            return Ok(());
        };
        let Ok(bytes) = base64_png(keel::string(message, "png"), MAX_ICON_BYTES) else {
            return Ok(());
        };
        let Ok(png) = png_guard::sanitize(&bytes, MAX_ICON_BYTES, 128) else {
            return Ok(());
        };
        save_icon(&self.icon_dir, package.as_str(), &png)?;
        Ok(())
    }

    fn toast(&self, message: &Map<String, Value>) -> Result<()> {
        let Some(package) = package(message) else {
            return Ok(());
        };
        let text = clean_text(keel::string(message, "text"), 512)
            .trim()
            .to_owned();
        if text.is_empty() {
            return Ok(());
        }
        let package = package.as_str();
        {
            let mut last = sync::lock(&self.last_toast);
            let now = Instant::now();
            if last
                .get(package)
                .is_some_and(|previous| now.duration_since(*previous) < TOAST_INTERVAL)
            {
                return Ok(());
            }
            last.retain(|_, previous| now.duration_since(*previous) < TOAST_LIFETIME);
            last.insert(package.to_owned(), now);
        }
        let label = toast_label(message);
        let label = if label.trim().is_empty() {
            package
        } else {
            label.as_str()
        };
        let icon_url = deckard_notifications::icon_url(&self.icon_dir, package);
        let desktop_entry = keel::app_id(package);
        let hints: HashMap<&str, Variant<'_>> = HashMap::from([
            ("desktop-entry", Variant::from(&desktop_entry)),
            ("urgency", Variant::from(0_u8)),
            ("transient", Variant::from(true)),
            ("suppress-sound", Variant::from(true)),
            ("x-marathon-toast", Variant::from(true)),
        ]);
        let reply = self.bus.call_method(
            Some(HUB),
            HUB_PATH,
            Some(HUB),
            "Notify",
            &(
                label,
                0_u32,
                icon_url.as_str(),
                text.as_str(),
                "",
                Vec::<String>::new(),
                hints,
                3500_i32,
            ),
        )?;
        let id: u32 = reply.body().deserialize()?;
        let bus = self.bus.clone();
        thread::Builder::new()
            .name("deckard-toast-expiry".to_owned())
            .spawn(move || {
                thread::sleep(TOAST_LIFETIME);
                let _ =
                    bus.call_method(Some(HUB), HUB_PATH, Some(HUB), "CloseNotification", &(id,));
            })?;
        Ok(())
    }
}

fn package(message: &Map<String, Value>) -> Option<PackageName> {
    PackageName::try_from(keel::string(message, "pkg")).ok()
}

fn toast_label(message: &Map<String, Value>) -> String {
    let label = clean_text(keel::string(message, "label"), 128);
    RUNTIME_NAME
        .replace_all(&label, "Android")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn live_keys(message: &Map<String, Value>) -> Vec<String> {
    message
        .get("keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_SYNC_KEYS)
        .filter_map(Value::as_str)
        .filter(|key| notification_key(key).is_ok())
        .map(str::to_owned)
        .collect()
}

fn save_icon(directory: &Path, package: &str, png: &[u8]) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(png)?;
    file.as_file().sync_all()?;
    file.persist(directory.join(format!("{package}.png")))?;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "fixed JSON and temporary-directory fixtures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_bad_sync_keys_and_bounds_batch() {
        let input = json!({"keys":["good", "", "bad\u{0}", "also-good"]});
        assert_eq!(
            live_keys(input.as_object().expect("object")),
            ["good", "also-good"]
        );
    }

    #[test]
    fn toast_label_matches_runtime_branding() {
        let input = json!({"label":"  Lineage OS  Waydroid\nMedia  "});
        assert_eq!(
            toast_label(input.as_object().expect("object")),
            "Android Android Media"
        );
    }

    #[test]
    fn icon_write_is_atomic_and_replaces() {
        let dir = tempfile::tempdir().expect("temporary icon directory");
        save_icon(dir.path(), "com.example", b"first").expect("initial icon");
        save_icon(dir.path(), "com.example", b"second").expect("replacement icon");
        assert_eq!(
            fs::read(dir.path().join("com.example.png")).expect("saved icon"),
            b"second"
        );
        assert_eq!(fs::read_dir(dir.path()).expect("icon entries").count(), 1);
    }
}
