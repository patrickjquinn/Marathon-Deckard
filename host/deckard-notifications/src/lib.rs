//! D-Bus adapter for Deckard's validated Android notification payloads.

mod digest;

use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use deckard_core::keel;
use deckard_core::messages::Post;
use deckard_core::notification_state::{BridgeEvent, Command, NotificationState};
use deckard_core::png_guard;
use deckard_core::sync;
use serde_json::Value;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::{Message, Type};
use zbus::zvariant::Value as Variant;

/// Any failure of the adapter.
pub type AdapterError = Box<dyn Error + Send + Sync>;
/// The adapter's result.
pub type Result<T> = std::result::Result<T, AdapterError>;

const HUB: &str = "org.freedesktop.Notifications";
const HUB_PATH: &str = "/org/freedesktop/Notifications";
const MAX_STATE_BYTES: usize = 1024 * 1024;
const MAX_PENDING_DISMISSALS: usize = 512;

/// A dismissal: when it is due, the notification id and its token.
type Dismissal = (Instant, u32, u64);

struct Inner {
    bus: Connection,
    state: Mutex<NotificationState>,
    state_path: PathBuf,
    icon_dir: PathBuf,
    to_android: Box<dyn Fn(BridgeEvent) + Send + Sync>,
    dismissals: SyncSender<Dismissal>,
}

/// The single owner of Android's notifications on the Hub. Every change
/// runs under one lock, in the order it is made.
#[derive(Clone)]
pub struct NotificationAdapter {
    inner: Arc<Inner>,
}

impl NotificationAdapter {
    /// Restores `state_path` and posts on `bus`; Hub actions go back to
    /// Android through `to_android`.
    ///
    /// # Errors
    /// The saved state is unreadable or too large, or the dismissal thread
    /// could not start.
    pub fn new(
        bus: Connection,
        state_path: impl Into<PathBuf>,
        icon_dir: impl Into<PathBuf>,
        to_android: impl Fn(BridgeEvent) + Send + Sync + 'static,
    ) -> Result<Self> {
        let state_path = state_path.into();
        let state = match fs::read(&state_path) {
            Ok(bytes) if bytes.len() > MAX_STATE_BYTES => {
                return Err(
                    io::Error::new(io::ErrorKind::InvalidData, "state file too large").into(),
                );
            }
            Ok(bytes) => NotificationState::restore(&serde_json::from_slice::<Value>(&bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => NotificationState::default(),
            Err(error) => return Err(error.into()),
        };
        let (dismissals, due) = mpsc::sync_channel(MAX_PENDING_DISMISSALS);
        let inner = Arc::new(Inner {
            bus,
            state: Mutex::new(state),
            state_path,
            icon_dir: icon_dir.into(),
            to_android: Box::new(to_android),
            dismissals,
        });
        let weak = Arc::downgrade(&inner);
        thread::Builder::new()
            .name("deckard-notification-timer".to_owned())
            .spawn(move || dismiss(&weak, &due))?;
        Ok(Self { inner })
    }

    /// Subscribes to the Hub's signals, then follows them on a thread that
    /// ends with the bus.
    ///
    /// # Errors
    /// The subscription failed.
    pub fn listen(&self) -> Result<JoinHandle<()>> {
        let rule = MatchRule::builder().msg_type(Type::Signal).sender(HUB)?;
        let rule = rule.path(HUB_PATH)?.interface(HUB)?.build();
        let signals = MessageIterator::for_match_rule(rule, &self.inner.bus, Some(256))?;
        let adapter = self.clone();
        Ok(thread::spawn(move || {
            for signal in signals {
                let signal = match signal {
                    Ok(signal) => signal,
                    Err(error) => return eprintln!("deckard-notifications signal stream: {error}"),
                };
                if let Err(error) = adapter.handle_signal(&signal) {
                    eprintln!("deckard-notifications signal: {error}");
                }
            }
        }))
    }

    /// Posts or updates `post`.
    ///
    /// # Errors
    /// The Hub refused it, or the state could not be saved.
    pub fn post(&self, post: &Post) -> Result<()> {
        let digest = digest::display_digest(post);
        let mut state = sync::lock(&self.inner.state);
        let Some(replaces_id) = state.replaces(post, &digest) else {
            return Ok(());
        };
        let id = self.notify(post, replaces_id)?;
        let commands = state.notified(post, &digest, id);
        self.execute(commands)
    }

    /// Withdraws Android's notification `key`.
    ///
    /// # Errors
    /// The Hub refused it, or the state could not be saved.
    pub fn remove(&self, key: &str) -> Result<()> {
        self.apply(|state| state.remove(key))
    }

    /// Withdraws every notification not in `live_keys`.
    ///
    /// # Errors
    /// The Hub refused it, or the state could not be saved.
    pub fn sync(&self, live_keys: &[String]) -> Result<()> {
        self.apply(|state| state.sync(live_keys))
    }

    fn apply(&self, change: impl FnOnce(&mut NotificationState) -> Vec<Command>) -> Result<()> {
        let mut state = sync::lock(&self.inner.state);
        let commands = change(&mut state);
        self.execute(commands)
    }

    fn execute(&self, commands: Vec<Command>) -> Result<()> {
        let mut close_error = None;
        for command in commands {
            match command {
                Command::Close(id) => {
                    if let Err(error) = self.hub("CloseNotification", &(id,)) {
                        close_error.get_or_insert(error);
                    }
                }
                Command::Persist(snapshot) => persist(&self.inner.state_path, &snapshot)?,
                Command::ScheduleDismiss {
                    id,
                    token,
                    delay_ms,
                } => {
                    let due = Instant::now() + Duration::from_millis(delay_ms);
                    let queued = self.inner.dismissals.try_send((due, id, token));
                    if let Err(TrySendError::Full(_)) = queued {
                        eprintln!("deckard-notifications: too many dismissals; {id} stays");
                    }
                }
                Command::Send(event) => (self.inner.to_android)(event),
            }
        }
        close_error.map_or(Ok(()), Err)
    }

    fn hub<B>(&self, method: &str, body: &B) -> Result<Message>
    where
        B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
    {
        let bus = &self.inner.bus;
        Ok(bus.call_method(Some(HUB), HUB_PATH, Some(HUB), method, body)?)
    }

    fn notify(&self, post: &Post, replaces_id: u32) -> Result<u32> {
        let desktop_entry = keel::app_id(post.package.as_str());
        let mut hints = HashMap::from([
            ("desktop-entry", Variant::from(&desktop_entry)),
            ("urgency", Variant::from(post.urgency)),
            ("resident", Variant::from(post.ongoing)),
            ("suppress-sound", Variant::from(post.silent)),
        ]);
        if !post.category.is_empty() {
            hints.insert("category", Variant::from(post.category.as_str()));
        }
        if !post.reply_action.is_empty() && !post.reply_placeholder.is_empty() {
            let placeholder = Variant::from(post.reply_placeholder.as_str());
            hints.insert("x-kde-reply-placeholder-text", placeholder);
        }
        let conversation = post.conversation.as_ref().map(Value::to_string);
        if let Some(value) = conversation.as_deref() {
            hints.insert("x-marathon-conversation", Variant::from(value));
        }
        if let Some(progress) = post.progress {
            hints.insert("value", Variant::from(progress));
        }
        if let Some((width, height, pixels)) = image_data(&post.image) {
            let image = (width, height, width * 4, true, 8_i32, 4_i32, pixels);
            hints.insert("image-data", Variant::new(image));
        }
        let icon_url = icon_url(&self.inner.icon_dir, post.package.as_str());
        let mut actions = Vec::with_capacity(post.actions.len() * 2);
        for (id, label) in &post.actions {
            actions.extend([id.as_str(), label.as_str()]);
        }
        let text = html_escape(&post.body);
        let timeout = if post.ongoing { 0_i32 } else { -1_i32 };
        let (label, title, icon_url) = (post.label.as_str(), post.title.as_str(), &icon_url);
        let body = (
            label,
            replaces_id,
            icon_url,
            title,
            &text,
            actions,
            hints,
            timeout,
        );
        let reply = self.hub("Notify", &body)?;
        Ok(reply.body().deserialize::<u32>()?)
    }

    fn handle_signal(&self, message: &Message) -> Result<()> {
        let body = message.body();
        let header = message.header();
        match header.member().map(|member| member.as_str()) {
            Some("ActionInvoked") => {
                let (id, action): (u32, String) = body.deserialize()?;
                self.apply(|state| state.action_invoked(id, &action, now_ms()))
            }
            Some("NotificationReplied") => {
                let (id, text): (u32, String) = body.deserialize()?;
                self.apply(|state| state.notification_replied(id, &text))
            }
            Some("NotificationClosed") => {
                let (id, reason): (u32, u32) = body.deserialize()?;
                self.apply(|state| state.notification_closed(id, reason))
            }
            _ => Ok(()),
        }
    }
}

/// The `file://` URL of `package`'s icon in `icon_dir`; empty when it has none.
#[must_use]
pub fn icon_url(icon_dir: &Path, package: &str) -> String {
    let icon = icon_dir.join(format!("{package}.png"));
    if icon.is_file() {
        format!("file://{}", icon.display())
    } else {
        String::new()
    }
}

/// Every delay is `DISMISS_DELAY_MS`, so dismissals arrive in deadline order.
fn dismiss(adapter: &Weak<Inner>, due: &Receiver<Dismissal>) {
    for (deadline, id, token) in due {
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
        let Some(inner) = adapter.upgrade() else {
            return;
        };
        let adapter = NotificationAdapter { inner };
        if let Err(error) = adapter.apply(|state| state.dismiss_due(id, token, now_ms())) {
            eprintln!("deckard-notifications dismiss: {error}");
        }
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

/// The RGBA pixels of a PNG scaled to fit 128 px.
fn image_data(png: &[u8]) -> Option<(i32, i32, Vec<u8>)> {
    let image = png_guard::decode(png, 512 * 1024, 128)?;
    let width = i32::try_from(image.width()).ok()?;
    let height = i32::try_from(image.height()).ok()?;
    Some((width, height, image.into_raw()))
}

fn html_escape(value: &str) -> String {
    let value = value.replace('&', "&amp;").replace('<', "&lt;");
    value.replace('>', "&gt;").replace('"', "&quot;")
}

fn now_ms() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH);
    i64::try_from(since.unwrap_or_default().as_millis()).unwrap_or(i64::MAX)
}
