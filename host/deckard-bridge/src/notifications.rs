//! Android's notifications, icons and toasts on the Hub.

mod digest;

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use deckard_core::messages::{self, Post};
use deckard_core::{ids, json, keel, png_guard, sync, validate};
use deckard_host_tools::files;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use zbus::Variant;
use zbus::message::Message;
use zbus::{Connection, MessageStream};

use crate::Outbound;

/// Any failure of the Hub adapter.
pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const HUB: &str = "org.freedesktop.Notifications";
const HUB_PATH: &str = "/org/freedesktop/Notifications";
const MAX_TRACKED: usize = 200;
const MAX_STATE_BYTES: usize = 1024 * 1024;
const MAX_PENDING_DISMISSALS: usize = 512;
const MAX_EXPIRING: usize = 64;
const MAX_ICON_BYTES: usize = 256 * 1024;
const MAX_SYNC_KEYS: usize = 4096;
const DISMISS_DELAY: Duration = Duration::from_millis(500);
const TAP_GRACE: Duration = Duration::from_secs(3);
const TOAST_INTERVAL: Duration = Duration::from_millis(500);
const TOAST_LIFETIME: Duration = Duration::from_secs(4);

/// A Hub row, saved in `notifications.json` as deckardd saved it.
#[derive(Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct Entry {
    id: u32,
    digest: String,
    reply: String,
    auto_cancel: bool,
    #[serde(skip)]
    invoked_at: Option<Instant>,
}

#[derive(Default)]
struct State {
    entries: BTreeMap<String, Entry>,
    keys: HashMap<u32, String>,
    /// Rows the person closed, held `DISMISS_DELAY` for a late tap, with
    /// when they closed.
    closing: HashMap<u32, (String, Entry, u64, Instant)>,
    next_token: u64,
    last_toast: HashMap<String, Instant>,
}

/// A due dismissal: when, the Hub id and its token.
type Dismissal = (Instant, u32, u64);

struct Inner {
    bus: Connection,
    state: Mutex<State>,
    state_path: PathBuf,
    icon_dir: PathBuf,
    to_android: Arc<Outbound>,
    dismissals: SyncSender<Dismissal>,
    /// Toasts to close and when, in the order they were shown.
    expiring: SyncSender<(Instant, u32)>,
}

/// The single owner of Android's rows on the Hub; every change runs under
/// one lock, in the order it is made.
#[derive(Clone)]
pub struct Notifications(Arc<Inner>);

impl Notifications {
    /// Restores `state_path`; Hub actions go back to Android through
    /// `to_android`, which must not block.
    ///
    /// # Errors
    /// The saved state is unreadable or too large, or a timer thread could
    /// not start.
    pub fn new(
        bus: Connection,
        state_path: PathBuf,
        icon_dir: PathBuf,
        to_android: Arc<Outbound>,
    ) -> Result<Self> {
        let mut state = State::default();
        match fs::read(&state_path) {
            Ok(bytes) if bytes.len() > MAX_STATE_BYTES => return Err("state file too large".into()),
            Ok(bytes) => {
                let rows: Value = serde_json::from_slice(&bytes)?;
                for (key, row) in rows.as_object().into_iter().flatten() {
                    let entry = Entry::deserialize(row).unwrap_or_default();
                    if entry.id != 0 {
                        state.keys.insert(entry.id, key.clone());
                        state.entries.insert(key.clone(), entry);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let (dismissals, due) = mpsc::sync_channel(MAX_PENDING_DISMISSALS);
        let (expiring, closes) = mpsc::sync_channel(MAX_EXPIRING);
        let hub = bus.clone();
        thread::Builder::new()
            .name("deckard-toast-expiry".to_owned())
            .spawn(move || close_toasts(&hub, &closes))?;
        let inner = Arc::new(Inner {
            bus,
            state: Mutex::new(state),
            state_path,
            icon_dir,
            to_android,
            dismissals,
            expiring,
        });
        let weak = Arc::downgrade(&inner);
        thread::Builder::new()
            .name("deckard-notification-timer".to_owned())
            .spawn(move || dismiss(&weak, &due))?;
        Ok(Self(inner))
    }

    /// Follows the Hub's signals on a thread that ends with the bus.
    ///
    /// # Errors
    /// The subscription failed.
    pub fn listen(&self) -> Result<JoinHandle<()>> {
        let rule = crate::signals(HUB, HUB_PATH, HUB)?.build();
        let signals = futures_lite::stream::block_on(zbus::block_on(
            MessageStream::for_match_rule(rule?, &self.0.bus, std::num::NonZeroUsize::new(256)),
        )?);
        let notifications = self.clone();
        Ok(thread::spawn(move || {
            for signal in signals {
                let signal = match signal {
                    Ok(signal) => signal,
                    Err(error) => return eprintln!("deckard-bridge: Hub signals: {error}"),
                };
                crate::report("Hub signal", notifications.signal(&signal));
            }
        }))
    }

    /// A message from the authenticated app bridge, in wire order.
    ///
    /// # Errors
    /// The Hub refused a call, or state or an icon could not be saved.
    pub fn handle(&self, message: &Map<String, Value>) -> Result<()> {
        match json::string(message, "t") {
            "post" => messages::post(message).map_or(Ok(()), |post| self.post(&post)),
            "remove" => self.withdraw(|key| key == json::string(message, "key")),
            "sync" => {
                let live = json::array(message, "keys");
                let live = &live[..live.len().min(MAX_SYNC_KEYS)];
                self.withdraw(|key| !live.iter().any(|live| live.as_str() == Some(key)))
            }
            "icon" => self.icon(message),
            "toast" => self.toast(message),
            _ => Ok(()),
        }
    }

    /// Posts or updates `post`.
    ///
    /// # Errors
    /// The Hub refused it, or the state could not be saved.
    pub fn post(&self, post: &Post) -> Result<()> {
        let digest = hex::encode(digest::display_digest(post));
        let mut state = self.lock();
        let replaces = match state.entries.get(&post.key) {
            Some(entry) if entry.digest == digest => return Ok(()),
            Some(entry) => entry.id,
            None if state.entries.len() >= MAX_TRACKED => return Ok(()),
            None => 0,
        };
        let id = self.notify(post, replaces)?;
        if id == 0 {
            return Ok(());
        }
        let entry = Entry {
            id,
            digest,
            reply: post.reply_action.clone(),
            auto_cancel: post.auto_cancel,
            invoked_at: None,
        };
        if let Some(previous) = state.entries.insert(post.key.clone(), entry)
            && previous.id != id
        {
            state.keys.remove(&previous.id);
        }
        state.keys.insert(id, post.key.clone());
        self.save(&state)
    }

    /// Stops tracking, in key order, every row whose key `stale` picks, and
    /// closes it on the Hub.
    fn withdraw(&self, stale: impl Fn(&str) -> bool) -> Result<()> {
        let mut state = self.lock();
        let mut closed = Ok(());
        let keys: Vec<String> = state.entries.keys().cloned().collect();
        for key in keys {
            if !stale(&key) {
                continue;
            }
            let Some(entry) = state.entries.remove(&key) else {
                continue;
            };
            state.keys.remove(&entry.id);
            closed = closed.and(self.hub("CloseNotification", &(entry.id,)).map(drop));
            self.save(&state)?;
        }
        closed
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        sync::lock(&self.0.state)
    }

    fn save(&self, state: &State) -> Result<()> {
        let path = &self.0.state_path;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let rows = serde_json::to_vec(&state.entries)?;
        Ok(files::replace(path, &rows, 0o600)?)
    }

    fn send(&self, message: Value) {
        if let Value::Object(message) = message {
            (self.0.to_android)(message);
        }
    }

    fn hub<B>(&self, method: &str, body: &B) -> Result<Message>
    where
        B: Serialize + zbus::DynamicType,
    {
        let bus = &self.0.bus;
        Ok(zbus::block_on(bus.call_method(
            Some(HUB),
            HUB_PATH,
            Some(HUB),
            method,
            body,
        ))?)
    }

    fn icon_url(&self, package: &str) -> String {
        let icon = self.0.icon_dir.join(format!("{package}.png"));
        match icon.is_file() {
            true => format!("file://{}", icon.display()),
            false => String::new(),
        }
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
        let mut actions = Vec::with_capacity(post.actions.len() * 2);
        for (id, label) in &post.actions {
            actions.extend([id.as_str(), label.as_str()]);
        }
        let text = html_escape(&post.body);
        let timeout = if post.ongoing { 0_i32 } else { -1_i32 };
        let icon = self.icon_url(post.package.as_str());
        let (label, title) = (post.label.as_str(), post.title.as_str());
        let body = (
            label,
            replaces_id,
            &icon,
            title,
            &text,
            actions,
            hints,
            timeout,
        );
        Ok(self.hub("Notify", &body)?.body().deserialize::<u32>()?)
    }

    fn signal(&self, message: &Message) -> Result<()> {
        let body = message.body();
        let header = message.header();
        let mut state = self.lock();
        let state = &mut *state;
        match header.member().map(|member| member.as_str()) {
            Some("ActionInvoked") => {
                let (id, action): (u32, String) = body.deserialize()?;
                if action == "inline-reply" {
                    return Ok(());
                }
                let tracked = state.keys.get(&id);
                let tracked = tracked.and_then(|key| Some((key, state.entries.get_mut(key)?)));
                let closing = state.closing.get_mut(&id);
                let closing = closing.map(|(key, entry, _, _)| (&*key, entry));
                if let Some((key, entry)) = tracked.or(closing) {
                    entry.invoked_at = Some(Instant::now());
                    let action = validate::truncate(&action, 16);
                    self.send(serde_json::json!({"t": "invoke", "key": key, "action": action}));
                }
            }
            Some("NotificationReplied") => {
                let (id, text): (u32, String) = body.deserialize()?;
                let key = state.keys.get(&id);
                let entry = key.and_then(|key| Some((key, state.entries.get(key)?)));
                if let Some((key, entry)) = entry.filter(|(_, entry)| !entry.reply.is_empty()) {
                    let text = validate::truncate(&text, 4096);
                    let reply = serde_json::json!({
                        "t": "reply", "key": key, "action": entry.reply, "text": text,
                    });
                    self.send(reply);
                }
            }
            Some("NotificationClosed") => {
                let (id, reason): (u32, u32) = body.deserialize()?;
                let Some(key) = state.keys.remove(&id) else {
                    return Ok(());
                };
                let Some(entry) = state.entries.remove(&key) else {
                    return Ok(());
                };
                self.save(state)?;
                if reason == 2 {
                    let token = state.next_token;
                    state.next_token = token.wrapping_add(1);
                    state
                        .closing
                        .insert(id, (key, entry, token, Instant::now()));
                    let due = (Instant::now() + DISMISS_DELAY, id, token);
                    if let Err(TrySendError::Full(_)) = self.0.dismissals.try_send(due) {
                        eprintln!("deckard-bridge: too many dismissals; {id} stays");
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn icon(&self, message: &Map<String, Value>) -> Result<()> {
        let package = json::string(message, "pkg");
        let png = validate::base64_png(json::string(message, "png"), MAX_ICON_BYTES);
        let png = png.and_then(|png| png_guard::sanitize(&png, MAX_ICON_BYTES, 128));
        let (Some(png), true) = (png, ids::is_package_name(package)) else {
            return Ok(());
        };
        fs::create_dir_all(&self.0.icon_dir)?;
        let path = self.0.icon_dir.join(format!("{package}.png"));
        Ok(files::replace(&path, &png, 0o600)?)
    }

    fn toast(&self, message: &Map<String, Value>) -> Result<()> {
        let package = json::string(message, "pkg");
        let text = json::text(message, "text", 512).trim().to_owned();
        if !ids::is_package_name(package) || text.is_empty() {
            return Ok(());
        }
        {
            let mut state = self.lock();
            let now = Instant::now();
            let last = state.last_toast.get(package);
            if last.is_some_and(|last| now.duration_since(*last) < TOAST_INTERVAL) {
                return Ok(());
            }
            let recent =
                |_: &String, last: &mut Instant| now.duration_since(*last) < TOAST_LIFETIME;
            state.last_toast.retain(recent);
            state.last_toast.insert(package.to_owned(), now);
        }
        let label = validate::label(json::string(message, "label"), package);
        let desktop_entry = keel::app_id(package);
        let hints = HashMap::from([
            ("desktop-entry", Variant::from(&desktop_entry)),
            ("urgency", Variant::from(0_u8)),
            ("transient", Variant::from(true)),
            ("suppress-sound", Variant::from(true)),
            ("x-marathon-toast", Variant::from(true)),
        ]);
        let icon = self.icon_url(package);
        let no_actions = Vec::<String>::new();
        let body = (&label, 0_u32, &icon, &text, "", no_actions, hints, 3500_i32);
        let id: u32 = self.hub("Notify", &body)?.body().deserialize()?;
        // Full: the Hub's own expiry closes it.
        let _ = self
            .0
            .expiring
            .try_send((Instant::now() + TOAST_LIFETIME, id));
        Ok(())
    }
}

/// Every delay is `DISMISS_DELAY`, so dismissals come due in queue order.
fn dismiss(notifications: &Weak<Inner>, due: &Receiver<Dismissal>) {
    for (deadline, id, token) in due {
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
        let Some(inner) = notifications.upgrade() else {
            return;
        };
        let notifications = Notifications(inner);
        let mut state = notifications.lock();
        let current = state.closing.get(&id).map(|(_, _, current, _)| *current);
        if current != Some(token) {
            continue;
        }
        let Some((key, entry, _, closed)) = state.closing.remove(&id) else {
            continue;
        };
        // Counted from the close, not from now: a late wake must not turn a
        // tap just before it into a dismissal. A tap after it counts too.
        let tapped = entry
            .invoked_at
            .is_some_and(|at| closed.saturating_duration_since(at) < TAP_GRACE);
        if entry.auto_cancel || !tapped {
            notifications.send(serde_json::json!({"t": "dismiss", "key": key}));
        }
    }
}

/// Every toast lives `TOAST_LIFETIME`, so they come due in queue order.
fn close_toasts(bus: &Connection, due: &Receiver<(Instant, u32)>) {
    for (deadline, id) in due {
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
        let _ = zbus::block_on(bus.call_method(
            Some(HUB),
            HUB_PATH,
            Some(HUB),
            "CloseNotification",
            &(id,),
        ));
    }
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
