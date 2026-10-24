//! Android notification socket dispatch for the Rust Hub adapter.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use deckard_core::ids::PackageName;
use deckard_core::json;
use deckard_core::messages;
use deckard_core::notification_state::BridgeEvent;
use deckard_core::png_guard;
use deckard_core::validate;
use deckard_notifications::{NotificationAdapter, Result};
use serde_json::{Map, Value};
use zbus::blocking::Connection;
use zbus::zvariant::Value as Variant;

const HUB: &str = "org.freedesktop.Notifications";
const HUB_PATH: &str = "/org/freedesktop/Notifications";
const MAX_ICON_BYTES: usize = 256 * 1024;
const MAX_SYNC_KEYS: usize = 4096;
const TOAST_INTERVAL: Duration = Duration::from_millis(500);
const TOAST_LIFETIME: Duration = Duration::from_secs(4);
static NEXT_ICON: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct NotificationBridge {
    adapter: NotificationAdapter,
    bus: Connection,
    icon_dir: PathBuf,
    last_toast: Arc<Mutex<HashMap<String, Instant>>>,
}

impl NotificationBridge {
    /// The returned receiver carries validated Hub actions back to the
    /// authenticated user bridge. The caller must drain it independently of
    /// inbound Android messages to avoid notification-action deadlocks.
    pub fn new(
        bus: Connection,
        state_path: impl Into<PathBuf>,
        icon_dir: impl Into<PathBuf>,
    ) -> Result<(Self, mpsc::Receiver<BridgeEvent>)> {
        let icon_dir = icon_dir.into();
        let (sender, receiver) = mpsc::channel();
        let adapter = NotificationAdapter::new(bus.clone(), state_path, icon_dir.clone(), sender)?;
        Ok((
            Self {
                adapter,
                bus,
                icon_dir,
                last_toast: Arc::new(Mutex::new(HashMap::new())),
            },
            receiver,
        ))
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
                let key = json::string(message, "key");
                if validate::is_notification_key(key) {
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

    pub fn snapshot(&self) -> Result<Value> {
        self.adapter.snapshot()
    }

    fn icon(&self, message: &Map<String, Value>) -> Result<()> {
        let Some(package) = package(message) else {
            return Ok(());
        };
        let Some(bytes) = validate::base64_png(json::string(message, "png"), MAX_ICON_BYTES) else {
            return Ok(());
        };
        let Some(png) = png_guard::sanitize(&bytes, MAX_ICON_BYTES, 128) else {
            return Ok(());
        };
        save_icon(&self.icon_dir, &package, &png)?;
        Ok(())
    }

    fn toast(&self, message: &Map<String, Value>) -> Result<()> {
        let Some(package) = package(message) else {
            return Ok(());
        };
        let text = json::text(message, "text", 512).trim().to_owned();
        if text.is_empty() {
            return Ok(());
        }
        {
            let mut last = self
                .last_toast
                .lock()
                .map_err(|_| io::Error::other("toast state poisoned"))?;
            let now = Instant::now();
            if last
                .get(&package)
                .is_some_and(|previous| now.duration_since(*previous) < TOAST_INTERVAL)
            {
                return Ok(());
            }
            last.retain(|_, previous| now.duration_since(*previous) < TOAST_LIFETIME);
            last.insert(package.clone(), now);
        }
        let label = validate::label(json::string(message, "label"), &package);
        let icon = self.icon_dir.join(format!("{package}.png"));
        let icon_url = if icon.is_file() {
            format!("file://{}", icon.display())
        } else {
            String::new()
        };
        let desktop_entry = format!("waydroid.{package}");
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

pub fn bridge_event(event: &BridgeEvent) -> Map<String, Value> {
    match serde_json::to_value(event) {
        Ok(Value::Object(object)) => object,
        _ => Map::new(),
    }
}

fn package(message: &Map<String, Value>) -> Option<String> {
    PackageName::try_from(json::string(message, "pkg"))
        .ok()
        .map(|name| name.as_str().to_owned())
}

fn live_keys(message: &Map<String, Value>) -> Vec<String> {
    message
        .get("keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_SYNC_KEYS)
        .filter_map(Value::as_str)
        .filter(|key| validate::is_notification_key(key))
        .map(str::to_owned)
        .collect()
}

fn save_icon(directory: &Path, package: &str, png: &[u8]) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let destination = directory.join(format!("{package}.png"));
    for _ in 0..4 {
        let nonce = NEXT_ICON.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(".{package}.{}.{}.tmp", std::process::id(), nonce));
        let result = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary);
        let Ok(mut output) = result else {
            continue;
        };
        let written = output.write_all(png).and_then(|()| output.sync_all());
        drop(output);
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "cannot allocate icon file",
    ))
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "fixed JSON and temporary-directory fixtures"
)]
mod tests {
    use super::*;
    use deckard_core::notification_state::BridgeEvent;
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
    fn action_event_keeps_android_wire_contract() {
        assert_eq!(
            bridge_event(&BridgeEvent::Invoke {
                key: "k".into(),
                action: "default".into()
            }),
            json!({"t":"invoke","key":"k","action":"default"})
                .as_object()
                .expect("object")
                .clone()
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
