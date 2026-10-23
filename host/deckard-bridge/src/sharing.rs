//! Android apps in Marathon's share sheet (protocol/bridge.md, Share). As
//! targets: the Shell asks `org.marathonos.Deckard1.Share` for the activities
//! that take a type and hands the chosen one the request, whose files are
//! copied into `xfer/<id>/`. As sources: DeckardSystem's `share_out` becomes
//! `share(a{sv})` on `org.sailfishos.share`, with the files as descriptors.

use std::collections::HashMap;
use std::fs;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_channel::Sender;
use async_io::Timer;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use deckard_core::{ids, png_guard, validate};
use futures_lite::future;
use serde_json::{Map, Value, json};
use zbus::message::Header;
use zbus::zvariant::{Dict, Fd, OwnedValue, Value as Variant};
use zbus::{fdo, interface};

use crate::api::ApiError;
use crate::contract;
use crate::intents::{self, NAVIGATION, SendSystem};

const MAX_RESOURCES: usize = 32;
const MAX_TARGETS: usize = 64;
const MAX_TEXT: usize = 65_536;
const MAX_BYTES: u64 = 1 << 30;
const MAX_PENDING: usize = 16;
const TIMEOUT: Duration = Duration::from_secs(15);
const BAD_REQUEST: &str = "a target and 1 to 32 resources";

/// Why a request was refused.
type Error = &'static str;

pub type VariantMap = HashMap<String, OwnedValue>;
type Answer = Result<Map<String, Value>, String>;

fn denied() -> ApiError {
    ApiError::Standard(fdo::Error::AccessDenied("only the shell shares".into()))
}

fn invalid(message: &str) -> ApiError {
    ApiError::Standard(fdo::Error::InvalidArgs(message.into()))
}

/// The answer a pending request waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Targets,
    Share,
}

#[derive(Default)]
struct State {
    generation: Option<u64>,
    pending: HashMap<String, (Kind, Sender<Answer>)>,
}

impl State {
    fn fail_all(&mut self) {
        for (_, (_, answer)) in self.pending.drain() {
            let _ = answer.try_send(Err("Android stopped".into()));
        }
    }
}

/// The `org.marathonos.Deckard1.Share` interface and DeckardSystem's share
/// messages.
#[derive(Clone)]
pub struct Sharing {
    bus: zbus::Connection,
    send: Arc<SendSystem>,
    xfer: PathBuf,
    state: Arc<Mutex<State>>,
}

impl Sharing {
    /// Shares through `xfer/<id>/`.
    pub fn new(bus: zbus::Connection, send: Arc<SendSystem>, xfer: PathBuf) -> Self {
        let state = Arc::default();
        Self {
            bus,
            send,
            xfer,
            state,
        }
    }

    /// DeckardSystem (re)connected: older requests fail.
    pub fn connected(&self, generation: u64) {
        let mut state = intents::lock(&self.state);
        state.generation = Some(generation);
        state.fail_all();
    }

    /// DeckardSystem left: pending requests fail.
    pub fn disconnected(&self, generation: u64) {
        let mut state = intents::lock(&self.state);
        if state.generation == Some(generation) {
            state.generation = None;
            state.fail_all();
        }
    }

    /// A message from the authenticated system channel.
    pub fn handle(&self, generation: u64, m: &Map<String, Value>) {
        let mut state = intents::lock(&self.state);
        let kind = match intents::field(m, "t") {
            _ if state.generation != Some(generation) => return,
            "share_out" => {
                drop(state);
                return self.share_out(m);
            }
            "share_targets_result" => Kind::Targets,
            "shared" => Kind::Share,
            _ => return,
        };
        let id = intents::field(m, "id");
        if state
            .pending
            .get(id)
            .is_some_and(|(waits, _)| *waits == kind)
            && let Some((_, answer)) = state.pending.remove(id)
        {
            let _ = answer.try_send(Ok(m.clone()));
        }
    }

    async fn shell(&self, header: Header<'_>) -> Result<(), ApiError> {
        let sender = header.sender().ok_or_else(denied)?.to_string();
        let pid = |name: String| async move {
            let dbus = (
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            );
            let method = "GetConnectionUnixProcessID";
            let reply = intents::call(&self.bus, dbus, method, &(name,), TIMEOUT).await?;
            reply.body().deserialize::<u32>()
        };
        match (pid(sender).await, pid(NAVIGATION.0.into()).await) {
            (Ok(caller), Ok(shell)) if caller != 0 && caller == shell => Ok(()),
            _ => Err(denied()),
        }
    }

    /// Sends `message` as request `id` and waits for DeckardSystem's answer.
    async fn request(&self, kind: Kind, id: String, mut message: Map<String, Value>) -> Answer {
        let (answer, answered) = async_channel::bounded(1);
        {
            let mut state = intents::lock(&self.state);
            if state.generation.is_none() {
                return Err("Android isn't running".into());
            }
            if state.pending.len() >= MAX_PENDING || state.pending.contains_key(&id) {
                return Err("too many shares".into());
            }
            state.pending.insert(id.clone(), (kind, answer));
        }
        message.insert("id".into(), id.clone().into());
        let reply = if (self.send)(message) {
            let timeout = async {
                Timer::after(TIMEOUT).await;
                Err("Android didn't answer".into())
            };
            let stopped = || Err("Android stopped".into());
            let answer = async { answered.recv().await.unwrap_or_else(|_| stopped()) };
            future::or(answer, timeout).await
        } else {
            Err("Android isn't running".into())
        };
        intents::lock(&self.state).pending.remove(&id);
        reply
    }

    fn share_out(&self, m: &Map<String, Value>) {
        let (id, pkg) = (intents::field(m, "id"), intents::field(m, "pkg"));
        if !intents::request_id(id) || !ids::is_package_name(pkg) {
            return;
        }
        let dir = intents::transfer(&self.xfer, id).ok();
        let mut resources = Vec::<Variant<'static>>::new();
        let mut total = 0_u64;
        for name in intents::list(m, "files") {
            let name = name.as_str().unwrap_or("");
            if name != intents::safe_name(name) || resources.len() >= MAX_RESOURCES {
                continue;
            }
            let Some(fd) = dir
                .as_ref()
                .and_then(|dir| intents::open_file(dir, name).ok())
            else {
                continue;
            };
            total += intents::file_size(fd.as_fd()).unwrap_or(0);
            if total > MAX_BYTES {
                break;
            }
            let name = Variant::from(name.to_owned());
            let item = [("name", name), ("fileDescriptor", Fd::from(fd).into())];
            resources.push(HashMap::from(item).into());
        }
        let title = validate::clean_text(intents::field(m, "title"), 256);
        let text = validate::clean_text(intents::field(m, "text"), MAX_TEXT);
        if !text.is_empty() && resources.len() < MAX_RESOURCES {
            let name = Variant::from(title.clone());
            resources.push(HashMap::from([("name", name), ("data", text.into())]).into());
        }
        if resources.is_empty() {
            return;
        }
        let mime = Some(intents::field(m, "mime")).filter(|mime| is_mime(mime));
        let configuration = HashMap::from([
            ("resources", Variant::from(resources)),
            ("mimeType", mime.unwrap_or("").to_owned().into()),
            ("title", title.into()),
            ("source", format!("waydroid.{pkg}").into()),
        ]);
        let (bus, pkg) = (self.bus.clone(), pkg.to_owned());
        let task = async move {
            let sheet = ("org.sailfishos.share", "/", "org.sailfishos.share");
            let body = (configuration,);
            if let Err(error) =
                intents::call(&bus, sheet, "share", &body, Duration::from_secs(10)).await
            {
                eprintln!("deckard-bridge: the share sheet refused {pkg}: {error}");
            }
        };
        let executor = self.bus.executor();
        executor.spawn(task, "deckard-share-out").detach();
    }
}

#[interface(name = "org.marathonos.Deckard1.Share")]
impl Sharing {
    /// `{id, pkg, label, icon, multiple}` for the activities taking
    /// ACTION_SEND of `mime_type`.
    #[zbus(name = "ShareTargets")]
    async fn share_targets(
        &self,
        mime_type: String,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<Vec<VariantMap>, ApiError> {
        self.shell(header).await?;
        if !is_mime(&mime_type) {
            return Err(invalid("not a MIME type"));
        }
        let message = intents::object(json!({"t": "share_targets", "mime": mime_type}));
        let reply = self.request(Kind::Targets, new_id()?, message).await;
        Ok(reply.map(|reply| targets(&reply)).unwrap_or_default())
    }

    /// The sheet's request (`mimeType`, `title`, `resources`) for target `id`.
    #[zbus(name = "Share")]
    async fn share(
        &self,
        id: String,
        configuration: VariantMap,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        self.shell(header).await?;
        let value = |key| configuration.get(key).and_then(|v| string(v));
        let value = |key| value(key).unwrap_or_default();
        let (mut mime, title) = (value("mimeType"), validate::truncate(&value("title"), 256));
        if !target_id(&id) || (!mime.is_empty() && !is_mime(&mime)) {
            return Err(invalid(BAD_REQUEST));
        }
        let (tid, xfer) = (new_id()?, self.xfer.clone());
        let copy_tid = tid.clone();
        let copied = blocking::unblock(move || copy_in(&xfer, &copy_tid, &configuration)).await;
        let (files, joined) = copied.map_err(|error| {
            remove(&self.xfer, &tid);
            invalid(error)
        })?;
        if mime.is_empty() {
            let guess = files.first().map(mime_guess::from_path);
            let guess = guess.map(|guess| guess.first_or_octet_stream().essence_str().to_owned());
            mime = guess.unwrap_or_else(|| "text/plain".into());
        }
        let pkg = id.split('/').next().unwrap_or("");
        let expect = (format!("waydroid.{pkg}"),);
        let wait = Duration::from_secs(2);
        let _ = intents::call(&self.bus, NAVIGATION, "ExpectAppWindow", &expect, wait).await;
        let message = json!({
            "t": "share", "target": id, "mime": mime, "title": title, "text": joined, "files": files,
        });
        let reply = self
            .request(Kind::Share, tid.clone(), intents::object(message))
            .await;
        let failure = match reply {
            Ok(reply) if reply.get("ok").and_then(Value::as_bool) == Some(true) => return Ok(()),
            Ok(reply) => validate::clean_text(intents::field(&reply, "error"), 512),
            Err(error) => error,
        };
        remove(&self.xfer, &tid);
        eprintln!("deckard-bridge: share failed: {failure}");
        Err(ApiError::Failed(failure))
    }
}

fn remove(xfer: &Path, tid: &str) {
    if intents::request_id(tid) {
        let _ = fs::remove_dir_all(xfer.join(tid));
    }
}

/// A random transfer ID.
fn new_id() -> Result<String, ApiError> {
    let random = getrandom::u64().map_err(|error| ApiError::Failed(error.to_string()))?;
    Ok(format!("{random:016x}"))
}

fn bare<'a, 'v>(value: &'a Variant<'v>) -> &'a Variant<'v> {
    match value {
        Variant::Value(inner) => bare(inner),
        value => value,
    }
}

fn string(value: &Variant<'_>) -> Option<String> {
    match bare(value) {
        Variant::Str(text) => Some(text.as_str().to_owned()),
        _ => None,
    }
}

fn entry<'a, 'v>(dict: &'a Dict<'v, 'v>, key: &str) -> Option<&'a Variant<'v>> {
    let mut entries = dict.iter();
    let found = entries.find(|(name, _)| string(name).is_some_and(|name| name == key));
    found.map(|(_, value)| bare(value))
}

/// Copies the request's files (`resources`, as `av` or `aa{sv}`) into
/// `xfer/<tid>/`; their names and the joined text.
fn copy_in(xfer: &Path, tid: &str, request: &VariantMap) -> Result<(Vec<String>, String), Error> {
    let items = match request.get("resources").map(|resources| bare(resources)) {
        Some(Variant::Array(items)) if (1..=MAX_RESOURCES).contains(&items.len()) => items,
        _ => return Err(BAD_REQUEST),
    };
    let (mut files, mut texts, mut budget, mut dir) = (Vec::new(), Vec::new(), MAX_BYTES, None);
    for item in items.inner() {
        let dict = match bare(item) {
            Variant::Dict(dict) => Some(dict),
            _ => None,
        };
        let get = |key| dict.and_then(|dict| entry(dict, key));
        let fd = match get("fileDescriptor") {
            Some(Variant::Fd(fd)) => fd,
            Some(_) => return Err("not a file"),
            None => {
                let text = get("data").and_then(string).unwrap_or_default();
                if !(1..=MAX_TEXT).contains(&text.encode_utf16().count()) {
                    return Err("text of 1 to 65536 characters");
                }
                texts.push(text);
                continue;
            }
        };
        intents::file_size(fd.as_fd()).ok_or("not a file")?;
        if dir.is_none() {
            dir = intents::new_transfer(xfer, tid).ok();
        }
        let dir = dir.as_ref().ok_or("cannot write the transfer")?;
        let name = intents::safe_name(&get("name").and_then(string).unwrap_or_default());
        files.push(intents::copy_into(dir, &name, 2, fd.as_fd(), &mut budget)?.0);
    }
    Ok((files, texts.join("\n")))
}

/// `<pkg>/<activity class>`, at most 256 characters.
pub fn target_id(id: &str) -> bool {
    let Some((pkg, class)) = id.split_once('/') else {
        return false;
    };
    let part = |part: &str| {
        let word = |b: u8| b.is_ascii_alphanumeric() || b"_$".contains(&b);
        let first = part.bytes().next().is_some_and(|b| !b.is_ascii_digit());
        first && part.bytes().all(word)
    };
    let class = class.strip_prefix('.').unwrap_or(class);
    id.len() <= 256 && ids::is_package_name(pkg) && class.split('.').all(part)
}

/// `type/subtype`, at most 128 characters.
fn is_mime(mime: &str) -> bool {
    let word = |c: u8| c.is_ascii_alphanumeric() || b"_.+-".contains(&c);
    let Some((kind, subtype)) = mime.split_once('/') else {
        return false;
    };
    let parts = !kind.is_empty() && !subtype.is_empty() && kind.bytes().all(word);
    mime.len() <= 128 && parts && subtype.bytes().all(|c| word(c) || c == b'*')
}

/// DeckardSystem's activities as the Shell takes them.
fn targets(reply: &Map<String, Value>) -> Vec<VariantMap> {
    let mut out = Vec::new();
    for target in intents::list(reply, "targets") {
        let Some(target) = target.as_object() else {
            continue;
        };
        let pkg = intents::field(target, "pkg");
        let id = format!("{pkg}/{}", intents::field(target, "activity"));
        if !target_id(&id) || pkg.starts_with("org.marathonos.deckard.") {
            continue;
        }
        let png = validate::base64_png(intents::field(target, "icon"), 256 * 1024);
        let png = png.and_then(|png| png_guard::sanitize(&png, 256 * 1024, 96));
        let icon = png.map(|png| format!("data:image/png;base64,{}", STANDARD.encode(png)));
        let multiple = target.get("multiple").and_then(Value::as_bool);
        let entries = [
            ("id", Variant::from(id)),
            ("pkg", pkg.into()),
            (
                "label",
                contract::runtime_label(intents::field(target, "label")).into(),
            ),
            ("icon", icon.unwrap_or_default().into()),
            ("multiple", multiple.unwrap_or(false).into()),
        ];
        let mut map = VariantMap::new();
        for (key, value) in entries {
            if let Ok(value) = OwnedValue::try_from(value) {
                map.insert(key.into(), value);
            }
        }
        out.push(map);
        if out.len() >= MAX_TARGETS {
            break;
        }
    }
    out
}
