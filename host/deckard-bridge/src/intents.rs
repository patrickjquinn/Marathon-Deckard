//! DeckardSystem's intents (protocol/bridge.md, system channel): links,
//! numbers, addresses, places and settings pages routed to Marathon apps, and
//! file pickers and camera captures whose files cross into `xfer/<id>/`.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use async_channel::Receiver;
use async_io::Timer;
use deckard_core::{ids, text, validate};
use futures_lite::{StreamExt, future};
use percent_encoding::CONTROLS;
use regex::Regex;
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags};
use rustix::path::Arg;
use serde::Serialize;
use serde_json::{Map, Value, json};
use zbus::message::Message;
use zbus::zvariant::{DynamicType, OwnedValue, Value as Variant};

/// Queues a message on the authenticated system channel; false while
/// DeckardSystem is not connected.
pub type SendSystem = dyn Fn(Map<String, Value>) -> bool + Send + Sync;

/// A D-Bus service, object path and interface.
pub(crate) type Endpoint<'a> = (&'a str, &'a str, &'a str);

pub(crate) const NAVIGATION: Endpoint<'static> = (
    "org.marathonos.Shell",
    "/org/marathonos/Shell/Navigation",
    "org.marathonos.Shell.Navigation1",
);
const KEEL_PATH: &str = "/org/shipwright/Keel/Actions";
const KEEL: &str = "org.shipwright.Keel.Actions";
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const REQUEST: &str = "org.freedesktop.portal.Request";
const MAX_PENDING: usize = 4;
const MAX_SAVED: usize = 32;
const MAX_FILES: usize = 32;
const MAX_BYTES: u64 = 1 << 30;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(630);
const XFER_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const WRITE_FAILED: &str = "cannot write the transfer";

static POINT: LazyLock<Regex> = LazyLock::new(|| {
    let (n, s) = (r"(-?(?-u:\d){1,3}(?:\.(?-u:\d)+)?)", r"(?-u:\s)*");
    let pattern = format!(r"^{s}{n}{s},{s}{n}{s}(?:\((.*)\))?{s}$");
    Regex::new(&pattern).unwrap_or_else(|error| panic!("invalid fixed geo pattern: {error}"))
});
static MIME_RULE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = r"^[a-z0-9.+-]{1,64}/[a-z0-9.+*-]{1,64}$";
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid fixed MIME pattern: {error}"))
});

/// Android settings actions and Marathon Settings' pages.
#[rustfmt::skip]
const PAGES: &[(&str, &str)] = &[
    ("android.settings.WIFI_SETTINGS", "wifi"),
    ("android.settings.WIRELESS_SETTINGS", "wifi"),
    ("android.settings.panel.action.INTERNET_CONNECTIVITY", "wifi"),
    ("android.settings.panel.action.WIFI", "wifi"),
    ("android.settings.BLUETOOTH_SETTINGS", "bluetooth"),
    ("android.settings.SOUND_SETTINGS", "sound"),
    ("android.settings.panel.action.VOLUME", "sound"),
    ("android.settings.DISPLAY_SETTINGS", "display"),
    ("android.settings.DATE_SETTINGS", "datetime"),
    ("android.settings.INTERNAL_STORAGE_SETTINGS", "storage"),
    ("android.settings.MEMORY_CARD_SETTINGS", "storage"),
    ("android.settings.BATTERY_SAVER_SETTINGS", "battery"),
    ("android.intent.action.POWER_USAGE_SUMMARY", "battery"),
    ("android.settings.SECURITY_SETTINGS", "security"),
    ("android.settings.INPUT_METHOD_SETTINGS", "keyboard"),
    ("android.settings.SYNC_SETTINGS", "accounts"),
    ("android.settings.ADD_ACCOUNT_SETTINGS", "accounts"),
    ("android.settings.DEVICE_INFO_SETTINGS", "about"),
    ("android.settings.NOTIFICATION_SETTINGS", "notifications"),
    ("android.settings.ZEN_MODE_SETTINGS", "notifications"),
    ("android.settings.APP_NOTIFICATION_SETTINGS", "notifications"),
    ("android.settings.CHANNEL_NOTIFICATION_SETTINGS", "notifications"),
    ("android.settings.APPLICATION_DETAILS_SETTINGS", "appmanager"),
    ("android.settings.MANAGE_APPLICATIONS_SETTINGS", "appmanager"),
    ("android.settings.MANAGE_ALL_APPLICATIONS_SETTINGS", "appmanager"),
];

/// Special access: Marathon Settings' page for the kind.
#[rustfmt::skip]
const ACCESS: &[(&str, &str)] = &[
    ("android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION", "all_files"),
    ("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION", "all_files"),
    ("android.settings.MANAGE_UNKNOWN_APP_SOURCES", "install_apps"),
    ("android.settings.action.MANAGE_OVERLAY_PERMISSION", "overlay"),
    ("android.settings.action.MANAGE_WRITE_SETTINGS", "write_settings"),
    ("android.settings.USAGE_ACCESS_SETTINGS", "usage"),
    ("android.settings.ACTION_NOTIFICATION_LISTENER_SETTINGS", "notification_listener"),
    ("android.settings.NOTIFICATION_LISTENER_DETAIL_SETTINGS", "notification_listener"),
    ("android.settings.NOTIFICATION_POLICY_ACCESS_SETTINGS", "dnd"),
    ("android.settings.IGNORE_BATTERY_OPTIMIZATION_SETTINGS", "battery"),
    ("android.settings.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS", "battery"),
    ("android.settings.REQUEST_SCHEDULE_EXACT_ALARM", "alarms"),
    ("android.settings.PICTURE_IN_PICTURE_SETTINGS", "pip"),
    ("android.settings.ACCESSIBILITY_SETTINGS", "accessibility"),
];

/// Where an `open` goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// `Navigation1.LaunchAppWithRoute`: app, route, arguments.
    Route(&'static str, String, Map<String, Value>),
    /// The Keel action of `org.marathonos.<app>`: app, action, arguments.
    Keel(&'static str, &'static str, Map<String, Value>),
    /// `Navigation1.LaunchApp`.
    Launch(&'static str),
}

/// Routes an `open` message from DeckardSystem's intent stubs.
///
/// # Errors
///
/// Why the message names nothing Marathon opens.
pub fn route(m: &Map<String, Value>) -> Result<Target, &'static str> {
    let (kind, uri) = (field(m, "kind"), field(m, "uri"));
    if uri.encode_utf16().count() > 4096 {
        return Err("uri too long");
    }
    let scheme = uri.split(':').next().unwrap_or("").to_lowercase();
    match kind {
        "url" => {
            let url = web(uri, &scheme).ok_or("not a web address")?;
            let args = object(json!({"url": url, "newTab": true}));
            Ok(Target::Keel("browser", "browser.open", args))
        }
        "tel" | "sms" => phone(kind == "tel", &scheme, m),
        "mailto" => Ok(mail(m)),
        "geo" if scheme == "geo" => Ok(geo(uri)),
        "geo" => Err("not a geo: URI"),
        "settings" => match settings_route(field(m, "action"), field(m, "pkg")) {
            route if route.is_empty() => Ok(Target::Launch("settings")),
            route => Ok(Target::Route("settings", route, Map::new())),
        },
        _ => Err("unknown kind"),
    }
}

/// `uri` as a web address with its path, query and fragment encoded, or
/// None when it is not one.
fn web(uri: &str, scheme: &str) -> Option<String> {
    let (_, rest) = uri.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let forbidden = |c: char| c.is_ascii_control() || " \"<>\\^`{|}".contains(c);
    let hex = |part: &str| {
        part.get(..2)
            .is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    let web = scheme == "http" || scheme == "https";
    let host_ok = !host.is_empty() && !host.starts_with(':');
    let chars_ok = !uri.contains(forbidden) && uri.split('%').skip(1).all(hex);
    if !web || !host_ok || !chars_ok || uri.encode_utf16().count() > 2048 {
        return None;
    }
    let tail = rest.strip_prefix(authority).unwrap_or("");
    let head = uri.strip_suffix(tail).unwrap_or(uri);
    let tail = percent_encoding::utf8_percent_encode(tail, CONTROLS);
    Some(format!("{head}{tail}"))
}

fn phone(tel: bool, scheme: &str, m: &Map<String, Value>) -> Result<Target, &'static str> {
    let sms = ["sms", "smsto", "mms", "mmsto"].contains(&scheme);
    if (tel && scheme != "tel") || (!tel && !sms) {
        return Err("wrong scheme");
    }
    let uri = field(m, "uri");
    let app = if tel { "phone" } else { "messages" };
    let number = number(uri);
    let mut body = String::new();
    if !tel {
        body = validate::clean_text(field(m, "body"), 10_000);
        if body.is_empty() {
            body = validate::clean_text(&query(uri, "body"), 10_000);
        }
    }
    let route = match (tel, number.is_empty()) {
        (true, _) => "/call",
        (false, true) => "compose",
        (false, false) => "conversation",
    };
    let args = filled([("number", number), ("body", body)]);
    if args.is_empty() {
        return Ok(Target::Launch(app));
    }
    Ok(Target::Route(app, route.into(), args))
}

/// The pairs whose values are not empty.
fn filled<const N: usize>(pairs: [(&str, String); N]) -> Map<String, Value> {
    let mut args = Map::new();
    for (key, value) in pairs {
        if !value.is_empty() {
            args.insert(key.into(), value.into());
        }
    }
    args
}

/// The number in a tel:, sms: or smsto: URI, or "" if it has none.
fn number(uri: &str) -> String {
    let rest = uri.split_once(':').map_or("", |(_, rest)| rest);
    let decoded = percent_encoding::percent_decode_str(rest).decode_utf8_lossy();
    let head = decoded.split(['?', ',']).next().unwrap_or("");
    let number: String = head.chars().filter(|c| !" ().-".contains(*c)).collect();
    let dialable = |c: char| c.is_ascii_digit() || "+*#,;pw".contains(c);
    let fits = number.len() <= 64 && number.chars().all(dialable);
    if fits { number } else { String::new() }
}

/// The first `key` of the URI's query, percent-decoded.
fn query(uri: &str, key: &str) -> String {
    let Some((_, query)) = uri.split_once('?') else {
        return String::new();
    };
    let pairs = query.split('&');
    for pair in pairs {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == key {
            return percent_encoding::percent_decode_str(value)
                .decode_utf8_lossy()
                .into_owned();
        }
    }
    String::new()
}

fn mail(m: &Map<String, Value>) -> Target {
    let args = filled([
        ("to", addresses(m, "to")),
        ("cc", addresses(m, "cc")),
        ("subject", validate::clean_text(field(m, "subject"), 1000)),
        ("body", validate::clean_text(field(m, "body"), 100_000)),
    ]);
    Target::Keel("email", "mail.compose", args)
}

fn addresses(m: &Map<String, Value>, key: &str) -> String {
    let mut out = Vec::new();
    for value in list(m, key) {
        let address = value.as_str().unwrap_or("").trim();
        let length = address.encode_utf16().count();
        let odd = |c: char| c.is_ascii_whitespace() || "\x0b,<>\"".contains(c);
        if (1..=254).contains(&length) && !address.contains(odd) && out.len() < 20 {
            out.push(address);
        }
    }
    out.join(",")
}

/// geo:lat,lon[;u=..][?q=lat,lon(label)|text]
fn geo(uri: &str) -> Target {
    let body = uri.get(4..).unwrap_or("");
    let place = body.split(['?', ';']).next().unwrap_or("");
    let q = query(uri, "q");
    if let Some(args) = point(&q).or_else(|| point(place)) {
        return Target::Keel("maps", "map.show", args);
    }
    match validate::clean_text(&q, 256).trim() {
        "" => Target::Launch("maps"),
        text => Target::Keel("maps", "search.show", object(json!({"query": text}))),
    }
}

fn point(text: &str) -> Option<Map<String, Value>> {
    let captures = POINT.captures(text)?;
    let lat: f64 = captures.get(1)?.as_str().parse().ok()?;
    let lon: f64 = captures.get(2)?.as_str().parse().ok()?;
    let nowhere = lat == 0.0 && lon == 0.0;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) || nowhere {
        return None;
    }
    let mut args = object(json!({"lat": lat, "lon": lon}));
    let name = text::truncate(captures.get(3).map_or("", |name| name.as_str()), 256);
    if !name.is_empty() {
        args.insert("name".into(), name.into());
    }
    Some(args)
}

/// Marathon Settings' page for an Android settings action, or "" for none;
/// per app when it names one.
pub fn settings_route(action: &str, pkg: &str) -> String {
    let find = |table: &[(&str, &'static str)]| {
        let found = table.iter().find(|(key, _)| *key == action);
        found.map(|(_, page)| *page)
    };
    let valid = ids::is_package_name(pkg);
    match (find(ACCESS), find(PAGES)) {
        (Some(kind), _) if valid => format!("androidaccess/{kind}/{pkg}"),
        (Some(kind), _) => format!("androidaccess/{kind}"),
        (None, Some(page @ ("notifications" | "appmanager"))) if valid => {
            format!("{page}/waydroid.{pkg}")
        }
        (None, page) => page.unwrap_or("").into(),
    }
}

/// A transfer ID: 16 lowercase hex digits.
pub fn request_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The last component of `path`, fit to be a file name in `xfer/<id>/`.
pub fn safe_name(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or("");
    let kept: String = base.chars().filter(|c| *c >= ' ' && *c != '\\').collect();
    match text::truncate(&kept, 200).trim() {
        "" | "." | ".." => "file".into(),
        name => name.into(),
    }
}

pub(crate) fn field<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn list<'a>(m: &'a Map<String, Value>, key: &str) -> &'a [Value] {
    let value = m.get(key).and_then(Value::as_array);
    value.map_or(&[], Vec::as_slice)
}

pub(crate) fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Calls `method` of `to`, failing once `limit` has passed.
pub(crate) async fn call<B>(
    bus: &zbus::Connection,
    (service, path, interface): Endpoint<'_>,
    method: &str,
    body: &B,
    limit: Duration,
) -> zbus::Result<Message>
where
    B: Serialize + DynamicType,
{
    let call = bus.call_method(Some(service), path, Some(interface), method, body);
    let timeout = async {
        Timer::after(limit).await;
        Err(zbus::Error::Failure("timed out".into()))
    };
    future::or(call, timeout).await
}

/// Runs `target` on the session bus.
///
/// # Errors
///
/// The bus error of the call, or a timeout after 30 seconds.
pub async fn launch(bus: &zbus::Connection, target: &Target) -> zbus::Result<()> {
    let json = |args: &Map<String, Value>| Value::Object(args.clone()).to_string();
    let reply = match target {
        Target::Keel(app, action, args) => {
            let service = format!("org.marathonos.{app}");
            let body = (*action, json(args), HashMap::<&str, Variant<'_>>::new());
            let keel = (service.as_str(), KEEL_PATH, KEEL);
            call(bus, keel, "Invoke", &body, CALL_TIMEOUT).await
        }
        Target::Route(app, route, args) => {
            let body = (*app, route.as_str(), json(args));
            call(bus, NAVIGATION, "LaunchAppWithRoute", &body, CALL_TIMEOUT).await
        }
        Target::Launch(app) => call(bus, NAVIGATION, "LaunchApp", &(*app,), CALL_TIMEOUT).await,
    };
    reply.map(drop)
}

/// Opens `name` in `dir`, never through a final link.
fn open_at(dir: impl AsFd, name: impl Arg, flags: OFlags, mode: u32) -> io::Result<OwnedFd> {
    let flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mode = Mode::from_raw_mode(mode);
    Ok(rustix::fs::openat(dir, name, flags, mode)?)
}

/// `xfer/<id>/`, never through a link.
pub(crate) fn transfer(root: &Path, id: &str) -> io::Result<OwnedFd> {
    if !request_id(id) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let root = open_at(CWD, root, OFlags::RDONLY | OFlags::DIRECTORY, 0)?;
    open_at(&root, id, OFlags::RDONLY | OFlags::DIRECTORY, 0)
}

/// Makes `xfer/<id>/` and opens it.
pub(crate) fn new_transfer(root: &Path, id: &str) -> io::Result<OwnedFd> {
    fs::create_dir_all(root)?;
    let dir = open_at(CWD, root, OFlags::RDONLY | OFlags::DIRECTORY, 0)?;
    if request_id(id) {
        let _ = rustix::fs::mkdirat(&dir, id, Mode::from_raw_mode(0o777));
    }
    transfer(root, id)
}

/// A regular file in `dir`, never through a link nor blocking on a FIFO.
pub(crate) fn open_file(dir: impl AsFd, name: impl Arg) -> io::Result<OwnedFd> {
    let fd = open_at(dir, name, OFlags::RDONLY | OFlags::NONBLOCK, 0)?;
    file_size(fd.as_fd()).ok_or(io::ErrorKind::InvalidInput)?;
    Ok(fd)
}

/// The size of `fd` when it is a regular file.
pub(crate) fn file_size(fd: BorrowedFd<'_>) -> Option<u64> {
    let stat = rustix::fs::fstat(fd).ok()?;
    let regular = FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile;
    u64::try_from(stat.st_size).ok().filter(|_| regular)
}

fn create(dir: impl AsFd, name: impl Arg) -> io::Result<OwnedFd> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL;
    open_at(dir, name, flags, 0o666)
}

/// Copies `input` (at most `size` bytes) from its start into `output`.
fn copy_data(input: BorrowedFd<'_>, size: u64, output: OwnedFd) -> Result<u64, &'static str> {
    let mut output = File::from(output);
    let mut buffer = vec![0; 256 * 1024];
    let mut offset = 0_u64;
    loop {
        let read = rustix::io::pread(input, &mut buffer[..], offset);
        let n = read.map_err(|_| "cannot read a file")?;
        let Some(chunk) = buffer.get(..n).filter(|chunk| !chunk.is_empty()) else {
            return Ok(offset);
        };
        output.write_all(chunk).map_err(|_| WRITE_FAILED)?;
        offset += chunk.len() as u64;
        if offset > size {
            return Err("a file grew while it was copied");
        }
    }
}

/// Copies `input` into `dir` as `base`, or `<n>-base` counting from
/// `first`, taking its size from `budget`; the name and bytes copied.
///
/// # Errors
///
/// What stopped the copy; a partial copy is removed.
pub(crate) fn copy_into(
    dir: &OwnedFd,
    base: &str,
    first: usize,
    input: BorrowedFd<'_>,
    budget: &mut u64,
) -> Result<(String, u64), &'static str> {
    let size = file_size(input).ok_or("not a file")?;
    if size > *budget {
        return Err("too large");
    }
    let mut name = base.to_owned();
    for n in first..=first + MAX_FILES {
        let output = match create(dir, name.as_str()) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                name = format!("{n}-{base}");
                continue;
            }
            output => output.map_err(|_| WRITE_FAILED)?,
        };
        let copied = copy_data(input, size, output);
        if copied.is_err() {
            let _ = rustix::fs::unlinkat(dir, name.as_str(), AtFlags::empty());
        }
        let copied = copied?;
        *budget -= copied;
        return Ok((name, copied));
    }
    Err(WRITE_FAILED)
}

/// Copies host files into `xfer/<id>/` and lists them as `picked` does.
fn copy_in(xfer: &Path, id: &str, paths: &[PathBuf]) -> Vec<Value> {
    let dir = (!paths.is_empty()).then(|| new_transfer(xfer, id));
    let Some(Ok(dir)) = dir else {
        return Vec::new();
    };
    let mut budget = MAX_BYTES;
    let mut files = Vec::new();
    let paths = paths.iter().take(MAX_FILES);
    for path in paths {
        let base = safe_name(&path.to_string_lossy());
        let input = open_file(CWD, path).map_err(|_| "not a file");
        let copied = input.and_then(|fd| copy_into(&dir, &base, 1, fd.as_fd(), &mut budget));
        match copied {
            Ok((name, size)) => files.push(json!({"name": name, "size": size})),
            Err(error) => eprintln!("deckard-bridge: copy of {base} failed: {error}"),
        }
    }
    files
}

/// Writes `xfer/<id>/<name of target>` over `target`, through a
/// `.deckard-part` beside it.
fn save(xfer: &Path, id: &str, target: &Path) -> io::Result<()> {
    let name = safe_name(&target.to_string_lossy());
    let input = open_file(transfer(xfer, id)?, name.as_str())?;
    let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(io::ErrorKind::InvalidInput.into());
    };
    let mut part = OsString::from(name);
    part.push(".deckard-part");
    let parent = parent.canonicalize()?;
    let parent = open_at(CWD, parent, OFlags::RDONLY | OFlags::DIRECTORY, 0)?;
    let _ = rustix::fs::unlinkat(&parent, &part, AtFlags::empty());
    let size = file_size(input.as_fd()).unwrap_or(0).min(MAX_BYTES);
    let copied = copy_data(input.as_fd(), size, create(&parent, &part)?);
    let saved = copied
        .map_err(io::Error::other)
        .and_then(|_| Ok(rustix::fs::renameat(&parent, &part, &parent, name)?));
    if saved.is_err() {
        let _ = rustix::fs::unlinkat(&parent, &part, AtFlags::empty());
    }
    saved
}

/// Removes transfers older than a month; an app that kept a grant to one
/// gets "file not found".
fn sweep(xfer: &Path) {
    let cutoff = SystemTime::now().checked_sub(XFER_MAX_AGE);
    let (Ok(entries), Some(cutoff)) = (fs::read_dir(xfer), cutoff) else {
        return;
    };
    let entries = entries.flatten();
    for entry in entries {
        let named = entry.file_name().to_str().is_some_and(request_id);
        let meta = entry.metadata().ok().filter(fs::Metadata::is_dir);
        let modified = meta.and_then(|meta| meta.modified().ok());
        if named && modified.is_some_and(|time| time < cutoff) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// The file a `file://` URI names.
fn file_path(uri: &str) -> Option<PathBuf> {
    let path = uri.strip_prefix("file://")?;
    let bytes: Vec<u8> = percent_encoding::percent_decode_str(path).collect();
    let local = path.starts_with('/') && !bytes.contains(&0);
    local.then(|| OsString::from_vec(bytes).into())
}

/// What a `pick` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    Open,
    Save,
    Photo,
    Video,
}

/// A pick in flight; dropping its sender ends the pick's task.
type Pending = (Ask, async_channel::Sender<()>);
type Options = HashMap<&'static str, OwnedValue>;

#[derive(Default)]
struct State {
    generation: Option<u64>,
    focus: String,
    pending: HashMap<String, Pending>,
    saved: VecDeque<(String, PathBuf)>,
}

/// Opens, picks and captures for DeckardSystem.
#[derive(Clone)]
pub struct Intents {
    bus: zbus::Connection,
    send: Arc<SendSystem>,
    xfer: PathBuf,
    camera: PathBuf,
    state: Arc<Mutex<State>>,
}

/// Locks `state`; a panic while it was held is fatal.
pub(crate) fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|_| {
        eprintln!("deckard-bridge: state lock poisoned");
        std::process::abort()
    })
}

impl Intents {
    /// Makes and sweeps `xfer`, and follows the Shell's foreground app.
    /// `camera` is Marathon Camera's folder.
    ///
    /// # Errors
    ///
    /// When `xfer` cannot be created.
    pub fn start(
        bus: zbus::Connection,
        send: Arc<SendSystem>,
        xfer: PathBuf,
        camera: PathBuf,
    ) -> io::Result<Self> {
        fs::create_dir_all(&xfer)?;
        sweep(&xfer);
        let state = Arc::default();
        let intents = Self {
            bus,
            send,
            xfer,
            camera,
            state,
        };
        let follower = intents.clone();
        intents.spawn("deckard-foreground", async move {
            if let Err(error) = follower.follow_foreground().await {
                eprintln!("deckard-bridge: foreground watch failed: {error}");
            }
        });
        Ok(intents)
    }

    fn spawn(&self, name: &str, task: impl Future<Output = ()> + Send + 'static) {
        self.bus.executor().spawn(task, name).detach();
    }

    fn send(&self, message: Value) {
        (self.send)(object(message));
    }

    /// DeckardSystem connected: it hears which Android app is in front.
    pub fn connected(&self, generation: u64) {
        let mut state = lock(&self.state);
        state.generation = Some(generation);
        let focus = state.focus.clone();
        drop(state);
        self.send(json!({"t": "focus", "pkg": focus}));
    }

    /// DeckardSystem left: its picks end and their choosers close.
    pub fn disconnected(&self, generation: u64) {
        let mut state = lock(&self.state);
        if state.generation == Some(generation) {
            state.generation = None;
            state.pending.clear();
        }
    }

    /// The Shell's foreground app changed: DeckardSystem hears the Android
    /// package in front, or "" for none.
    pub fn foreground_changed(&self, app: &str) {
        let pkg = app.strip_prefix("waydroid.").unwrap_or("");
        let pkg = if ids::is_package_name(pkg) { pkg } else { "" };
        let mut state = lock(&self.state);
        if state.focus != pkg {
            state.focus = pkg.into();
            drop(state);
            self.send(json!({"t": "focus", "pkg": pkg}));
        }
    }

    async fn follow_foreground(&self) -> zbus::Result<()> {
        let (service, path, interface) = NAVIGATION;
        let shell = zbus::Proxy::new(&self.bus, service, path, interface).await?;
        let mut changes = shell.receive_signal("ForegroundAppChanged").await?;
        while let Some(signal) = changes.next().await {
            if let Ok(app) = signal.body().deserialize::<String>() {
                self.foreground_changed(&app);
            }
        }
        Ok(())
    }

    /// A message from the authenticated system channel.
    pub fn handle(&self, generation: u64, m: &Map<String, Value>) {
        if lock(&self.state).generation != Some(generation) {
            return;
        }
        match field(m, "t") {
            "open" => self.open(m),
            "pick" => self.pick(m),
            "pick_done" => self.pick_done(field(m, "id")),
            "pick_cancel" => drop(lock(&self.state).pending.remove(field(m, "id"))),
            _ => {}
        }
    }

    fn open(&self, m: &Map<String, Value>) {
        let target = match route(m) {
            Ok(target) => target,
            Err(error) => return eprintln!("deckard-bridge: open refused: {error}"),
        };
        let bus = self.bus.clone();
        self.spawn("deckard-open", async move {
            if let Err(error) = launch(&bus, &target).await {
                eprintln!("deckard-bridge: open failed: {error}");
            }
        });
    }

    fn picked(&self, id: &str, files: Vec<Value>) {
        let ok = !files.is_empty();
        self.send(json!({"t": "picked", "id": id, "ok": ok, "files": files}));
    }

    fn pick(&self, m: &Map<String, Value>) {
        let id = field(m, "id");
        let ask = match (field(m, "mode"), field(m, "kind")) {
            ("capture", "video") => Some(Ask::Video),
            ("capture", _) => Some(Ask::Photo),
            ("open", _) => Some(Ask::Open),
            ("save", _) => Some(Ask::Save),
            _ => None,
        };
        let mut state = lock(&self.state);
        if !request_id(id) || state.pending.contains_key(id) {
            return;
        }
        let Some(ask) = ask.filter(|_| state.pending.len() < MAX_PENDING) else {
            drop(state);
            return self.picked(id, Vec::new());
        };
        let (cancel, cancelled) = async_channel::bounded(1);
        state.pending.insert(id.into(), (ask, cancel));
        drop(state);
        let options = portal_options(m, id, ask);
        let (this, id) = (self.clone(), id.to_owned());
        self.spawn("deckard-pick", async move {
            if let Err(error) = this.run(id, ask, options, cancelled).await {
                eprintln!("deckard-bridge: pick failed: {error}");
            }
        });
    }

    async fn run(
        &self,
        id: String,
        ask: Ask,
        opts: Options,
        gone: Receiver<()>,
    ) -> zbus::Result<()> {
        let asked = async {
            let chosen = match ask {
                Ask::Photo | Ask::Video => self.capture(ask).await,
                Ask::Open | Ask::Save => self.choose(&id, ask, opts).await,
            };
            Some(chosen.unwrap_or_else(|error| {
                eprintln!("deckard-bridge: pick {id} ended: {error}");
                Vec::new()
            }))
        };
        let gave_up = async {
            let _ = gone.recv().await;
            None
        };
        let Some(chosen) = future::or(asked, gave_up).await else {
            if matches!(ask, Ask::Open | Ask::Save) {
                let request = (PORTAL, &*self.request_path(&id)?, REQUEST);
                call(&self.bus, request, "Close", &(), CALL_TIMEOUT).await?;
            }
            return Ok(());
        };
        if let Some(copy) = self.conclude(&id, ask, chosen) {
            let (xfer, copy_id) = (self.xfer.clone(), id.clone());
            let files = blocking::unblock(move || copy_in(&xfer, &copy_id, &copy)).await;
            self.picked(&id, files);
        }
        Ok(())
    }

    /// Ends the request: answers a save, or hands back the files to copy.
    fn conclude(&self, id: &str, ask: Ask, chosen: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
        let mut state = lock(&self.state);
        if state.pending.remove(id).map(|(asked, _)| asked) != Some(ask) {
            return None;
        }
        if ask != Ask::Save {
            return Some(chosen);
        }
        let Some(target) = chosen.into_iter().next() else {
            drop(state);
            self.picked(id, Vec::new());
            return None;
        };
        let name = safe_name(&target.to_string_lossy());
        state.saved.retain(|(saved, _)| saved != id);
        if state.saved.len() >= MAX_SAVED {
            state.saved.pop_front();
        }
        state.saved.push_back((id.to_owned(), target));
        drop(state);
        self.picked(id, vec![json!({"name": name})]);
        None
    }

    fn request_path(&self, id: &str) -> zbus::Result<String> {
        let unique = self.bus.unique_name();
        let unique = unique.ok_or_else(|| zbus::Error::Failure("no unique name".into()))?;
        let sender = unique.trim_start_matches(':').replace('.', "_");
        let requests = "/org/freedesktop/portal/desktop/request";
        Ok(format!("{requests}/{sender}/deckard_{id}"))
    }

    /// Asks the FileChooser portal; the chosen local files.
    async fn choose(&self, id: &str, ask: Ask, options: Options) -> zbus::Result<Vec<PathBuf>> {
        let path = self.request_path(id)?;
        let request = zbus::Proxy::new(&self.bus, PORTAL, path, REQUEST).await?;
        let mut responses = request.receive_signal("Response").await?;
        let (method, title) = match ask {
            Ask::Save => ("SaveFile", "Save"),
            _ => ("OpenFile", "Open"),
        };
        let chooser = "org.freedesktop.portal.FileChooser";
        let chooser = (PORTAL, "/org/freedesktop/portal/desktop", chooser);
        let body = ("", title, options);
        call(&self.bus, chooser, method, &body, CALL_TIMEOUT).await?;
        let Some(response) = responses.next().await else {
            return Ok(Vec::new());
        };
        let (code, results): (u32, HashMap<String, OwnedValue>) = response.body().deserialize()?;
        let uris = match results.get("uris") {
            Some(uris) if code == 0 => Vec::<String>::try_from(uris.try_clone()?)?,
            _ => Vec::new(),
        };
        Ok(uris.iter().filter_map(|uri| file_path(uri)).collect())
    }

    /// Marathon Camera's camera.capture; only a file in its folder, of the
    /// kind asked for, goes to Android.
    async fn capture(&self, ask: Ask) -> zbus::Result<Vec<PathBuf>> {
        let video = ask == Ask::Video;
        let mode = json!({"mode": if video { "video" } else { "photo" }}).to_string();
        let body = ("camera.capture", mode, HashMap::<&str, Variant<'_>>::new());
        let camera = ("org.marathonos.camera", KEEL_PATH, KEEL);
        let reply = call(&self.bus, camera, "Invoke", &body, CAPTURE_TIMEOUT).await?;
        let reply = serde_json::from_str(&reply.body().deserialize::<String>()?);
        let path = PathBuf::from(field(&reply.unwrap_or_default(), "path"));
        let kinds = match video {
            true => ["mp4", "mkv", "webm", "mov"].as_slice(),
            false => ["jpg", "jpeg", "png"].as_slice(),
        };
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let file = path.canonicalize().ok().filter(|file| file.is_file());
        let folder = self.camera.canonicalize().ok();
        let parent = file.as_deref().and_then(Path::parent);
        let inside = folder.is_some() && parent == folder.as_deref();
        if !inside || !kinds.contains(&extension.to_ascii_lowercase().as_str()) {
            eprintln!("deckard-bridge: capture refused {}", path.display());
            return Ok(Vec::new());
        }
        Ok(file.into_iter().collect())
    }

    /// A file saved through DeckardBridge's Handoff provider was closed: it
    /// goes to where the person chose.
    pub fn pick_done(&self, id: &str) {
        let state = lock(&self.state);
        let saved = state.saved.iter().find(|(saved, _)| saved == id).cloned();
        drop(state);
        let Some((id, target)) = saved else {
            return;
        };
        let xfer = self.xfer.clone();
        self.spawn("deckard-save", async move {
            let shown = target.display().to_string();
            match blocking::unblock(move || save(&xfer, &id, &target)).await {
                Ok(()) => eprintln!("deckard-bridge: saved {shown}"),
                Err(error) => eprintln!("deckard-bridge: save of {shown} failed: {error}"),
            }
        });
    }
}

fn portal_options(m: &Map<String, Value>, id: &str, ask: Ask) -> Options {
    let mut options = Options::new();
    let mut put = |key, value: Variant<'_>| {
        if let Ok(value) = OwnedValue::try_from(value) {
            options.insert(key, value);
        }
    };
    put("handle_token", format!("deckard_{id}").into());
    if ask == Ask::Save {
        let name = safe_name(&validate::clean_text(field(m, "name"), 255));
        put("current_name", name.into());
    }
    if ask != Ask::Open {
        return options;
    }
    let multiple = m.get("multiple").and_then(Value::as_bool);
    put("multiple", multiple.unwrap_or(false).into());
    let types = list(m, "mime");
    let mut any = types.is_empty();
    let mut rules = Vec::new();
    for value in types {
        let mime = value.as_str().unwrap_or("").to_lowercase();
        any |= mime == "*/*";
        if MIME_RULE.is_match(&mime) && rules.len() < 16 {
            rules.push((1_u32, mime));
        }
    }
    if !any && !rules.is_empty() {
        put("filters", vec![("Files".to_owned(), rules)].into());
    }
    options
}
