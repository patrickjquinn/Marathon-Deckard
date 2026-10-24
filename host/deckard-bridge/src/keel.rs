//! Each Android app as an `org.shipwright.Keel.Actions` provider for keel-mcp
//! and Pilot (docs/pilot.md): the bus name `deckard.<pkg>`, on a connection
//! of its own, since keel-mcp describes a name again only when its owner
//! changes.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use async_executor::{Executor, Task};
use async_io::Timer;
use deckard_core::keel::{self, APP_ID_PREFIX, App, Manifest, Shortcut};
use deckard_core::{json, sync, validate};
use deckard_packages::{BridgeRoute, BridgeSend};
use futures_lite::future;
use serde_json::{Map, Value, json};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::zvariant::{OwnedValue, Value as Variant};
use zbus::{Connection, DBusError, connection, interface};

use crate::auth;
use crate::intents::{self, NAVIGATION};

const PATH: &str = "/org/shipwright/Keel/Actions";
const INTERFACE: &str = "org.shipwright.Keel.Actions";
const ERRORS: &str = "org.shipwright.Keel.Actions.Error.";

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
#[derive(Debug, DBusError)]
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
    #[zbus(name = "shipwright.Keel.Actions.Error.Timeout")]
    Timeout(String),
    #[zbus(name = "shipwright.Keel.Actions.Error.Cancelled")]
    Cancelled(String),
}

impl Error {
    /// The shell's Keel error, by the code after the prefix.
    fn from_shell(name: &str, message: String) -> Self {
        match name.strip_prefix(ERRORS).unwrap_or("Failed") {
            "UnknownAction" => Self::UnknownAction(message),
            "InvalidArguments" => Self::InvalidArguments(message),
            "NotAvailable" => Self::NotAvailable(message),
            "Timeout" => Self::Timeout(message),
            "Cancelled" => Self::Cancelled(message),
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
    /// Local, for a request the person started: may act on the screen.
    Person,
    /// A paired computer: no screen content.
    External,
}

/// Android's reply, or why there is none.
type Answer = Result<Map<String, Value>, String>;

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
    /// Upgraded by each call in flight.
    calls: Weak<()>,
    /// The last `screen.read`'s.
    snapshot: i64,
}

#[derive(Default)]
struct State {
    apps: HashMap<String, Entry>,
    /// What the shell was told, by package.
    secure: HashMap<String, bool>,
    /// The packages whose secure check runs.
    checking: HashSet<String>,
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
    /// Every app Android has; the others are withdrawn.
    List(HashSet<String>),
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

fn keel_op(op: &str, pkg: &str) -> Value {
    json!({"t": "keel", "op": op, "pkg": pkg})
}

/// At most `limit` bytes of the file at `path`.
fn read_at_most(path: impl AsRef<Path>, limit: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    File::open(path)?.take(limit).read_to_end(&mut out)?;
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
        let (weak, bus) = (Arc::downgrade(&shared), shared.bus.clone());
        let executor = &shared.executor;
        executor.spawn(registry(weak.clone(), incoming)).detach();
        executor.spawn(refresher(weak.clone(), refresh)).detach();
        let foreground = async move {
            let follow = intents::follow_foreground(&bus, |id| {
                let Some(shared) = weak.upgrade() else {
                    return ControlFlow::Break(());
                };
                shared.foreground(id);
                ControlFlow::Continue(())
            });
            if let Err(error) = follow.await {
                eprintln!("deckard-bridge: keel: no foreground changes: {error}");
            }
        };
        executor.spawn(foreground).detach();
        Ok(Self(shared))
    }

    /// The DeckardBridge messages `handle` takes.
    #[must_use]
    pub fn handles(kind: &str) -> bool {
        matches!(
            kind,
            "app_info" | "app_gone" | "app_list" | "launched" | "screen" | "ui_done"
        )
    }

    /// A message from DeckardBridge (`User`) or DeckardSystem (`System`), in
    /// wire order; other types are ignored.
    pub fn handle(&self, route: BridgeRoute, message: &Map<String, Value>) {
        let kind = json::string(message, "t");
        let change = match (route, kind) {
            (BridgeRoute::User, "app_info") => match keel::app(message) {
                Some(app) => Change::App(app),
                None => return eprintln!("deckard-bridge: keel: app_info refused: bad package"),
            },
            (BridgeRoute::User, "app_gone") => {
                Change::Gone(json::string(message, "pkg").to_owned())
            }
            (BridgeRoute::User, "app_list") => {
                let pkgs = json::array(message, "pkgs");
                let listed = pkgs.iter().filter_map(Value::as_str);
                Change::List(listed.map(str::to_owned).collect())
            }
            (BridgeRoute::System, "keel_changed") if self.0.known(json::string(message, "pkg")) => {
                return self.0.refresh_shortcuts(json::string(message, "pkg"));
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
            let known: Vec<String> = sync::lock(&self.0.state).apps.keys().cloned().collect();
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
        let ask = keel_op("shortcuts", &pkg);
        let reply = shared.request(BridgeRoute::System, ask).await;
        if let Some(reply) = reply.ok().filter(|reply| json::boolean(reply, "ok")) {
            let shortcuts = keel::shortcuts(json::array(&reply, "shortcuts"));
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

impl Shared {
    /// App `id` came to the front: the shell keeps a secure app's windows off
    /// its card and out of screenshots.
    fn foreground(self: &Arc<Self>, id: &str) {
        let pkg = id.strip_prefix(APP_ID_PREFIX).filter(|pkg| self.known(pkg));
        if let Some(pkg) = pkg
            && sync::lock(&self.state).checking.insert(pkg.to_owned())
        {
            let (pkg, shared) = (pkg.to_owned(), Arc::clone(self));
            let check = async move {
                shared.check_secure(&pkg, 2).await;
                sync::lock(&shared.state).checking.remove(&pkg);
            };
            self.executor.spawn(check).detach();
        }
    }

    fn change(&self, change: Change) {
        if self.changes.try_send(change).is_err() {
            eprintln!("deckard-bridge: keel: too many app changes; one dropped");
        }
    }

    fn known(&self, pkg: &str) -> bool {
        sync::lock(&self.state).apps.contains_key(pkg)
    }

    fn app(&self, pkg: &str) -> Option<App> {
        let state = sync::lock(&self.state);
        state.apps.get(pkg).map(|entry| entry.app.clone())
    }

    fn label(&self, pkg: &str) -> String {
        self.app(pkg).map(|app| app.label).unwrap_or_default()
    }

    async fn load(self: &Arc<Self>) {
        let saved = read_at_most(&self.state_path, MAX_STATE_BYTES).ok();
        let saved = saved.and_then(|bytes| serde_json::from_slice::<Vec<Value>>(&bytes).ok());
        let saved = saved.unwrap_or_default();
        let objects = saved.iter().filter_map(Value::as_object);
        let apps = objects.filter_map(keel::app);
        for app in apps {
            self.publish(app).await;
        }
    }

    fn save(&self) {
        let apps: Vec<App> = {
            let state = sync::lock(&self.state);
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
            Change::List(listed) => {
                let known: Vec<String> = sync::lock(&self.state).apps.keys().cloned().collect();
                let gone: Vec<&String> =
                    known.iter().filter(|pkg| !listed.contains(*pkg)).collect();
                for pkg in &gone {
                    self.unpublish(pkg).await;
                }
                if !gone.is_empty() {
                    self.save();
                }
            }
        }
    }

    /// Serves `app`; a changed manifest moves to a new connection.
    async fn publish(self: &Arc<Self>, app: App) {
        let manifest = keel::manifest(&app);
        if let Some(entry) = sync::lock(&self.state).apps.get_mut(&app.pkg)
            && entry.manifest.json == manifest.json
        {
            entry.app = app;
            return;
        }
        self.unpublish(&app.pkg).await;
        let id = keel::app_id(&app.pkg);
        if sync::lock(&self.state).apps.len() >= MAX_APPS {
            return eprintln!("deckard-bridge: keel: too many apps; not publishing {id}");
        }
        let calls = Arc::new(());
        let weak = Arc::downgrade(&calls);
        let provider = Provider {
            apps: Arc::downgrade(self),
            pkg: app.pkg.clone(),
            calls,
        };
        let builder = async {
            let builder = connection::Builder::session()?.internal_executor(false);
            builder.serve_at(PATH, provider)?.build().await
        };
        let connection = match builder.await {
            Ok(connection) => connection,
            Err(error) => return eprintln!("deckard-bridge: keel: cannot publish {id}: {error}"),
        };
        let ticker = connection.clone();
        let tick = self.executor.spawn(async move {
            loop {
                ticker.executor().tick().await;
            }
        });
        let tools = manifest.json["tools"].as_array().map_or(0, Vec::len);
        let pkg = app.pkg.clone();
        let entry = Entry {
            app,
            manifest,
            connection: connection.clone(),
            _tick: tick,
            calls: weak,
            snapshot: 0,
        };
        // In the catalogue before the name is owned: Describe never answers {}.
        sync::lock(&self.state).apps.insert(pkg.clone(), entry);
        let flags = RequestNameFlags::DoNotQueue.into();
        match connection.request_name_with_flags(id.as_str(), flags).await {
            Ok(RequestNameReply::PrimaryOwner) => {
                eprintln!("deckard-bridge: keel: published {id} with {tools} actions");
            }
            owned => {
                sync::lock(&self.state).apps.remove(&pkg);
                eprintln!("deckard-bridge: keel: cannot publish {id}: {owned:?}");
            }
        }
    }

    /// Releases the name before the next owner asks for it.
    async fn unpublish(&self, pkg: &str) {
        let Some(entry) = sync::lock(&self.state).apps.remove(pkg) else {
            return;
        };
        let id = keel::app_id(pkg);
        if let Err(error) = entry.connection.release_name(id.as_str()).await {
            eprintln!("deckard-bridge: keel: cannot release {id}: {error}");
        }
        eprintln!("deckard-bridge: keel: withdrew {id}");
        // Calls in flight run on the connection's executor: tick it until they end.
        let drain = async move {
            while entry.calls.strong_count() > 1 {
                Timer::after(WINDOW_POLL).await;
            }
            Timer::after(WINDOW_POLL).await;
            drop(entry);
        };
        self.executor.spawn(drain).detach();
    }

    fn connected_changed(&self, route: BridgeRoute, connected: bool) {
        let mut state = sync::lock(&self.state);
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
    async fn request(&self, route: BridgeRoute, mut message: Value) -> Answer {
        let (reply, answer) = async_channel::bounded(1);
        let id = {
            let mut state = sync::lock(&self.state);
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
        let answered = intents::within(BRIDGE_TIMEOUT, answer.recv()).await;
        let mut state = sync::lock(&self.state);
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
        let mut state = sync::lock(&self.state);
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
        if sync::lock(&self.state).system && self.refreshes.try_send(pkg.to_owned()).is_err() {
            eprintln!("deckard-bridge: keel: too many shortcut refreshes; one dropped");
        }
    }

    async fn shell(&self, action: &str, args: Value) -> Result<Map<String, Value>, Error> {
        let options: HashMap<&str, Variant<'_>> = HashMap::new();
        let body = (action, args.to_string(), options);
        let shell = ("org.marathonos.shell", PATH, INTERFACE);
        match intents::call(&self.bus, shell, "Invoke", &body, SHELL_TIMEOUT).await {
            Ok(reply) => {
                let body: String = reply.body().deserialize().unwrap_or_default();
                Ok(serde_json::from_str(&body).unwrap_or_default())
            }
            Err(zbus::Error::MethodError(name, message, _)) => {
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
        if need == Need::InFront && !(lit && json::string(&state, "foregroundApp") == id) {
            let why = format!("{label} is not on screen: open it first with app.show");
            return Err(unavailable(why));
        }
        Ok(())
    }

    async fn wait_for_window(&self, id: &str) -> Result<(), Error> {
        for poll in 0..=WINDOW_POLLS {
            if poll > 0 {
                Timer::after(WINDOW_POLL).await;
            }
            let running = self.shell("apps.running", json!({})).await?;
            let running = json::array(&running, "result");
            if running.iter().any(|app| app["id"] == id) {
                return Ok(());
            }
        }
        Err(Error::Failed("the app's window did not appear".into()))
    }

    async fn launched(&self, route: BridgeRoute, launch: Value) -> Result<(), Error> {
        let reply = self.request(route, launch).await.map_err(unavailable)?;
        if json::boolean(&reply, "ok") {
            return Ok(());
        }
        let why = json::text(&reply, "error", 256);
        Err(Error::Failed(why))
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
        if !sync::lock(&self.state).connected(route) {
            self.shell("app.launch", json!({"id": id})).await?;
            return self.launched(route, launch).await;
        }
        let expected = intents::expect_window(&self.bus, &id, EXPECT_TIMEOUT).await;
        self.launched(route, launch).await?;
        if !expected {
            self.wait_for_window(&id).await?;
            self.shell("app.launch", json!({"id": id})).await?;
        }
        Ok(())
    }

    /// The screen of an unlocked phone's front app that no secure window
    /// hides.
    async fn read_screen(&self, pkg: &str, max_units: usize) -> Result<Value, Error> {
        self.unlocked(pkg, Need::InFront).await?;
        self.not_secure(pkg).await?;
        let ask = json!({"t": "screen_read", "pkg": pkg});
        let reply = self.request(BridgeRoute::User, ask).await;
        let reply = reply.map_err(unavailable)?;
        if !json::boolean(&reply, "ok") {
            let why = json::text(&reply, "error", 256);
            let why = Some(why).filter(|why| !why.is_empty());
            let why = why.as_deref().unwrap_or("the screen cannot be read");
            return Err(unavailable(why));
        }
        let screen = keel::screen(&reply).filter(|screen| screen.pkg == pkg);
        let mut state = sync::lock(&self.state);
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

    /// DeckardSystem's `secure` for the app, which it may leave out; None
    /// when it does not answer.
    async fn secure(&self, pkg: &str) -> Option<Option<bool>> {
        if !sync::lock(&self.state).system {
            return None;
        }
        let reply = self.request(BridgeRoute::System, keel_op("secure", pkg));
        let reply = reply
            .await
            .ok()
            .filter(|reply| json::boolean(reply, "ok"))?;
        Some(reply.get("secure").and_then(Value::as_bool))
    }

    /// DeckardSystem vouches that no window of the app is secure.
    async fn not_secure(&self, pkg: &str) -> Result<(), Error> {
        let secure = self.secure(pkg).await;
        let secure = secure.ok_or_else(|| unavailable(UNKNOWN_SECURE))?;
        let secure = secure.unwrap_or(true);
        self.report_secure(pkg, secure);
        if secure {
            let why = format!("{} protects this screen from being read", self.label(pkg));
            return Err(unavailable(why));
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
            if !sync::lock(&self.state).system {
                return;
            }
            if let Some(secure) = self.secure(pkg).await {
                return self.report_secure(pkg, secure == Some(true));
            }
        }
    }

    fn report_secure(&self, pkg: &str, secure: bool) {
        {
            let mut state = sync::lock(&self.state);
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
            let tell = intents::call(&bus, NAVIGATION, "SetAppWindowSecure", &body, SHELL_TIMEOUT);
            if let Err(error) = tell.await {
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
        host: Host,
    ) -> Result<Value, Error> {
        let unknown = || {
            Error::UnknownAction(format!(
                "no action named {}",
                validate::truncate(action, 64)
            ))
        };
        let (tool, search, shortcut, snapshot) = {
            let state = sync::lock(&self.state);
            let entry = state.apps.get(pkg);
            let entry = entry.ok_or_else(|| unavailable("the app is not installed"))?;
            let tools = entry.manifest.json["tools"].as_array();
            let tools = tools.map_or(&[][..], Vec::as_slice);
            let named = |tool: &&Value| tool["_meta"]["org.shipwright.keel/action"] == action;
            let tool = tools.iter().find(named).cloned().ok_or_else(unknown)?;
            let shortcut = entry.manifest.shortcuts.get(action).cloned();
            (tool, entry.app.search.clone(), shortcut, entry.snapshot)
        };
        if let Some(why) = keel::invalid(&tool["inputSchema"], args) {
            return Err(invalid(why));
        }
        let id = keel::app_id(pkg);
        eprintln!("deckard-bridge: keel: {id} {action}");
        let arg = |key: &str| json::string(args, key);
        let (launch, route) = match action {
            "app.show" if arg("link").trim().is_empty() => {
                self.shell("app.launch", json!({"id": id})).await?;
                return Ok(json!({}));
            }
            "app.show" => {
                let link = arg("link").trim();
                keel::link_allowed(link).map_err(invalid)?;
                let view = json!({"t": "launch", "pkg": pkg, "kind": "view", "uri": link});
                (view, BridgeRoute::User)
            }
            "share.text" => {
                let mut send =
                    json!({"t": "launch", "pkg": pkg, "kind": "send", "text": arg("text")});
                if !arg("subject").is_empty() {
                    send["subject"] = json!(arg("subject"));
                }
                (send, BridgeRoute::User)
            }
            "app.search" => {
                let query = arg("query").trim();
                let search = json!({"t": "launch", "pkg": pkg, "kind": "search", "query": query, "mode": search});
                (search, BridgeRoute::User)
            }
            "screen.read" if host == Host::External => {
                let why = "screen content is not shared with a paired computer";
                return Err(unavailable(why));
            }
            "screen.read" => return self.read_screen(pkg, SCREEN_UNITS).await,
            "ui.tap" | "ui.type" => {
                if host != Host::Person {
                    let why =
                        "only Pilot acts on an app's screen, for a request the person started";
                    return Err(unavailable(why));
                }
                let node = keel::parse_node(arg("node"))
                    .ok_or_else(|| invalid("node must be an id from screen.read, such as 3.17"))?;
                if node.snapshot != snapshot {
                    let why = "that id is from an older screen.read: read the screen again";
                    return Err(invalid(why));
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
                let mut start = keel_op("start", pkg);
                start["id"] = json!(shortcut.ok_or_else(unknown)?);
                (start, BridgeRoute::System)
            }
        };
        self.unlocked(pkg, Need::Unlocked).await?;
        self.launch_then_show(pkg, launch, route).await?;
        Ok(json!({}))
    }

    async fn context(&self, pkg: &str, host: Host) -> Value {
        if host == Host::External || !self.known(pkg) {
            return json!({});
        }
        let Ok(screen) = self.read_screen(pkg, CONTEXT_UNITS).await else {
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
    calls: Arc<()>,
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
        let sender = header.sender().ok_or_else(denied)?;
        let pid = auth::pid(connection, sender.as_str()).await;
        let pid = pid
            .ok()
            .filter(|pid| auth::runs_one_of(*pid, &shared.trusted));
        let pid = pid.ok_or_else(denied)?;
        let cmdline = read_at_most(format!("/proc/{pid}/cmdline"), MAX_CMDLINE);
        let host = match cmdline.is_ok_and(|line| keel::is_external_host(&line)) {
            true => Host::External,
            false => Host::Local,
        };
        Ok((shared, host))
    }
}

fn no_entity(entity_type: &str) -> Error {
    let entity_type = validate::truncate(entity_type, 64);
    Error::UnknownAction(format!("no entity type {entity_type}"))
}

#[interface(name = "org.shipwright.Keel.Actions")]
impl Provider {
    fn describe(&self) -> String {
        let Ok(shared) = self.shared() else {
            return "{}".into();
        };
        let state = sync::lock(&shared.state);
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
        let _call = Arc::clone(&self.calls);
        let what = "use an Android app's actions";
        let (shared, host) = self.trusted(&header, connection, what).await?;
        let Ok(Value::Object(args)) = serde_json::from_str::<Value>(&arguments) else {
            return Err(invalid("arguments must be a JSON object"));
        };
        let asked = options.get("userInitiated");
        let person = asked.is_some_and(|value| matches!(bool::try_from(&**value), Ok(true)));
        let host = match host {
            Host::Local if person => Host::Person,
            host => host,
        };
        let result = shared.invoke(&self.pkg, &action, &args, host);
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
        let _call = Arc::clone(&self.calls);
        let what = "read an Android app's context";
        let (shared, host) = self.trusted(&header, connection, what).await?;
        Ok(shared.context(&self.pkg, host).await.to_string())
    }
}
