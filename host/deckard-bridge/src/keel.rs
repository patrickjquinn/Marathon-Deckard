//! Each Android app as an `org.shipwright.Keel.Actions` provider for keel-mcp
//! and Pilot (docs/pilot.md): the bus name `waydroid.<pkg>`, on a connection
//! of its own, since keel-mcp describes a name again only when its owner
//! changes.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::future::Future;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::Duration;

use async_executor::{Executor, Task};
use async_io::Timer;
use deckard_core::keel::{self, APP_ID_PREFIX, App, Manifest, Shortcut, string};
use deckard_core::text::truncate;
use deckard_core::validate::clean_text;
use deckard_packages::{BridgeRoute, BridgeSend};
use futures_lite::{StreamExt, future};
use serde_json::{Map, Value, json};
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::zvariant::{DynamicType, OwnedValue};
use zbus::{Connection, Message, connection};

const PATH: &str = "/org/shipwright/Keel/Actions";
const INTERFACE: &str = "org.shipwright.Keel.Actions";
const ERRORS: &str = "org.shipwright.Keel.Actions.Error.";
const SHELL: &str = "org.marathonos.Shell";
const NAVIGATION_PATH: &str = "/org/marathonos/Shell/Navigation";
const NAVIGATION: &str = "org.marathonos.Shell.Navigation1";

const MAX_APPS: usize = 200;
const MAX_PENDING: usize = 64;
const MAX_CHANGES: usize = 512;
const MAX_STATE_BYTES: u64 = 4 << 20;
const MAX_CMDLINE: u64 = 65536;
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(15);
const SHELL_TIMEOUT: Duration = Duration::from_secs(10);
const EXPECT_TIMEOUT: Duration = Duration::from_secs(5);
const SECURE_RETRY: Duration = Duration::from_secs(2);
const WINDOW_POLL: Duration = Duration::from_millis(250);
const WINDOW_POLLS: u32 = 60;
const SCREEN_UNITS: usize = 16000;
const CONTEXT_UNITS: usize = 4000;
const UNKNOWN_SECURE: &str = "Android cannot say whether the screen is protected";
const NO_SHELL: &str = "the shell did not answer";

/// Keel's errors, as keel-mcp maps them.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org")]
pub enum Error {
    #[zbus(error)]
    ZBus(zbus::Error),
    #[zbus(name = "freedesktop.DBus.Error.AccessDenied")]
    AccessDenied(String),
    #[zbus(name = "shipwright.Keel.Actions.Error.UnknownAction")]
    UnknownAction(String),
    #[zbus(name = "shipwright.Keel.Actions.Error.InvalidArguments")]
    InvalidArguments(String),
    #[zbus(name = "shipwright.Keel.Actions.Error.NotAvailable")]
    NotAvailable(String),
    #[zbus(name = "shipwright.Keel.Actions.Error.Failed")]
    Failed(String),
}

impl Error {
    /// The shell's Keel error, by the code after the prefix.
    fn from_shell(name: &str, message: String) -> Self {
        match name.strip_prefix(ERRORS).unwrap_or("Failed") {
            "UnknownAction" => Self::UnknownAction(message),
            "InvalidArguments" => Self::InvalidArguments(message),
            "NotAvailable" => Self::NotAvailable(message),
            _ => Self::Failed(message),
        }
    }
}

fn unavailable(message: impl Into<String>) -> Error {
    Error::NotAvailable(message.into())
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidArguments(message.into())
}

/// What a shell check requires of the app besides an unlocked phone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    Unlocked,
    InFront,
}

/// The caller keel-mcp or Pilot runs for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Host {
    Local,
    /// A paired computer: no screen content.
    External,
}

struct Pending {
    message: Value,
    route: BridgeRoute,
    sent: bool,
    reply: async_channel::Sender<Map<String, Value>>,
}

struct Entry {
    app: App,
    manifest: Manifest,
    connection: Connection,
    _tick: Task<()>,
    /// The last `screen.read`'s.
    snapshot: i64,
}

#[derive(Default)]
struct State {
    apps: HashMap<String, Entry>,
    /// What the shell was told, by package.
    secure: HashMap<String, bool>,
    user: bool,
    system: bool,
    next: i64,
    pending: HashMap<i64, Pending>,
}

impl State {
    fn connected(&self, route: BridgeRoute) -> bool {
        match route {
            BridgeRoute::User => self.user,
            BridgeRoute::System => self.system,
        }
    }
}

enum Change {
    App(App),
    Gone(String),
    Shortcuts(String, Vec<Shortcut>),
}

struct Shared {
    bus: Connection,
    send: Arc<BridgeSend>,
    trusted: Vec<PathBuf>,
    state_path: PathBuf,
    executor: Arc<Executor<'static>>,
    changes: async_channel::Sender<Change>,
    refreshes: async_channel::Sender<String>,
    state: Mutex<State>,
    _stop: async_channel::Sender<()>,
}

/// # Panics
///
/// On a poisoned lock: a thread died mid-update and the catalogue cannot be trusted.
fn locked(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|_| panic!("deckard-bridge: keel state poisoned"))
}

fn ok(reply: &Map<String, Value>) -> bool {
    reply.get("ok").and_then(Value::as_bool) == Some(true)
}

async fn within<T>(limit: Duration, work: impl Future<Output = T>) -> Option<T> {
    future::or(async { Some(work.await) }, async {
        Timer::after(limit).await;
        None
    })
    .await
}

fn keel_op(op: &str, pkg: &str) -> Value {
    json!({"t": "keel", "op": op, "pkg": pkg})
}

async fn navigation<B>(bus: &Connection, method: &str, body: &B) -> zbus::Result<Message>
where
    B: serde::Serialize + DynamicType,
{
    bus.call_method(Some(SHELL), NAVIGATION_PATH, Some(NAVIGATION), method, body)
        .await
}

fn runs_one_of(pid: u32, paths: &[PathBuf]) -> bool {
    let Ok(exe) = fs::metadata(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let same = |path: &PathBuf| {
        fs::metadata(path).is_ok_and(|file| file.dev() == exe.dev() && file.ino() == exe.ino())
    };
    pid > 0 && paths.iter().any(same)
}

fn cmdline(pid: u32) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let file = File::open(format!("/proc/{pid}/cmdline"))?;
    file.take(MAX_CMDLINE).read_to_end(&mut out)?;
    Ok(out)
}

/// The Keel Actions providers of every Android app DeckardBridge reports.
#[derive(Clone)]
pub struct KeelApps(Arc<Shared>);

impl KeelApps {
    /// Serves the catalogue saved at `state_path` before it returns, so the
    /// names are back before Android is up. `bus` reaches the shell;
    /// `trusted` are the executables (keel-mcp, pilotd) whose calls are
    /// served, by file.
    ///
    /// # Errors
    ///
    /// When the Keel thread cannot start.
    pub fn start(
        bus: Connection,
        send: Arc<BridgeSend>,
        trusted: Vec<PathBuf>,
        state_path: PathBuf,
    ) -> io::Result<Self> {
        let executor = Arc::new(Executor::new());
        let (stop, stopped) = async_channel::bounded::<()>(1);
        let (changes, incoming) = async_channel::bounded(MAX_CHANGES);
        let (refreshes, refresh) = async_channel::bounded(MAX_CHANGES);
        let runner = Arc::clone(&executor);
        thread::Builder::new()
            .name("deckard-keel".into())
            .spawn(move || future::block_on(runner.run(stopped.recv())))?;
        let shared = Arc::new(Shared {
            bus,
            send,
            trusted,
            state_path,
            executor,
            changes,
            refreshes,
            state: Mutex::default(),
            _stop: stop,
        });
        future::block_on(shared.load());
        let weak = Arc::downgrade(&shared);
        let catalogue = registry(weak.clone(), incoming);
        shared.executor.spawn(catalogue).detach();
        shared
            .executor
            .spawn(refresher(weak.clone(), refresh))
            .detach();
        let bus = shared.bus.clone();
        let foreground = async move {
            if let Err(error) = follow_foreground(weak, bus).await {
                eprintln!("deckard-bridge: keel: no foreground changes: {error}");
            }
        };
        shared.executor.spawn(foreground).detach();
        Ok(Self(shared))
    }

    /// The DeckardBridge messages `handle` takes.
    pub fn handles(kind: &str) -> bool {
        matches!(
            kind,
            "app_info" | "app_gone" | "app_list" | "launched" | "screen" | "ui_done"
        )
    }

    /// A message from DeckardBridge (`User`) or DeckardSystem (`System`), in
    /// wire order; other types are ignored.
    pub fn handle(&self, route: BridgeRoute, message: &Map<String, Value>) {
        let kind = string(message, "t");
        let change = match (route, kind) {
            (BridgeRoute::User, "app_info") => match keel::app(message) {
                Some(app) => Change::App(app),
                None => return eprintln!("deckard-bridge: keel: app_info refused: bad package"),
            },
            (BridgeRoute::User, "app_gone") => Change::Gone(string(message, "pkg").to_owned()),
            (BridgeRoute::User, "app_list") => {
                let listed = keel::array(message, "pkgs").iter();
                let listed: HashSet<&str> = listed.filter_map(Value::as_str).collect();
                let known: Vec<String> = locked(&self.0.state).apps.keys().cloned().collect();
                let gone = known
                    .into_iter()
                    .filter(|pkg| !listed.contains(pkg.as_str()));
                for pkg in gone {
                    self.0.change(Change::Gone(pkg));
                }
                return;
            }
            (BridgeRoute::System, "keel_changed") if self.0.known(string(message, "pkg")) => {
                return self.0.refresh_shortcuts(string(message, "pkg"));
            }
            (BridgeRoute::User, _) | (BridgeRoute::System, "keel_done") => {
                return self.0.complete(route, message);
            }
            _ => return,
        };
        self.0.change(change);
    }

    /// DeckardBridge or DeckardSystem connected or went away. When
    /// DeckardSystem connects every app's shortcuts are asked for.
    pub fn set_connected(&self, route: BridgeRoute, connected: bool) {
        self.0.connected_changed(route, connected);
        if route == BridgeRoute::System && connected {
            let known: Vec<String> = locked(&self.0.state).apps.keys().cloned().collect();
            for pkg in known {
                self.0.refresh_shortcuts(&pkg);
            }
        }
    }
}

async fn refresher(shared: Weak<Shared>, pkgs: async_channel::Receiver<String>) {
    while let Ok(pkg) = pkgs.recv().await {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let reply = shared
            .request(BridgeRoute::System, keel_op("shortcuts", &pkg))
            .await;
        if let Some(reply) = reply.ok().filter(ok) {
            let shortcuts = keel::shortcuts(keel::array(&reply, "shortcuts"));
            shared.change(Change::Shortcuts(pkg, shortcuts));
        }
    }
}

/// Applies catalogue changes one at a time.
async fn registry(shared: Weak<Shared>, changes: async_channel::Receiver<Change>) {
    while let Ok(change) = changes.recv().await {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        shared.apply(change).await;
    }
}

/// The shell keeps a secure app's windows off its card and out of
/// screenshots; it hears when an Android app comes to the front.
async fn follow_foreground(shared: Weak<Shared>, bus: Connection) -> zbus::Result<()> {
    let navigation = zbus::Proxy::new(&bus, SHELL, NAVIGATION_PATH, NAVIGATION).await?;
    let mut signals = navigation.receive_signal("ForegroundAppChanged").await?;
    while let Some(signal) = signals.next().await {
        let Some(shared) = shared.upgrade() else {
            break;
        };
        let id: String = signal.body().deserialize().unwrap_or_default();
        if let Some(pkg) = id
            .strip_prefix(APP_ID_PREFIX)
            .filter(|pkg| shared.known(pkg))
        {
            let (pkg, task) = (pkg.to_owned(), Arc::clone(&shared));
            let check = async move { task.check_secure(&pkg, 2).await };
            shared.executor.spawn(check).detach();
        }
    }
    Ok(())
}

impl Shared {
    fn change(&self, change: Change) {
        if self.changes.try_send(change).is_err() {
            eprintln!("deckard-bridge: keel: too many app changes; one dropped");
        }
    }

    fn known(&self, pkg: &str) -> bool {
        locked(&self.state).apps.contains_key(pkg)
    }

    fn app(&self, pkg: &str) -> Option<App> {
        let state = locked(&self.state);
        state.apps.get(pkg).map(|entry| entry.app.clone())
    }

    fn label(&self, pkg: &str) -> String {
        self.app(pkg).map(|app| app.label).unwrap_or_default()
    }

    async fn load(self: &Arc<Self>) {
        let mut bytes = Vec::new();
        let read = File::open(&self.state_path)
            .and_then(|file| file.take(MAX_STATE_BYTES).read_to_end(&mut bytes));
        let saved: Vec<Value> = match read {
            Ok(_) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let apps = saved
            .iter()
            .filter_map(Value::as_object)
            .filter_map(keel::app);
        for app in apps {
            self.publish(app).await;
        }
    }

    fn save(&self) {
        let apps: Vec<App> = {
            let state = locked(&self.state);
            state.apps.values().map(|entry| entry.app.clone()).collect()
        };
        let write = || -> io::Result<()> {
            if let Some(parent) = self.state_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let temporary = self.state_path.with_extension("json.tmp");
            let mut file = File::create(&temporary)?;
            file.write_all(&serde_json::to_vec(&apps)?)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.state_path)
        };
        if let Err(error) = write() {
            eprintln!("deckard-bridge: keel: cannot save the catalogue: {error}");
        }
    }

    async fn apply(self: &Arc<Self>, change: Change) {
        match change {
            Change::App(mut app) => {
                if let Some(known) = self.app(&app.pkg) {
                    app.shortcuts = known.shortcuts;
                }
                let pkg = app.pkg.clone();
                self.publish(app).await;
                self.save();
                self.refresh_shortcuts(&pkg);
            }
            Change::Gone(pkg) if self.known(&pkg) => {
                self.unpublish(&pkg).await;
                self.save();
            }
            Change::Shortcuts(pkg, shortcuts) => {
                if let Some(app) = self.app(&pkg) {
                    self.publish(App { shortcuts, ..app }).await;
                    self.save();
                }
            }
            Change::Gone(_) => {}
        }
    }

    /// Serves `app`; a changed manifest moves to a new connection.
    async fn publish(self: &Arc<Self>, app: App) {
        let manifest = keel::manifest(&app);
        if let Some(entry) = locked(&self.state).apps.get_mut(&app.pkg)
            && entry.manifest.json == manifest.json
        {
            entry.app = app;
            return;
        }
        self.unpublish(&app.pkg).await;
        let id = keel::app_id(&app.pkg);
        if locked(&self.state).apps.len() >= MAX_APPS {
            return eprintln!("deckard-bridge: keel: too many apps; not publishing {id}");
        }
        let (connection, tick) = match self.serve(&app.pkg, &id).await {
            Ok(served) => served,
            Err(error) => return eprintln!("deckard-bridge: keel: cannot publish {id}: {error}"),
        };
        let tools = manifest.json["tools"].as_array().map_or(0, Vec::len);
        eprintln!("deckard-bridge: keel: published {id} with {tools} actions");
        let pkg = app.pkg.clone();
        let entry = Entry {
            app,
            manifest,
            connection,
            _tick: tick,
            snapshot: 0,
        };
        locked(&self.state).apps.insert(pkg, entry);
    }

    async fn serve(self: &Arc<Self>, pkg: &str, id: &str) -> zbus::Result<(Connection, Task<()>)> {
        let provider = Provider {
            apps: Arc::downgrade(self),
            pkg: pkg.to_owned(),
        };
        let connection = connection::Builder::session()?
            .internal_executor(false)
            .serve_at(PATH, provider)?
            .build()
            .await?;
        let ticker = connection.clone();
        let tick = self.executor.spawn(async move {
            loop {
                ticker.executor().tick().await;
            }
        });
        let flags = RequestNameFlags::DoNotQueue.into();
        match connection.request_name_with_flags(id, flags).await? {
            RequestNameReply::PrimaryOwner => Ok((connection, tick)),
            _ => Err(zbus::Error::NameTaken),
        }
    }

    /// Releases the name before the next owner asks for it.
    async fn unpublish(&self, pkg: &str) {
        let Some(entry) = locked(&self.state).apps.remove(pkg) else {
            return;
        };
        let id = keel::app_id(pkg);
        if let Err(error) = entry.connection.release_name(id.as_str()).await {
            eprintln!("deckard-bridge: keel: cannot release {id}: {error}");
        }
        eprintln!("deckard-bridge: keel: withdrew {id}");
    }

    fn connected_changed(&self, route: BridgeRoute, connected: bool) {
        let mut state = locked(&self.state);
        match route {
            BridgeRoute::User => state.user = connected,
            BridgeRoute::System => state.system = connected,
        }
        // A failed request drops its sender, which its caller sees.
        state.pending.retain(|_, pending| {
            if pending.route == route && connected && !pending.sent {
                pending.sent = (self.send)(route, &pending.message);
            }
            pending.route != route || connected || !pending.sent
        });
    }

    /// Asks Android; a request made while it is down goes when it connects.
    async fn request(
        &self,
        route: BridgeRoute,
        mut message: Value,
    ) -> Result<Map<String, Value>, String> {
        let (reply, answer) = async_channel::bounded(1);
        let id = {
            let mut state = locked(&self.state);
            if state.pending.len() >= MAX_PENDING {
                return Err("Android is busy".into());
            }
            state.next = state.next % i64::from(i32::MAX) + 1;
            let id = state.next;
            message["req"] = json!(id);
            let sent = state.connected(route) && (self.send)(route, &message);
            let pending = Pending {
                message,
                route,
                sent,
                reply,
            };
            state.pending.insert(id, pending);
            id
        };
        let answered = within(BRIDGE_TIMEOUT, answer.recv()).await;
        let mut state = locked(&self.state);
        state.pending.remove(&id);
        match answered {
            Some(Ok(reply)) => Ok(reply),
            Some(Err(_)) => Err("Android went away".into()),
            None if state.connected(route) => Err("Android did not answer".into()),
            None => Err("Android is not running".into()),
        }
    }

    fn complete(&self, route: BridgeRoute, message: &Map<String, Value>) {
        let Some(id) = message.get("req").and_then(Value::as_i64) else {
            return;
        };
        let mut state = locked(&self.state);
        let waiting = |pending: &Pending| pending.sent && pending.route == route;
        if state.pending.get(&id).is_some_and(waiting)
            && let Some(pending) = state.pending.remove(&id)
        {
            let _ = pending.reply.try_send(message.clone());
        }
    }

    /// Launcher shortcuts come from DeckardSystem, which as the system uid
    /// may read and start every app's; DeckardBridge may not.
    /// Asked one app at a time, so a burst of apps stays within the
    /// request and channel bounds.
    fn refresh_shortcuts(&self, pkg: &str) {
        if locked(&self.state).system && self.refreshes.try_send(pkg.to_owned()).is_err() {
            eprintln!("deckard-bridge: keel: too many shortcut refreshes; one dropped");
        }
    }

    async fn shell(&self, action: &str, args: Value) -> Result<Map<String, Value>, Error> {
        let options: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        let body = (action, args.to_string(), options);
        let destination = Some("org.marathonos.shell");
        let call = self
            .bus
            .call_method(destination, PATH, Some(INTERFACE), "Invoke", &body);
        match within(SHELL_TIMEOUT, call).await {
            Some(Ok(reply)) => {
                let body: String = reply.body().deserialize().unwrap_or_default();
                Ok(serde_json::from_str(&body).unwrap_or_default())
            }
            Some(Err(zbus::Error::MethodError(name, message, _))) => {
                let message = message.filter(|message| !message.is_empty());
                let message = message.unwrap_or_else(|| NO_SHELL.into());
                Err(Error::from_shell(name.as_str(), message))
            }
            _ => Err(Error::Failed(NO_SHELL.into())),
        }
    }

    async fn unlocked(&self, pkg: &str, need: Need) -> Result<(), Error> {
        let id = keel::app_id(pkg);
        let label = self.label(pkg);
        let state = self.shell("shell.state", json!({})).await?;
        if state.get("locked").and_then(Value::as_bool) == Some(true) {
            return Err(unavailable("the phone is locked"));
        }
        let lit = state.get("screenOn").and_then(Value::as_bool) == Some(true);
        if need == Need::InFront && !(lit && string(&state, "foregroundApp") == id) {
            let why = format!("{label} is not on screen: open it first with app.show");
            return Err(unavailable(why));
        }
        Ok(())
    }

    async fn wait_for_window(&self, id: &str) -> Result<(), Error> {
        for left in (0..=WINDOW_POLLS).rev() {
            let running = self.shell("apps.running", json!({})).await?;
            if keel::array(&running, "result")
                .iter()
                .any(|app| app["id"] == id)
            {
                return Ok(());
            }
            if left > 0 {
                Timer::after(WINDOW_POLL).await;
            }
        }
        Err(Error::Failed("the app's window did not appear".into()))
    }

    async fn launched(&self, route: BridgeRoute, launch: Value) -> Result<(), Error> {
        let reply = self.request(route, launch).await.map_err(unavailable)?;
        match ok(&reply) {
            true => Ok(()),
            false => Err(Error::Failed(clean_text(string(&reply, "error"), 256))),
        }
    }

    /// Android starts the activity. A shell that expects the window brings it
    /// forward as a launch; otherwise the bridge waits for the shell to file
    /// it and asks `app.launch` to show it.
    async fn launch_then_show(
        &self,
        pkg: &str,
        launch: Value,
        route: BridgeRoute,
    ) -> Result<(), Error> {
        let id = keel::app_id(pkg);
        if !locked(&self.state).connected(route) {
            self.shell("app.launch", json!({"id": id})).await?;
            return self.launched(route, launch).await;
        }
        // The shell's ExpectAppWindow shows the app's window, and lets a new
        // one take the front for a while, as for a launch.
        let body = (id.as_str(),);
        let expect = navigation(&self.bus, "ExpectAppWindow", &body);
        let reply = within(EXPECT_TIMEOUT, expect).await.and_then(Result::ok);
        let expected = reply.is_some_and(|reply| reply.body().deserialize() == Ok(true));
        self.launched(route, launch).await?;
        if !expected {
            self.wait_for_window(&id).await?;
            self.shell("app.launch", json!({"id": id})).await?;
        }
        Ok(())
    }

    async fn read_screen(&self, pkg: &str, max_units: usize) -> Result<Value, Error> {
        let ask = json!({"t": "screen_read", "pkg": pkg});
        let reply = self.request(BridgeRoute::User, ask).await;
        let reply = reply.map_err(unavailable)?;
        if !ok(&reply) {
            let why = clean_text(string(&reply, "error"), 256);
            let why = Some(why).filter(|why| !why.is_empty());
            return Err(unavailable(
                why.unwrap_or_else(|| "the screen cannot be read".into()),
            ));
        }
        let screen = keel::screen(&reply).filter(|screen| screen.pkg == pkg);
        let mut state = locked(&self.state);
        let (Some(screen), Some(entry)) = (screen, state.apps.get_mut(pkg)) else {
            return Err(Error::Failed("Android sent an unusable screen".into()));
        };
        entry.snapshot = screen.snapshot;
        let rendered = keel::render(&screen, max_units);
        Ok(json!({
            "app": keel::app_id(pkg), "label": entry.app.label, "title": screen.title,
            "snapshot": screen.snapshot, "screen": rendered.text, "truncated": rendered.truncated,
        }))
    }

    /// DeckardSystem vouches that no window of the app is secure.
    async fn not_secure(&self, pkg: &str) -> Result<(), Error> {
        if !locked(&self.state).system {
            return Err(unavailable(UNKNOWN_SECURE));
        }
        let label = self.label(pkg);
        let reply = self
            .request(BridgeRoute::System, keel_op("secure", pkg))
            .await;
        let reply = reply.ok().filter(ok);
        let reply = reply.ok_or_else(|| unavailable(UNKNOWN_SECURE))?;
        let secure = reply.get("secure").and_then(Value::as_bool).unwrap_or(true);
        self.report_secure(pkg, secure);
        if secure {
            return Err(unavailable(format!(
                "{label} protects this screen from being read"
            )));
        }
        Ok(())
    }

    /// A window that is still starting is not in the dump yet, so a failed
    /// check is tried again after a while, `attempts` times in all.
    async fn check_secure(&self, pkg: &str, attempts: u32) {
        for attempt in 0..attempts {
            if attempt > 0 {
                Timer::after(SECURE_RETRY).await;
            }
            if !locked(&self.state).system {
                return;
            }
            let reply = self
                .request(BridgeRoute::System, keel_op("secure", pkg))
                .await;
            if let Some(reply) = reply.ok().filter(ok) {
                let secure = reply.get("secure").and_then(Value::as_bool) == Some(true);
                return self.report_secure(pkg, secure);
            }
        }
    }

    fn report_secure(&self, pkg: &str, secure: bool) {
        {
            let mut state = locked(&self.state);
            if state.secure.get(pkg).copied().unwrap_or(false) == secure {
                return;
            }
            state.secure.insert(pkg.to_owned(), secure);
        }
        let (id, bus) = (keel::app_id(pkg), self.bus.clone());
        let what = if secure { "is secure" } else { "is not secure" };
        eprintln!("deckard-bridge: keel: {id} {what}");
        let tell = async move {
            let body = (id.as_str(), secure);
            if let Err(error) = navigation(&bus, "SetAppWindowSecure", &body).await {
                eprintln!("deckard-bridge: keel: SetAppWindowSecure({id}) failed: {error}");
            }
        };
        self.executor.spawn(tell).detach();
    }

    async fn invoke(
        &self,
        pkg: &str,
        action: &str,
        args: &Map<String, Value>,
        user_initiated: bool,
        host: Host,
    ) -> Result<Value, Error> {
        let unknown = || Error::UnknownAction(format!("no action named {}", truncate(action, 64)));
        let (tool, search, shortcut, snapshot) = {
            let state = locked(&self.state);
            let entry = state
                .apps
                .get(pkg)
                .ok_or_else(|| unavailable("the app is not installed"))?;
            let tools = entry.manifest.json["tools"]
                .as_array()
                .map_or(&[][..], Vec::as_slice);
            let named = |tool: &&Value| tool["_meta"]["org.shipwright.keel/action"] == action;
            let tool = tools.iter().find(named).cloned().ok_or_else(unknown)?;
            let shortcut = entry.manifest.shortcuts.get(action).cloned();
            (tool, entry.app.search.clone(), shortcut, entry.snapshot)
        };
        if let Some(why) = keel::invalid(&tool["inputSchema"], args) {
            return Err(invalid(why));
        }
        eprintln!("deckard-bridge: keel: {} {action}", keel::app_id(pkg));
        let arg = |key: &str| string(args, key);
        let launch = match action {
            "app.show" if arg("link").trim().is_empty() => {
                self.shell("app.launch", json!({"id": keel::app_id(pkg)}))
                    .await?;
                return Ok(json!({}));
            }
            "app.show" => {
                let link = arg("link").trim();
                keel::link_allowed(link).map_err(invalid)?;
                json!({"t": "launch", "pkg": pkg, "kind": "view", "uri": link})
            }
            "share.text" => {
                let mut send =
                    json!({"t": "launch", "pkg": pkg, "kind": "send", "text": arg("text")});
                if !arg("subject").is_empty() {
                    send["subject"] = json!(arg("subject"));
                }
                send
            }
            "app.search" => {
                json!({"t": "launch", "pkg": pkg, "kind": "search", "query": arg("query").trim(), "mode": search})
            }
            "screen.read" if host == Host::External => {
                return Err(unavailable(
                    "screen content is not shared with a paired computer",
                ));
            }
            "screen.read" => {
                self.unlocked(pkg, Need::InFront).await?;
                self.not_secure(pkg).await?;
                return self.read_screen(pkg, SCREEN_UNITS).await;
            }
            "ui.tap" | "ui.type" => {
                if !user_initiated || host == Host::External {
                    let why =
                        "only Pilot acts on an app's screen, for a request the person started";
                    return Err(unavailable(why));
                }
                let node = keel::parse_node(arg("node"))
                    .ok_or_else(|| invalid("node must be an id from screen.read, such as 3.17"))?;
                if node.snapshot != snapshot {
                    return Err(invalid(
                        "that id is from an older screen.read: read the screen again",
                    ));
                }
                let mut act = json!({"t": "ui_act", "pkg": pkg, "snap": node.snapshot, "node": node.index, "act": "click"});
                if action == "ui.type" {
                    act["act"] = json!("set_text");
                    act["text"] = json!(arg("text"));
                }
                self.unlocked(pkg, Need::InFront).await?;
                self.launched(BridgeRoute::User, act).await?;
                return Ok(json!({}));
            }
            _ => {
                let id = shortcut.ok_or_else(unknown)?;
                let mut start = keel_op("start", pkg);
                start["id"] = json!(id);
                self.unlocked(pkg, Need::Unlocked).await?;
                self.launch_then_show(pkg, start, BridgeRoute::System)
                    .await?;
                return Ok(json!({}));
            }
        };
        self.unlocked(pkg, Need::Unlocked).await?;
        self.launch_then_show(pkg, launch, BridgeRoute::User)
            .await?;
        Ok(json!({}))
    }

    async fn context(&self, pkg: &str, host: Host) -> Value {
        if host == Host::External || !self.known(pkg) {
            return json!({});
        }
        let read = async {
            self.unlocked(pkg, Need::InFront).await?;
            self.not_secure(pkg).await?;
            self.read_screen(pkg, CONTEXT_UNITS).await
        };
        let Ok(screen) = read.await else {
            return json!({});
        };
        let label = screen["label"].as_str().unwrap_or("");
        let purpose = format!("What {label} (an Android app) shows on screen now");
        json!({"app": keel::app_id(pkg), "purpose": purpose, "text": screen["screen"]})
    }
}

/// One Android app's `org.shipwright.Keel.Actions`.
struct Provider {
    apps: Weak<Shared>,
    pkg: String,
}

impl Provider {
    fn shared(&self) -> Result<Arc<Shared>, Error> {
        let shared = self.apps.upgrade();
        shared.ok_or_else(|| unavailable("Deckard is stopping"))
    }

    /// keel-mcp or pilotd, by file, as Keel's `KeelCallerTrust` compares them.
    async fn trusted(
        &self,
        header: &Header<'_>,
        connection: &Connection,
        what: &str,
    ) -> Result<(Arc<Shared>, Host), Error> {
        let shared = self.shared()?;
        let denied = || Error::AccessDenied(format!("only keel-mcp and Pilot may {what}"));
        let sender = header.sender().ok_or_else(denied)?.clone();
        let pid = DBusProxy::new(connection).await?;
        let pid = pid.get_connection_unix_process_id(sender.into()).await;
        let pid = pid.ok().filter(|pid| runs_one_of(*pid, &shared.trusted));
        let pid = pid.ok_or_else(denied)?;
        let external = cmdline(pid).is_ok_and(|line| keel::is_external_host(&line));
        Ok((
            shared,
            if external {
                Host::External
            } else {
                Host::Local
            },
        ))
    }
}

fn no_entity(entity_type: &str) -> Error {
    Error::UnknownAction(format!("no entity type {}", truncate(entity_type, 64)))
}

#[zbus::interface(name = "org.shipwright.Keel.Actions")]
impl Provider {
    fn describe(&self) -> String {
        let Ok(shared) = self.shared() else {
            return "{}".into();
        };
        let state = locked(&shared.state);
        let manifest = state.apps.get(&self.pkg).map(|entry| &entry.manifest.json);
        manifest.map_or_else(|| "{}".into(), Value::to_string)
    }

    async fn invoke(
        &self,
        action: String,
        arguments: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> Result<String, Error> {
        let what = "use an Android app's actions";
        let (shared, host) = self.trusted(&header, connection, what).await?;
        let Ok(Value::Object(args)) = serde_json::from_str::<Value>(&arguments) else {
            return Err(invalid("arguments must be a JSON object"));
        };
        let user_initiated = options
            .get("userInitiated")
            .map(|value| bool::try_from(&**value));
        let user_initiated = matches!(user_initiated, Some(Ok(true)));
        let result = shared.invoke(&self.pkg, &action, &args, user_initiated, host);
        Ok(result.await?.to_string())
    }

    fn get_entity(&self, entity_type: String, _id: String) -> Result<String, Error> {
        Err(no_entity(&entity_type))
    }

    fn find_entities(
        &self,
        entity_type: String,
        _query: String,
        _limit: u32,
    ) -> Result<String, Error> {
        Err(no_entity(&entity_type))
    }

    async fn get_context(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> Result<String, Error> {
        let what = "read an Android app's context";
        let (shared, host) = self.trusted(&header, connection, what).await?;
        Ok(shared.context(&self.pkg, host).await.to_string())
    }
}
