#![allow(clippy::unwrap_used, reason = "test-only bus and payload fixtures")]

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use deckard_core::messages;
use deckard_core::notification_state::BridgeEvent;
use deckard_notifications::NotificationAdapter;
use image::{DynamicImage, ImageFormat};
use serde_json::json;
use zbus::blocking::{Connection, connection::Builder};
use zbus::interface;
use zbus::zvariant::OwnedValue;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const HUB: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

struct PrivateBus {
    daemon: Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl PrivateBus {
    fn start() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("bus");
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = daemon
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing daemon stdout"))?;
        let mut address = String::new();
        BufReader::new(stdout).read_line(&mut address)?;
        if address.trim().is_empty() {
            return Err(io::Error::other("dbus-daemon did not publish an address").into());
        }
        Ok(Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        })
    }

    fn connection(&self) -> TestResult<Connection> {
        Ok(Builder::address(self.address.as_str())?
            .method_timeout(Duration::from_secs(3))
            .build()?)
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

#[derive(Clone, Debug)]
struct NotifyCall {
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: HashMap<String, OwnedValue>,
    timeout: i32,
}

#[derive(Default)]
struct Calls {
    notify: Vec<NotifyCall>,
    close: Vec<u32>,
}

struct FakeHub(Arc<Mutex<Calls>>);

#[interface(name = "org.freedesktop.Notifications")]
impl FakeHub {
    #[zbus(name = "Notify")]
    #[allow(
        clippy::too_many_arguments,
        reason = "freedesktop Notify has eight wire parameters"
    )]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let mut calls = self.0.lock().unwrap();
        calls.notify.push(NotifyCall {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            hints,
            timeout: expire_timeout,
        });
        if replaces_id == 0 {
            40 + calls.notify.len() as u32
        } else {
            replaces_id
        }
    }

    #[zbus(name = "CloseNotification")]
    fn close_notification(&self, id: u32) {
        self.0.lock().unwrap().close.push(id);
    }
}

fn post(key: &str, title: &str, auto_cancel: bool) -> messages::Post {
    let message = json!({
        "key": key, "pkg": "org.example.App", "label": "Example",
        "title": title, "text": "<&>\"'", "content": true,
        "reply": {"action": "a1", "label": "Reply", "placeholder": "Type here"},
        "actions": [{"id": "a2", "label": "Archive"}],
        "category": "msg", "urgency": 2, "progress": {"max": 10, "current": 5},
        "autoCancel": auto_cancel,
    });
    messages::post(message.as_object().unwrap()).unwrap()
}

#[test]
fn private_bus_notify_replace_close_and_legacy_state() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let calls = Arc::new(Mutex::new(Calls::default()));
    let hub = Builder::address(bus.address.as_str())?
        .name(HUB)?
        .serve_at(PATH, FakeHub(calls.clone()))?
        .build()?;
    let home = tempfile::tempdir()?;
    let state_path = home.path().join("state/deckard/notifications.json");
    let (tx, _rx) = mpsc::channel();
    let adapter = NotificationAdapter::new(
        bus.connection()?,
        &state_path,
        home.path().join("icons"),
        tx,
    )?;

    let first = post("android-key", "First", false);
    adapter.post(&first)?;
    adapter.post(&first)?;
    {
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.notify.len(),
            1,
            "identical posts should not re-notify"
        );
        let notification = &calls.notify[0];
        assert_eq!(notification.app_name, "Example");
        assert_eq!(notification.replaces_id, 0);
        assert_eq!(notification.app_icon, "");
        assert_eq!(notification.summary, "First");
        assert_eq!(notification.body, "&lt;&amp;&gt;&quot;'");
        assert_eq!(
            notification.actions,
            ["default", "Open", "inline-reply", "Reply", "a2", "Archive"]
        );
        assert_eq!(notification.timeout, -1);
        assert!(notification.hints.contains_key("desktop-entry"));
        assert!(
            notification
                .hints
                .contains_key("x-kde-reply-placeholder-text")
        );
        assert!(notification.hints.contains_key("value"));
    }
    let persisted: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
    assert_eq!(persisted["android-key"]["id"], 41);
    assert_eq!(persisted["android-key"]["reply"], "a1");

    let restored = NotificationAdapter::new(
        bus.connection()?,
        &state_path,
        home.path().join("icons"),
        mpsc::channel().0,
    )?;
    restored.post(&first)?;
    assert_eq!(
        calls.lock().unwrap().notify.len(),
        1,
        "legacy digest must survive restart"
    );
    restored.post(&post("android-key", "Changed", false))?;
    assert_eq!(calls.lock().unwrap().notify[1].replaces_id, 41);
    restored.remove("android-key")?;
    assert_eq!(calls.lock().unwrap().close, [41]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(state_path)?)?,
        json!({})
    );
    drop(hub);
    Ok(())
}

#[test]
fn private_bus_actions_replies_and_dismissal_timing() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let calls = Arc::new(Mutex::new(Calls::default()));
    let hub = Builder::address(bus.address.as_str())?
        .name(HUB)?
        .serve_at(PATH, FakeHub(calls))?
        .build()?;
    let home = tempfile::tempdir()?;
    let (tx, rx) = mpsc::channel();
    let adapter = NotificationAdapter::new(
        bus.connection()?,
        home.path().join("notifications.json"),
        home.path().join("icons"),
        tx,
    )?;
    let _listener = adapter.listen()?;
    adapter.post(&post("tap", "Tap", false))?;
    hub.emit_signal(
        None::<&str>,
        PATH,
        HUB,
        "ActionInvoked",
        &(41_u32, "default"),
    )?;
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2))?,
        BridgeEvent::Invoke {
            key: "tap".into(),
            action: "default".into()
        }
    );
    hub.emit_signal(
        None::<&str>,
        PATH,
        HUB,
        "NotificationReplied",
        &(41_u32, "Hello"),
    )?;
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2))?,
        BridgeEvent::Reply {
            key: "tap".into(),
            action: "a1".into(),
            text: "Hello".into()
        }
    );
    hub.emit_signal(
        None::<&str>,
        PATH,
        HUB,
        "NotificationClosed",
        &(41_u32, 2_u32),
    )?;
    assert!(
        rx.recv_timeout(Duration::from_millis(750)).is_err(),
        "a tapped non-auto-cancel row must not be dismissed"
    );
    assert_eq!(adapter.snapshot()?, json!({}));

    adapter.post(&post("dismiss", "Dismiss", true))?;
    hub.emit_signal(
        None::<&str>,
        PATH,
        HUB,
        "NotificationClosed",
        &(42_u32, 2_u32),
    )?;
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2))?,
        BridgeEvent::Dismiss {
            key: "dismiss".into()
        }
    );
    Ok(())
}

#[test]
fn private_bus_image_data_uses_freedesktop_struct_signature() -> TestResult<()> {
    let bus = PrivateBus::start()?;
    let calls = Arc::new(Mutex::new(Calls::default()));
    let _hub = Builder::address(bus.address.as_str())?
        .name(HUB)?
        .serve_at(PATH, FakeHub(calls.clone()))?
        .build()?;
    let home = tempfile::tempdir()?;
    let (tx, _rx) = mpsc::channel();
    let adapter = NotificationAdapter::new(
        bus.connection()?,
        home.path().join("notifications.json"),
        home.path().join("icons"),
        tx,
    )?;
    let mut png = io::Cursor::new(Vec::new());
    DynamicImage::new_rgba8(2, 3).write_to(&mut png, ImageFormat::Png)?;
    let mut notification = post("image", "Image", false);
    notification.image = png.into_inner();
    adapter.post(&notification)?;
    let calls = calls.lock().unwrap();
    let image_data = &calls.notify[0].hints["image-data"];
    assert_eq!(image_data.value_signature().to_string(), "(iiibiiay)");
    Ok(())
}
