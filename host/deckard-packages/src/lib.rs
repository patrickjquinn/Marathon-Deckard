//! `org.marathonos.Deckard1.Packages`: installs, removals and package queries
//! relayed to Android. The host owns the bus name and the bridge sockets.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_channel::Sender;
use async_io::Timer;
use deckard_core::{ids, json, sync, validate};
use deckard_host_tools::bus;
use futures_lite::future;
use serde_json::{Map, Value, json};
use zbus::blocking::Connection;
use zbus::export::serde::Serialize;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{DynamicType, OwnedValue, Value as Variant};
use zbus::{DBusError, Task, fdo, interface};

/// Deckard's bus name, owned by the bridge.
pub const BUS_NAME: &str = "org.marathonos.Deckard";
/// Where Deckard's interfaces are served.
pub const OBJECT_PATH: &str = "/org/marathonos/Deckard";
/// The package interface.
pub const INTERFACE: &str = "org.marathonos.Deckard1.Packages";
/// The most requests waiting for Android at once.
pub const MAX_PENDING: usize = 64;
const ANDROID_INSTALL_DIR: &str = "/data/deckard/install/";
const MAX_APK_BYTES: u64 = 1 << 30;
const MAX_STAGED_BYTES: u64 = 2 << 30;
const MAX_STALE_ENTRIES: usize = 4096;
/// The most entries read from one Android list.
const MAX_READ: usize = 4096;
const INSPECT_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(180);
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

/// A D-Bus `a{sv}`.
pub type VariantMap = HashMap<String, OwnedValue>;
/// Queues a message to the bridge on a route; false when it is not connected.
pub type BridgeSend = dyn Fn(BridgeRoute, &Value) -> bool + Send + Sync;
/// Whether a caller, by unique bus name, may install or remove apps.
pub type MayChange = dyn Fn(String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync;
type Answer = fdo::Result<Map<String, Value>>;

/// The authenticated Android socket a message travels on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeRoute {
    User,
    System,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Inspect,
    Install,
    Uninstall,
    Device,
    Packages,
}

impl Kind {
    /// The message type that answers the request.
    fn result(self) -> &'static str {
        match self {
            Self::Inspect => "inspect_result",
            Self::Install | Self::Uninstall => "install_result",
            Self::Device => "device_result",
            Self::Packages => "packages_result",
        }
    }
}

/// An `a{sv}` of `entries`; a value that cannot be owned is left out.
#[must_use]
pub fn dict<'a>(entries: impl IntoIterator<Item = (&'a str, Variant<'a>)>) -> VariantMap {
    let mut map = VariantMap::new();
    for (key, value) in entries {
        if let Ok(value) = OwnedValue::try_from(value) {
            map.insert(key.to_owned(), value);
        }
    }
    map
}

/// A staged APK and its share of the staging budget, both given back on drop.
struct Staged {
    path: Option<PathBuf>,
    bytes: u64,
    budget: Arc<AtomicU64>,
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
        self.budget.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

struct Pending {
    kind: Kind,
    route: BridgeRoute,
    staged: Option<Staged>,
    /// None for a job, which ends with `Finished`.
    reply: Option<Sender<Answer>>,
    timeout: Option<Task<()>>,
}

struct Inner {
    bus: Connection,
    install_dir: PathBuf,
    bridge: Arc<BridgeSend>,
    may_change: Arc<MayChange>,
    pending: Mutex<HashMap<String, Pending>>,
    staged: Arc<AtomicU64>,
}

/// The package service, shared with the bridge threads.
#[derive(Clone)]
pub struct Packages(Arc<Inner>);

/// The served interface; weak, so the bus does not keep the service alive.
pub struct PackagesInterface(Weak<Inner>);

fn failed(why: &str) -> fdo::Error {
    fdo::Error::Failed(why.to_owned())
}

impl Packages {
    /// Makes `install_dir` and removes the APKs a previous run left there.
    /// `may_change` gets the caller's unique bus name and fails closed.
    ///
    /// # Errors
    ///
    /// When the directory cannot be made or cleaned.
    pub fn new(
        bus: Connection,
        install_dir: impl Into<PathBuf>,
        bridge: Arc<BridgeSend>,
        may_change: Arc<MayChange>,
    ) -> io::Result<Self> {
        let install_dir = install_dir.into();
        fs::create_dir_all(&install_dir)?;
        fs::set_permissions(&install_dir, fs::Permissions::from_mode(0o755))?;
        reap_stale(&install_dir)?;
        Ok(Self(Arc::new(Inner {
            bus,
            install_dir,
            bridge,
            may_change,
            pending: Mutex::default(),
            staged: Arc::default(),
        })))
    }

    /// The interface to serve at [`OBJECT_PATH`].
    #[must_use]
    pub fn interface(&self) -> PackagesInterface {
        PackagesInterface(Arc::downgrade(&self.0))
    }

    fn emit<B: Serialize + DynamicType>(&self, member: &str, body: &B) -> zbus::Result<()> {
        let bus = &self.0.bus;
        bus.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, member, body)
    }

    /// A message of type `kind` from the authenticated bridge on `route`.
    ///
    /// # Errors
    ///
    /// When a signal cannot be sent.
    pub fn handle(
        &self,
        route: BridgeRoute,
        kind: &str,
        message: &Map<String, Value>,
    ) -> zbus::Result<()> {
        let id = json::string(message, "id");
        if kind == "package_changed" {
            let pkg = json::string(message, "pkg");
            let change = json::string(message, "change");
            let known = matches!(change, "installed" | "updated" | "removed");
            if route == BridgeRoute::User && known && ids::is_package_name(pkg) {
                return self.emit("PackageChanged", &(pkg, change));
            }
            return Ok(());
        }
        let mut pending = sync::lock(&self.0.pending);
        if kind == "install_progress" {
            let installing = |item: &Pending| item.kind == Kind::Install && item.route == route;
            if pending.get(id).is_some_and(installing) {
                drop(pending);
                let percent = (number(message, "percent") as i32).clamp(0, 100);
                return self.emit("Progress", &(id, percent));
            }
            return Ok(());
        }
        let answers = |item: &Pending| item.route == route && item.kind.result() == kind;
        if !pending.get(id).is_some_and(answers) {
            return Ok(());
        }
        let Some(Pending { staged, reply, .. }) = pending.remove(id) else {
            return Ok(());
        };
        drop((pending, staged));
        if let Some(reply) = reply {
            let _ = reply.try_send(Ok(message.clone()));
            return Ok(());
        }
        let pkg = Some(json::string(message, "pkg")).filter(|pkg| ids::is_package_name(pkg));
        let (ok, why) = (
            json::boolean(message, "ok"),
            json::text(message, "message", 512),
        );
        self.emit("Finished", &(id, ok, pkg.unwrap_or(""), why))
    }

    /// Fails the requests that went to the bridge on `route`.
    ///
    /// # Errors
    ///
    /// When a `Finished` signal cannot be sent.
    pub fn bridge_lost(&self, route: BridgeRoute) -> zbus::Result<()> {
        let lost: Vec<_> = sync::lock(&self.0.pending)
            .extract_if(|_, item| item.route == route)
            .collect();
        for (id, item) in lost {
            if let Some(finished) = fail(&id, item, "Android stopped") {
                self.emit("Finished", &finished)?;
            }
        }
        Ok(())
    }

    /// The requests still waiting for Android.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        sync::lock(&self.0.pending).len()
    }

    /// Sends `message` as a new request and fails it after its timeout; the
    /// timer ends with the request.
    fn start(
        &self,
        kind: Kind,
        route: BridgeRoute,
        mut message: Value,
        staged: Option<Staged>,
        reply: Option<Sender<Answer>>,
    ) -> fdo::Result<String> {
        let id = random_id()?;
        message["id"] = json!(id);
        let (inner, timed) = (Arc::downgrade(&self.0), id.clone());
        let limit = match kind {
            Kind::Install => INSTALL_TIMEOUT,
            _ => INSPECT_TIMEOUT,
        };
        let timeout = async move {
            Timer::after(limit).await;
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let item = sync::lock(&inner.pending).remove(&timed);
            let Some(mut item) = item else {
                return;
            };
            // Its own handle: dropped before the await, it would cancel this task.
            let _this = item.timeout.take();
            if let Some(finished) = fail(&timed, item, "Android didn't answer") {
                let bus = inner.bus.inner();
                let signal =
                    bus.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, "Finished", &finished);
                let _ = signal.await;
            }
        };
        let executor = self.0.bus.inner().executor();
        let item = Pending {
            kind,
            route,
            staged,
            reply,
            timeout: Some(executor.spawn(timeout, "deckard-package-timeout")),
        };
        {
            let mut pending = sync::lock(&self.0.pending);
            if pending.len() >= MAX_PENDING {
                let why = "too many pending package requests";
                return Err(fdo::Error::LimitsExceeded(why.to_owned()));
            }
            pending.insert(id.clone(), item);
        }
        if !(self.0.bridge)(route, &message) {
            sync::lock(&self.0.pending).remove(&id);
            return Err(failed("Android isn't running"));
        }
        Ok(id)
    }

    async fn query(&self, kind: Kind, message: Value, staged: Option<Staged>) -> Answer {
        let (reply, answer) = async_channel::bounded(1);
        self.start(kind, BridgeRoute::User, message, staged, Some(reply))?;
        let interrupted = |_| Err(failed("package request interrupted"));
        answer.recv().await.unwrap_or_else(interrupted)
    }

    /// Copies the APK at `path` into the install directory, off the bus's
    /// executor; its path in Android.
    async fn stage(&self, path: &str) -> fdo::Result<(String, Staged)> {
        let (service, path) = (self.clone(), path.to_owned());
        bus::unblock(move || service.copy(&path)).await
    }

    fn copy(&self, path: &str) -> fdo::Result<(String, Staged)> {
        let mut input = File::open(path).map_err(|_| failed("not a file"))?;
        let metadata = input.metadata().ok().filter(fs::Metadata::is_file);
        let bytes = metadata.map_or(0, |metadata| metadata.len());
        if !(5..=MAX_APK_BYTES).contains(&bytes) {
            return Err(failed("not a file"));
        }
        let mut magic = [0_u8; 4];
        if input.read_exact(&mut magic).is_err() || magic != *b"PK\x03\x04" {
            return Err(failed("not an Android app"));
        }
        let copy_failed = || failed("cannot copy the app into Android");
        input.rewind().map_err(|_| copy_failed())?;
        let fits = |used| (used <= MAX_STAGED_BYTES - bytes).then_some(used + bytes);
        let (budget, order) = (Arc::clone(&self.0.staged), Ordering::SeqCst);
        if budget.fetch_update(order, order, fits).is_err() {
            let why = "too many staged Android apps";
            return Err(fdo::Error::LimitsExceeded(why.to_owned()));
        }
        let mut staged = Staged {
            path: None,
            bytes,
            budget,
        };
        let name = format!("{}.apk", random_id()?);
        let target = self.0.install_dir.join(&name);
        let mut options = OpenOptions::new();
        let output = options
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&target);
        let mut output = output.map_err(|_| failed("cannot stage the app"))?;
        staged.path = Some(target);
        let copied = io::copy(&mut input.take(bytes + 1), &mut output);
        if copied.ok() != Some(bytes) {
            return Err(copy_failed());
        }
        Ok((format!("{ANDROID_INSTALL_DIR}{name}"), staged))
    }

    async fn allowed(&self, header: &Header<'_>) -> fdo::Result<()> {
        let sender = header.sender().map(ToString::to_string);
        let timeout = async {
            Timer::after(AUTH_TIMEOUT).await;
            false
        };
        if let Some(sender) = sender
            && future::or((self.0.may_change)(sender), timeout).await
        {
            return Ok(());
        }
        let why = "Not allowed for this caller".to_owned();
        Err(fdo::Error::AccessDenied(why))
    }
}

/// Ends `item` with `why`: a query hears it; a job's `Finished` is returned.
fn fail<'a>(id: &'a str, item: Pending, why: &'a str) -> Option<(&'a str, bool, &'a str, &'a str)> {
    drop(item.staged);
    let Some(reply) = item.reply else {
        return Some((id, false, "", why));
    };
    let _ = reply.try_send(Err(failed(why)));
    None
}

fn random_id() -> fdo::Result<String> {
    let random = getrandom::u64().map_err(|_| failed("no random id"))?;
    Ok(format!("{random:016x}"))
}

impl PackagesInterface {
    fn service(&self) -> fdo::Result<Packages> {
        let service = self.0.upgrade().map(Packages);
        service.ok_or_else(|| failed("package service stopped"))
    }
}

#[interface(name = "org.marathonos.Deckard1.Packages")]
impl PackagesInterface {
    async fn inspect_package(&self, path: &str) -> fdo::Result<VariantMap> {
        let service = self.service()?;
        let asked = async {
            let (path, staged) = service.stage(path).await?;
            let message = json!({"t": "inspect", "path": path});
            service.query(Kind::Inspect, message, Some(staged)).await
        };
        Ok(match asked.await {
            Ok(reply) if json::boolean(&reply, "ok") => inspect(&reply),
            Ok(reply) => failure(json::text(&reply, "message", 512)),
            Err(error) => failure(error.description().unwrap_or("").to_owned()),
        })
    }

    async fn install_package(
        &self,
        path: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<String> {
        let service = self.service()?;
        service.allowed(&header).await?;
        let (path, staged) = service.stage(path).await?;
        let (message, route) = (json!({"t": "install", "path": path}), BridgeRoute::User);
        service.start(Kind::Install, route, message, Some(staged), None)
    }

    async fn uninstall_package(
        &self,
        package: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<String> {
        let service = self.service()?;
        service.allowed(&header).await?;
        if !ids::is_package_name(package) {
            return Err(fdo::Error::InvalidArgs("bad package name".to_owned()));
        }
        let message = json!({"t": "uninstall", "pkg": package});
        service.start(Kind::Uninstall, BridgeRoute::System, message, None, None)
    }

    async fn device_profile(&self) -> fdo::Result<VariantMap> {
        let service = self.service()?;
        let reply = service.query(Kind::Device, json!({"t": "device"}), None);
        let reply = reply.await?;
        let sdk = ("sdk", Variant::from(number(&reply, "sdk") as i32));
        Ok(dict(hardware(&reply).into_iter().chain([sdk])))
    }

    async fn list_packages(&self) -> fdo::Result<Vec<VariantMap>> {
        let service = self.service()?;
        let reply = service.query(Kind::Packages, json!({"t": "packages"}), None);
        Ok(packages(&reply.await?))
    }

    #[zbus(signal)]
    async fn progress(emitter: &SignalEmitter<'_>, job: &str, percent: i32) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn finished(
        emitter: &SignalEmitter<'_>,
        job: &str,
        ok: bool,
        package: &str,
        message: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn package_changed(
        emitter: &SignalEmitter<'_>,
        package: &str,
        change: &str,
    ) -> zbus::Result<()>;
}

fn reap_stale(directory: &Path) -> io::Result<()> {
    let mut entries = fs::read_dir(directory)?;
    let first = entries.by_ref().take(MAX_STALE_ENTRIES);
    for entry in first {
        let entry = entry?;
        let apk = entry.path().extension().is_some_and(|ext| ext == "apk");
        if apk && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    if entries.next().is_some() {
        let why = "too many entries in Android install staging directory";
        return Err(io::Error::other(why));
    }
    Ok(())
}

/// A JSON number, or 0; callers cast it, saturating.
fn number(message: &Map<String, Value>, key: &str) -> f64 {
    message.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The first `limit` strings at `key` that pass `valid`, of the first
/// `64 * limit` entries, never more than `MAX_READ`.
fn names(
    message: &Map<String, Value>,
    key: &str,
    limit: usize,
    valid: fn(&str) -> bool,
) -> Variant<'static> {
    let mut out = Vec::new();
    let read = MAX_READ.min(64 * limit);
    let entries = json::array(message, key).iter().take(read);
    for name in entries {
        let name = name.as_str().unwrap_or("");
        if out.len() < limit && valid(name) {
            out.push(name.to_owned());
        }
    }
    out.into()
}

fn dotted(name: &str) -> bool {
    name.len() <= 160 && ids::is_package_name(name)
}

fn abi(name: &str) -> bool {
    let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b);
    (1..=16).contains(&name.len()) && name.bytes().all(allowed)
}

fn failure(why: String) -> VariantMap {
    dict([("ok", false.into()), ("message", why.into())])
}

fn hardware(message: &Map<String, Value>) -> [(&'static str, Variant<'static>); 4] {
    [
        ("features", names(message, "features", 64, dotted)),
        ("libraries", names(message, "libraries", 64, dotted)),
        ("abis", names(message, "abis", 16, abi)),
        ("gms", json::boolean(message, "gms").into()),
    ]
}

fn identity(message: &Map<String, Value>) -> [(&'static str, Variant<'static>); 4] {
    let pkg = Some(json::string(message, "pkg")).filter(|pkg| ids::is_package_name(pkg));
    let label = validate::label(json::string(message, "label"), "");
    let code = number(message, "versionCode") as i64;
    [
        ("pkg", pkg.unwrap_or("").to_owned().into()),
        ("label", label.into()),
        ("versionName", json::text(message, "versionName", 64).into()),
        ("versionCode", code.into()),
    ]
}

fn packages(message: &Map<String, Value>) -> Vec<VariantMap> {
    let mut list = Vec::new();
    let entries = json::array(message, "packages").iter().take(MAX_READ);
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        if !ids::is_package_name(json::string(entry, "pkg")) {
            continue;
        }
        let launchable = entry.get("launchable").and_then(Value::as_bool);
        let more = [
            ("system", json::boolean(entry, "system").into()),
            ("launchable", launchable.unwrap_or(true).into()),
        ];
        list.push(dict(identity(entry).into_iter().chain(more)));
        if list.len() == 2000 {
            break;
        }
    }
    list
}

fn inspect(message: &Map<String, Value>) -> VariantMap {
    let icon = validate::base64_png(json::string(message, "icon"), 256 * 1024);
    let mut map = dict(identity(message).into_iter().chain(hardware(message)));
    map.extend(dict([
        ("ok", true.into()),
        ("minSdk", (number(message, "minSdk") as i32).into()),
        ("targetSdk", (number(message, "targetSdk") as i32).into()),
        ("permissions", names(message, "permissions", 200, dotted)),
        ("icon", icon.unwrap_or_default().into()),
    ]));
    if message.contains_key("installedVersionCode") {
        let code = number(message, "installedVersionCode") as i64;
        let name = json::text(message, "installedVersionName", 64);
        map.extend(dict([
            ("installedVersionCode", code.into()),
            ("installedVersionName", name.into()),
        ]));
    }
    map
}
