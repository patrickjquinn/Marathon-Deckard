//! D-Bus adapter for Deckard's validated Android notification payloads.

mod digest;
mod timers;

use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use deckard_core::messages::Post;
use deckard_core::notification_state::{BridgeEvent, Command, NotificationState};
use image::{ImageFormat, ImageReader, imageops::FilterType};
use serde_json::Value;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::{Message, Type};
use zbus::zvariant::Value as Variant;

use crate::timers::{MAX_PENDING_DISMISSALS, TimerQueue};

pub type AdapterError = Box<dyn Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, AdapterError>;

const HUB: &str = "org.freedesktop.Notifications";
const HUB_PATH: &str = "/org/freedesktop/Notifications";

struct Inner {
    bus: Connection,
    operation: Mutex<()>,
    state: Mutex<NotificationState>,
    state_path: PathBuf,
    icon_dir: PathBuf,
    android_commands: mpsc::Sender<BridgeEvent>,
    timers: Arc<TimerQueue>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.timers.stop();
    }
}

/// A single notification owner. The Android bridge must call these methods in
/// message order and must not run alongside the old C++ notification handler.
#[derive(Clone)]
pub struct NotificationAdapter {
    inner: Arc<Inner>,
}

impl NotificationAdapter {
    /// `bus` may be a session bus connection or a private test-bus connection.
    pub fn new(
        bus: Connection,
        state_path: impl Into<PathBuf>,
        icon_dir: impl Into<PathBuf>,
        android_commands: mpsc::Sender<BridgeEvent>,
    ) -> Result<Self> {
        let state_path = state_path.into();
        let state = match fs::read(&state_path) {
            Ok(bytes) => {
                if bytes.len() > 1024 * 1024 {
                    return Err(
                        io::Error::new(io::ErrorKind::InvalidData, "state file too large").into(),
                    );
                }
                NotificationState::restore(&serde_json::from_slice::<Value>(&bytes)?)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => NotificationState::default(),
            Err(error) => return Err(error.into()),
        };
        let timers = Arc::new(TimerQueue::new(MAX_PENDING_DISMISSALS));
        let inner = Arc::new(Inner {
            bus,
            operation: Mutex::new(()),
            state: Mutex::new(state),
            state_path,
            icon_dir: icon_dir.into(),
            android_commands,
            timers: Arc::clone(&timers),
        });
        let weak = Arc::downgrade(&inner);
        thread::Builder::new()
            .name("deckard-notification-timer".to_owned())
            .spawn(move || {
                if let Err(error) = timers.run(|id, token| {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    if let Err(error) = (NotificationAdapter { inner }).dismiss_due(id, token) {
                        eprintln!("deckard-notifications dismiss: {error}");
                    }
                }) {
                    eprintln!("deckard-notifications timer worker: {error}");
                }
                timers.stop();
            })?;
        Ok(Self { inner })
    }

    /// Register the Hub signal match before spawning its receiver thread.
    /// The listener ends when the underlying bus closes. Dropping this adapter
    /// alone does not stop the thread because the listener retains a clone.
    pub fn listen(&self) -> Result<JoinHandle<()>> {
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(HUB)?
            .path(HUB_PATH)?
            .interface(HUB)?
            .build();
        let mut signals = MessageIterator::for_match_rule(rule, &self.inner.bus, Some(256))?;
        let adapter = self.clone();
        Ok(thread::spawn(move || {
            for message in &mut signals {
                match message {
                    Ok(message) => {
                        if let Err(error) = adapter.handle_signal(&message) {
                            eprintln!("deckard-notifications signal: {error}");
                        }
                    }
                    Err(error) => {
                        eprintln!("deckard-notifications signal stream: {error}");
                        break;
                    }
                }
            }
        }))
    }

    pub fn post(&self, post: &Post) -> Result<()> {
        let _operation = self.operation()?;
        let digest = digest::display_digest(post);
        let commands = self.state()?.prepare_post(post, &digest);
        self.execute(commands)
    }

    pub fn remove(&self, key: &str) -> Result<()> {
        let _operation = self.operation()?;
        let commands = self.state()?.remove(key);
        self.execute(commands)
    }

    pub fn sync(&self, live_keys: &[String]) -> Result<()> {
        let _operation = self.operation()?;
        let commands = self.state()?.sync(live_keys);
        self.execute(commands)
    }

    pub fn snapshot(&self) -> Result<Value> {
        Ok(self.state()?.snapshot())
    }

    fn state(&self) -> Result<MutexGuard<'_, NotificationState>> {
        self.inner
            .state
            .lock()
            .map_err(|_| io::Error::other("notification state lock poisoned").into())
    }

    fn operation(&self) -> Result<MutexGuard<'_, ()>> {
        self.inner
            .operation
            .lock()
            .map_err(|_| io::Error::other("notification operation lock poisoned").into())
    }

    fn dismiss_due(&self, id: u32, token: u64) -> Result<()> {
        let _operation = self.operation()?;
        let commands = self.state()?.dismiss_due(id, token, now_ms());
        self.execute(commands)
    }

    fn execute(&self, commands: Vec<Command>) -> Result<()> {
        let mut close_error = None;
        for command in commands {
            match command {
                Command::Notify {
                    request,
                    post,
                    replaces_id,
                } => match self.notify(&post, replaces_id) {
                    Ok(id) => {
                        let follow_up = self.state()?.notified(request, id);
                        self.execute(follow_up)?;
                    }
                    Err(error) => {
                        self.state()?.notify_failed(request);
                        return Err(error);
                    }
                },
                Command::Close(id) => {
                    if let Err(error) = self.close(id) {
                        close_error.get_or_insert(error);
                    }
                }
                Command::Persist(snapshot) => persist(&self.inner.state_path, &snapshot)?,
                Command::ScheduleDismiss {
                    id,
                    token,
                    delay_ms,
                } => self
                    .inner
                    .timers
                    .schedule(id, token, Duration::from_millis(delay_ms))?,
                Command::Send(event) => self.inner.android_commands.send(event).map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "Android command receiver closed")
                })?,
            }
        }
        close_error.map_or(Ok(()), Err)
    }

    fn notify(&self, post: &Post, replaces_id: u32) -> Result<u32> {
        let mut hints: HashMap<&str, Variant<'_>> = HashMap::new();
        let desktop_entry = format!("waydroid.{}", post.package.as_str());
        hints.insert("desktop-entry", Variant::from(&desktop_entry));
        hints.insert("urgency", Variant::from(post.urgency));
        hints.insert("resident", Variant::from(post.ongoing));
        hints.insert("suppress-sound", Variant::from(post.silent));
        if !post.category.is_empty() {
            hints.insert("category", Variant::from(post.category.as_str()));
        }
        if !post.reply_action.is_empty() && !post.reply_placeholder.is_empty() {
            hints.insert(
                "x-kde-reply-placeholder-text",
                Variant::from(post.reply_placeholder.as_str()),
            );
        }
        let conversation = post.conversation.as_ref().map(Value::to_string);
        if let Some(value) = conversation.as_deref() {
            hints.insert("x-marathon-conversation", Variant::from(value));
        }
        if let Some(progress) = post.progress {
            hints.insert("value", Variant::from(progress));
        }
        if let Some((width, height, pixels)) = image_data(&post.image) {
            hints.insert(
                "image-data",
                Variant::new((width, height, width * 4, true, 8_i32, 4_i32, pixels)),
            );
        }
        let icon = self
            .inner
            .icon_dir
            .join(format!("{}.png", post.package.as_str()));
        let icon_url = if icon.is_file() {
            format!("file://{}", icon.display())
        } else {
            String::new()
        };
        let actions: Vec<&str> = post
            .actions
            .iter()
            .flat_map(|(id, label)| [id.as_str(), label.as_str()])
            .collect();
        let body = html_escape(&post.body);
        let reply = self.inner.bus.call_method(
            Some(HUB),
            HUB_PATH,
            Some(HUB),
            "Notify",
            &(
                post.label.as_str(),
                replaces_id,
                icon_url.as_str(),
                post.title.as_str(),
                body.as_str(),
                actions,
                hints,
                if post.ongoing { 0_i32 } else { -1_i32 },
            ),
        )?;
        Ok(reply.body().deserialize::<u32>()?)
    }

    fn close(&self, id: u32) -> Result<()> {
        self.inner
            .bus
            .call_method(Some(HUB), HUB_PATH, Some(HUB), "CloseNotification", &(id,))?;
        Ok(())
    }

    fn handle_signal(&self, message: &Message) -> Result<()> {
        let _operation = self.operation()?;
        let member = message
            .header()
            .member()
            .map(|name| name.as_str().to_owned());
        let commands = match member.as_deref() {
            Some("ActionInvoked") => {
                let (id, action): (u32, String) = message.body().deserialize()?;
                self.state()?.action_invoked(id, &action, now_ms())
            }
            Some("NotificationReplied") => {
                let (id, text): (u32, String) = message.body().deserialize()?;
                self.state()?.notification_replied(id, &text)
            }
            Some("NotificationClosed") => {
                let (id, reason): (u32, u32) = message.body().deserialize()?;
                self.state()?.notification_closed(id, reason)
            }
            _ => return Ok(()),
        };
        self.execute(commands)
    }
}

fn persist(path: &Path, state: &Value) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "state path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut temporary, state)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn image_data(png: &[u8]) -> Option<(i32, i32, Vec<u8>)> {
    if png.is_empty() || png.len() > 512 * 1024 {
        return None;
    }
    let (width, height) = ImageReader::with_format(Cursor::new(png), ImageFormat::Png)
        .into_dimensions()
        .ok()?;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return None;
    }
    let image = image::load_from_memory_with_format(png, ImageFormat::Png).ok()?;
    let image = if width.max(height) > 128 {
        image.resize(128, 128, FilterType::Lanczos3)
    } else {
        image
    }
    .to_rgba8();
    Some((
        image.width() as i32,
        image.height() as i32,
        image.into_raw(),
    ))
}

fn html_escape(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            _ => result.push(character),
        }
    }
    result
}

fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}
