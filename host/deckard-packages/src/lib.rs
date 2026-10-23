//! Inactive Rust implementation of Deckard's Packages D-Bus interface.
//!
//! The daemon must own the bus name and mount this interface at the existing
//! object path. It must supply an authenticated caller policy and connected
//! Android bridge sender; this crate never claims the production bus name.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use async_channel::Sender;
use async_io::Timer;
use deckard_core::ids::PackageName;
use deckard_core::transport::MAX_FRAME_SIZE;
use deckard_core::validate::{base64_png, clean_text};
use futures_lite::future;
use regex::Regex;
use serde_json::{Map, Value, json};
use zbus::blocking::Connection;
use zbus::message::Header;
use zbus::zvariant::{OwnedValue, Str, Value as Variant};
use zbus::{fdo, interface};

pub const BUS_NAME: &str = "org.marathonos.Deckard";
pub const OBJECT_PATH: &str = "/org/marathonos/Deckard";
pub const INTERFACE: &str = "org.marathonos.Deckard1.Packages";
pub const ANDROID_INSTALL_DIR: &str = "/data/deckard/install/";
pub const MAX_APK_BYTES: u64 = 1 << 30;
pub const MAX_STAGED_BYTES: u64 = 2 << 30;
pub const MAX_PENDING: usize = 64;
const MAX_STALE_ENTRIES: usize = 4096;

const INSPECT_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(180);
const SWEEP_INTERVAL: Duration = Duration::from_millis(100);
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
static RUNTIME_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:waydroid|lineageos|lineage os)\b")
        .unwrap_or_else(|error| panic!("invalid fixed runtime-name pattern: {error}"))
});

pub type VariantMap = HashMap<String, OwnedValue>;
pub type PackageList = Vec<VariantMap>;
pub type BridgeSend = dyn Fn(BridgeRoute, &Value) -> bool + Send + Sync;
pub type MayChange = dyn Fn(String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeRoute {
    User,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Inspect,
    Install,
    Uninstall,
    Device,
    Packages,
}

enum Reply {
    Map(VariantMap),
    List(PackageList),
    Error(fdo::Error),
}

struct Pending {
    kind: Kind,
    route: BridgeRoute,
    staged: Option<Staged>,
    reply: Option<Sender<Reply>>,
    deadline: Instant,
}

struct Inner {
    bus: Connection,
    install_dir: PathBuf,
    bridge: Arc<BridgeSend>,
    may_change: Arc<MayChange>,
    pending: Mutex<HashMap<String, Pending>>,
    staged_bytes: Arc<Mutex<u64>>,
}

struct Staged {
    path: Option<PathBuf>,
    bytes: u64,
    budget: Arc<Mutex<u64>>,
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
        if let Ok(mut used) = self.budget.lock() {
            *used = used.saturating_sub(self.bytes);
        }
    }
}

#[derive(Clone)]
pub struct Packages(Arc<Inner>);

/// Register this interface on the host-owned connection. Its weak reference
/// avoids a Connection -> object server -> Packages -> Connection cycle.
pub struct PackagesInterface(Weak<Inner>);

impl PackagesInterface {
    fn service(&self) -> fdo::Result<Packages> {
        self.0
            .upgrade()
            .map(Packages)
            .ok_or_else(|| fdo::Error::Failed("package service stopped".to_owned()))
    }
}

impl Packages {
    pub fn interface(&self) -> PackagesInterface {
        PackagesInterface(Arc::downgrade(&self.0))
    }

    /// `may_change` receives the D-Bus unique sender name, not a caller-supplied
    /// app ID. The host must resolve its process and Shell app identity and fail
    /// closed; only Settings, Store and the verified store handler may mutate.
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
        let inner = Arc::new(Inner {
            bus,
            install_dir,
            bridge,
            may_change,
            pending: Mutex::new(HashMap::new()),
            staged_bytes: Arc::new(Mutex::new(0)),
        });
        let weak = Arc::downgrade(&inner);
        thread::Builder::new()
            .name("deckard-packages-timeouts".to_owned())
            .spawn(move || sweep_timeouts(weak))?;
        Ok(Self(inner))
    }

    /// Called only for messages from the authenticated Android bridge.
    pub fn handle(
        &self,
        route: BridgeRoute,
        kind: &str,
        message: &Map<String, Value>,
    ) -> zbus::Result<()> {
        if serde_json::to_vec(message).is_ok_and(|bytes| bytes.len() > MAX_FRAME_SIZE) {
            return Ok(());
        }
        if kind == "package_changed" {
            if route != BridgeRoute::User {
                return Ok(());
            }
            let package = field_str(message, "pkg");
            let change = field_str(message, "change");
            if PackageName::try_from(package).is_ok()
                && matches!(change, "installed" | "updated" | "removed")
            {
                self.0.bus.emit_signal(
                    None::<&str>,
                    OBJECT_PATH,
                    INTERFACE,
                    "PackageChanged",
                    &(package, change),
                )?;
            }
            return Ok(());
        }
        let id = field_str(message, "id");
        if id.len() != 16 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(());
        }
        if kind == "install_progress" {
            let pending = self
                .0
                .pending
                .lock()
                .map_err(|_| zbus::Error::Failure("pending lock poisoned".into()))?;
            if pending
                .get(id)
                .is_some_and(|item| item.kind == Kind::Install && item.route == route)
            {
                let percent = integer(message, "percent", 0, 100, 0) as i32;
                drop(pending);
                self.0.bus.emit_signal(
                    None::<&str>,
                    OBJECT_PATH,
                    INTERFACE,
                    "Progress",
                    &(id, percent),
                )?;
            }
            return Ok(());
        }
        let expected = match kind {
            "inspect_result" => Kind::Inspect,
            "device_result" => Kind::Device,
            "packages_result" => Kind::Packages,
            // DeckardSystem deliberately uses the same completion type for removal.
            "install_result" => Kind::Install,
            _ => return Ok(()),
        };
        let pending = {
            let mut active = self
                .0
                .pending
                .lock()
                .map_err(|_| zbus::Error::Failure("pending lock poisoned".into()))?;
            if active.get(id).is_none_or(|item| {
                item.route != route
                    || (item.kind != expected
                        && !(kind == "install_result" && item.kind == Kind::Uninstall))
            }) {
                return Ok(());
            }
            active.remove(id)
        };
        if let Some(pending) = pending {
            self.finish(id, pending, kind, message)?;
        }
        Ok(())
    }

    /// Complete only the requests owned by a disconnected Android bridge.
    pub fn bridge_lost(&self, route: BridgeRoute) -> zbus::Result<()> {
        let pending = {
            let mut active = self
                .0
                .pending
                .lock()
                .map_err(|_| zbus::Error::Failure("pending lock poisoned".into()))?;
            let ids = active
                .iter()
                .filter(|(_, item)| item.route == route)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| active.remove(&id).map(|item| (id, item)))
                .collect::<Vec<_>>()
        };
        for (id, item) in pending {
            self.fail(&id, item, "Android stopped")?;
        }
        Ok(())
    }

    pub fn pending_count(&self) -> usize {
        self.0.pending.lock().map_or(0, |active| active.len())
    }

    fn finish(
        &self,
        id: &str,
        item: Pending,
        kind: &str,
        message: &Map<String, Value>,
    ) -> zbus::Result<()> {
        drop(item.staged);
        match kind {
            "inspect_result" => {
                let map = if boolean(message, "ok") {
                    inspect_info(message)
                } else {
                    failure(&text(message, "message", 512))
                };
                send_reply(item.reply, Reply::Map(map));
            }
            "device_result" => send_reply(item.reply, Reply::Map(device_profile(message))),
            "packages_result" => send_reply(item.reply, Reply::List(package_list(message))),
            "install_result" => {
                let package = valid_package(field_str(message, "pkg")).unwrap_or_default();
                let text = text(message, "message", 512);
                self.0.bus.emit_signal(
                    None::<&str>,
                    OBJECT_PATH,
                    INTERFACE,
                    "Finished",
                    &(id, boolean(message, "ok"), package, text),
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn fail(&self, id: &str, item: Pending, reason: &str) -> zbus::Result<()> {
        drop(item.staged);
        match item.kind {
            Kind::Inspect => send_reply(item.reply, Reply::Map(failure(reason))),
            Kind::Device | Kind::Packages => send_reply(
                item.reply,
                Reply::Error(fdo::Error::Failed(reason.to_owned())),
            ),
            Kind::Install | Kind::Uninstall => {
                self.0.bus.emit_signal(
                    None::<&str>,
                    OBJECT_PATH,
                    INTERFACE,
                    "Finished",
                    &(id, false, "", reason),
                )?;
            }
        }
        Ok(())
    }

    fn start(
        &self,
        kind: Kind,
        route: BridgeRoute,
        message: Value,
        staged: Option<Staged>,
        reply: Option<Sender<Reply>>,
    ) -> fdo::Result<String> {
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| fdo::Error::Failed("missing request id".to_owned()))?
            .to_owned();
        {
            let mut active = self
                .0
                .pending
                .lock()
                .map_err(|_| fdo::Error::Failed("pending lock poisoned".to_owned()))?;
            if active.len() >= MAX_PENDING || active.contains_key(&id) {
                return Err(fdo::Error::LimitsExceeded(
                    "too many pending package requests".to_owned(),
                ));
            }
            active.insert(
                id.clone(),
                Pending {
                    kind,
                    route,
                    staged,
                    reply,
                    deadline: Instant::now()
                        + if kind == Kind::Install {
                            INSTALL_TIMEOUT
                        } else {
                            INSPECT_TIMEOUT
                        },
                },
            );
        }
        if !(self.0.bridge)(route, &message) {
            if let Ok(mut active) = self.0.pending.lock() {
                active.remove(&id);
            }
            return Err(fdo::Error::Failed("Android isn't running".to_owned()));
        }
        Ok(id)
    }

    fn stage(&self, path: &str) -> fdo::Result<(String, Staged)> {
        if path.len() > 4096 {
            return Err(fdo::Error::Failed("not a file".to_owned()));
        }
        let mut input =
            File::open(path).map_err(|_| fdo::Error::Failed("not a file".to_owned()))?;
        let metadata = input
            .metadata()
            .map_err(|_| fdo::Error::Failed("not a file".to_owned()))?;
        if !metadata.is_file() || !(5..=MAX_APK_BYTES).contains(&metadata.len()) {
            return Err(fdo::Error::Failed("not a file".to_owned()));
        }
        let mut magic = [0_u8; 4];
        input
            .read_exact(&mut magic)
            .map_err(|_| fdo::Error::Failed("not an Android app".to_owned()))?;
        if magic != *b"PK\x03\x04" {
            return Err(fdo::Error::Failed("not an Android app".to_owned()));
        }
        input
            .seek(SeekFrom::Start(0))
            .map_err(|_| fdo::Error::Failed("cannot copy the app into Android".to_owned()))?;
        {
            let mut used = self
                .0
                .staged_bytes
                .lock()
                .map_err(|_| fdo::Error::Failed("staging budget unavailable".to_owned()))?;
            if *used > MAX_STAGED_BYTES - metadata.len() {
                return Err(fdo::Error::LimitsExceeded(
                    "too many staged Android apps".to_owned(),
                ));
            }
            *used += metadata.len();
        }
        let mut reservation = Staged {
            path: None,
            bytes: metadata.len(),
            budget: Arc::clone(&self.0.staged_bytes),
        };
        for _ in 0..4 {
            let name = format!(
                "{}.apk",
                random_id().map_err(|_| fdo::Error::Failed("cannot stage the app".to_owned()))?
            );
            let staged = self.0.install_dir.join(&name);
            let output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .open(&staged);
            let Ok(mut output) = output else {
                continue;
            };
            reservation.path = Some(staged);
            let copied = io::copy(
                &mut Read::by_ref(&mut input).take(metadata.len() + 1),
                &mut output,
            );
            if copied.is_ok_and(|count| count == metadata.len()) && output.flush().is_ok() {
                return Ok((name, reservation));
            }
            return Err(fdo::Error::Failed(
                "cannot copy the app into Android".to_owned(),
            ));
        }
        Err(fdo::Error::Failed("cannot stage the app".to_owned()))
    }

    async fn allowed(&self, header: Header<'_>) -> fdo::Result<()> {
        let sender = header
            .sender()
            .map(|sender| sender.as_str().to_owned())
            .unwrap_or_default();
        let allowed = if sender.is_empty() {
            false
        } else {
            future::or((self.0.may_change)(sender), async {
                Timer::after(AUTH_TIMEOUT).await;
                false
            })
            .await
        };
        if allowed {
            Ok(())
        } else {
            Err(fdo::Error::AccessDenied(
                "Not allowed for this caller".to_owned(),
            ))
        }
    }

    async fn query_map(
        &self,
        kind: Kind,
        route: BridgeRoute,
        message: Value,
        staged: Option<Staged>,
    ) -> fdo::Result<VariantMap> {
        let (tx, rx) = async_channel::bounded(1);
        self.start(kind, route, message, staged, Some(tx))?;
        match rx.recv().await {
            Ok(Reply::Map(map)) => Ok(map),
            Ok(Reply::Error(error)) => Err(error),
            _ => Err(fdo::Error::Failed("package request interrupted".to_owned())),
        }
    }

    async fn query_list(&self, message: Value) -> fdo::Result<PackageList> {
        let (tx, rx) = async_channel::bounded(1);
        self.start(Kind::Packages, BridgeRoute::User, message, None, Some(tx))?;
        match rx.recv().await {
            Ok(Reply::List(list)) => Ok(list),
            Ok(Reply::Error(error)) => Err(error),
            _ => Err(fdo::Error::Failed("package request interrupted".to_owned())),
        }
    }
}

#[interface(name = "org.marathonos.Deckard1.Packages")]
impl PackagesInterface {
    #[zbus(name = "InspectPackage")]
    async fn inspect_package(&self, path: &str) -> fdo::Result<VariantMap> {
        let service = self.service()?;
        let (name, staged) = match service.stage(path) {
            Ok(staged) => staged,
            Err(error) => return Ok(failure(&fdo_message(error))),
        };
        let id =
            random_id().map_err(|_| fdo::Error::Failed("cannot create request id".to_owned()))?;
        let message =
            json!({"t":"inspect", "id":id, "path":format!("{ANDROID_INSTALL_DIR}{name}")});
        match service
            .query_map(Kind::Inspect, BridgeRoute::User, message, Some(staged))
            .await
        {
            Ok(map) => Ok(map),
            Err(error) => Ok(failure(&fdo_message(error))),
        }
    }

    #[zbus(name = "InstallPackage")]
    async fn install_package(
        &self,
        path: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<String> {
        let service = self.service()?;
        service.allowed(header).await?;
        let (name, staged) = service.stage(path)?;
        let id =
            random_id().map_err(|_| fdo::Error::Failed("cannot create request id".to_owned()))?;
        service.start(
            Kind::Install,
            BridgeRoute::User,
            json!({"t":"install", "id":id, "path":format!("{ANDROID_INSTALL_DIR}{name}")}),
            Some(staged),
            None,
        )
    }

    #[zbus(name = "UninstallPackage")]
    async fn uninstall_package(
        &self,
        package: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<String> {
        let service = self.service()?;
        service.allowed(header).await?;
        let package = PackageName::try_from(package)
            .map_err(|_| fdo::Error::InvalidArgs("bad package name".to_owned()))?;
        let id =
            random_id().map_err(|_| fdo::Error::Failed("cannot create request id".to_owned()))?;
        service.start(
            Kind::Uninstall,
            BridgeRoute::System,
            json!({"t":"uninstall", "id":id, "pkg":package.as_str()}),
            None,
            None,
        )
    }

    #[zbus(name = "DeviceProfile")]
    async fn device_profile(&self) -> fdo::Result<VariantMap> {
        let service = self.service()?;
        let id =
            random_id().map_err(|_| fdo::Error::Failed("cannot create request id".to_owned()))?;
        service
            .query_map(
                Kind::Device,
                BridgeRoute::User,
                json!({"t":"device", "id":id}),
                None,
            )
            .await
    }

    #[zbus(name = "ListPackages")]
    async fn list_packages(&self) -> fdo::Result<PackageList> {
        let service = self.service()?;
        let id =
            random_id().map_err(|_| fdo::Error::Failed("cannot create request id".to_owned()))?;
        service.query_list(json!({"t":"packages", "id":id})).await
    }

    #[zbus(signal, name = "Progress")]
    async fn progress(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        job: &str,
        percent: i32,
    ) -> zbus::Result<()>;
    #[zbus(signal, name = "Finished")]
    async fn finished(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        job: &str,
        ok: bool,
        package: &str,
        message: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal, name = "PackageChanged")]
    async fn package_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        package: &str,
        change: &str,
    ) -> zbus::Result<()>;
}

fn sweep_timeouts(weak: Weak<Inner>) {
    loop {
        thread::sleep(SWEEP_INTERVAL);
        let Some(inner) = weak.upgrade() else {
            break;
        };
        let overdue = if let Ok(mut active) = inner.pending.lock() {
            let now = Instant::now();
            let ids: Vec<String> = active
                .iter()
                .filter(|(_, item)| item.deadline <= now)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| active.remove(&id).map(|item| (id, item)))
                .collect::<Vec<_>>()
        } else {
            break;
        };
        let service = Packages(inner);
        for (id, item) in overdue {
            let _ = service.fail(&id, item, "Android didn't answer");
        }
    }
}

fn send_reply(reply: Option<Sender<Reply>>, value: Reply) {
    if let Some(reply) = reply {
        let _ = reply.try_send(value);
    }
}

fn random_id() -> io::Result<String> {
    let mut bytes = [0_u8; 8];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format!("{:016x}", u64::from_le_bytes(bytes)))
}

fn reap_stale(directory: &Path) -> io::Result<()> {
    let mut entries = fs::read_dir(directory)?;
    for _ in 0..MAX_STALE_ENTRIES {
        let Some(entry) = entries.next() else {
            return Ok(());
        };
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|ext| ext == "apk")
        {
            fs::remove_file(entry.path())?;
        }
    }
    if entries.next().is_some() {
        return Err(io::Error::other(
            "too many entries in Android install staging directory",
        ));
    }
    Ok(())
}

fn field_str<'a>(value: &'a Map<String, Value>, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn boolean(value: &Map<String, Value>, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn integer(value: &Map<String, Value>, key: &str, min: i64, max: i64, fallback: i64) -> i64 {
    let Some(number) = value.get(key).and_then(Value::as_f64) else {
        return fallback;
    };
    if !number.is_finite() {
        return fallback;
    }
    number.clamp(min as f64, max as f64) as i64
}

fn text(value: &Map<String, Value>, key: &str, max_units: usize) -> String {
    clean_text(field_str(value, key), max_units)
}

fn label(value: &Map<String, Value>) -> String {
    let raw = text(value, "label", 128);
    let renamed = RUNTIME_NAME.replace_all(&raw, "Android");
    renamed.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn valid_package(value: &str) -> Option<String> {
    PackageName::try_from(value)
        .ok()
        .map(|package| package.as_str().to_owned())
}

fn names(value: &Map<String, Value>, key: &str, limit: usize) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(4096)
        .filter_map(Value::as_str)
        .filter(|name| name.encode_utf16().count() <= 160 && PackageName::try_from(*name).is_ok())
        .take(limit)
        .map(str::to_owned)
        .collect()
}

fn abis(value: &Map<String, Value>) -> Vec<String> {
    value
        .get("abis")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(1024)
        .filter_map(Value::as_str)
        .filter(|abi| {
            (1..=16).contains(&abi.len())
                && abi.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'
                        || byte == b'-'
                })
        })
        .take(16)
        .map(str::to_owned)
        .collect()
}

fn put<T: Into<OwnedValue>>(map: &mut VariantMap, key: &str, value: T) {
    map.insert(key.to_owned(), value.into());
}
fn put_str(map: &mut VariantMap, key: &str, value: &str) {
    put(map, key, Str::from(value));
}
fn put_array(map: &mut VariantMap, key: &str, value: Vec<String>) {
    if let Ok(value) = OwnedValue::try_from(Variant::from(value)) {
        map.insert(key.to_owned(), value);
    }
}

fn failure(reason: &str) -> VariantMap {
    let mut map = VariantMap::new();
    put(&mut map, "ok", false);
    put_str(&mut map, "message", reason);
    map
}

fn fdo_message(error: fdo::Error) -> String {
    match error {
        fdo::Error::Failed(message) | fdo::Error::LimitsExceeded(message) => message,
        other => other.to_string(),
    }
}

fn device_profile(message: &Map<String, Value>) -> VariantMap {
    let mut map = VariantMap::new();
    put_array(&mut map, "features", names(message, "features", 64));
    put_array(&mut map, "libraries", names(message, "libraries", 64));
    put_array(&mut map, "abis", abis(message));
    put(
        &mut map,
        "sdk",
        integer(message, "sdk", i32::MIN as i64, i32::MAX as i64, 0) as i32,
    );
    put(&mut map, "gms", boolean(message, "gms"));
    map
}

fn package_list(message: &Map<String, Value>) -> PackageList {
    let mut list = Vec::new();
    for entry in message
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(4096)
    {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(package) = valid_package(field_str(entry, "pkg")) else {
            continue;
        };
        let mut map = VariantMap::new();
        put_str(&mut map, "pkg", &package);
        put_str(&mut map, "label", &label(entry));
        put_str(&mut map, "versionName", &text(entry, "versionName", 64));
        put(
            &mut map,
            "versionCode",
            integer(entry, "versionCode", i64::MIN, i64::MAX, 0),
        );
        put(&mut map, "system", boolean(entry, "system"));
        put(
            &mut map,
            "launchable",
            entry
                .get("launchable")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        );
        list.push(map);
        if list.len() == 2000 {
            break;
        }
    }
    list
}

fn inspect_info(message: &Map<String, Value>) -> VariantMap {
    let mut map = VariantMap::new();
    put(&mut map, "ok", true);
    put_str(
        &mut map,
        "pkg",
        &valid_package(field_str(message, "pkg")).unwrap_or_default(),
    );
    put_str(&mut map, "label", &label(message));
    put_str(&mut map, "versionName", &text(message, "versionName", 64));
    put(
        &mut map,
        "versionCode",
        integer(message, "versionCode", i64::MIN, i64::MAX, 0),
    );
    put(
        &mut map,
        "minSdk",
        integer(message, "minSdk", i32::MIN as i64, i32::MAX as i64, 0) as i32,
    );
    put(
        &mut map,
        "targetSdk",
        integer(message, "targetSdk", i32::MIN as i64, i32::MAX as i64, 0) as i32,
    );
    put_array(&mut map, "permissions", names(message, "permissions", 200));
    let icon = base64_png(field_str(message, "icon"), 256 * 1024).unwrap_or_default();
    if let Ok(icon) = OwnedValue::try_from(Variant::from(icon)) {
        map.insert("icon".to_owned(), icon);
    }
    put_array(&mut map, "features", names(message, "features", 64));
    put_array(&mut map, "libraries", names(message, "libraries", 64));
    put_array(&mut map, "abis", abis(message));
    put(&mut map, "gms", boolean(message, "gms"));
    if message.contains_key("installedVersionCode") {
        put(
            &mut map,
            "installedVersionCode",
            integer(message, "installedVersionCode", i64::MIN, i64::MAX, 0),
        );
        put_str(
            &mut map,
            "installedVersionName",
            &text(message, "installedVersionName", 64),
        );
    }
    map
}
