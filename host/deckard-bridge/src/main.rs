use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use deckard_bridge::Outbound;
use deckard_bridge::api::{BUS_NAME, OBJECT_PATH, SettingsApi};
use deckard_bridge::app_channel::{AppChannel, ChannelEvent, ChannelHandle};
use deckard_bridge::audio::{self, AudioWake};
use deckard_bridge::ime;
use deckard_bridge::intents::Intents;
use deckard_bridge::keel::KeelApps;
use deckard_bridge::location::LocationSync;
use deckard_bridge::manage::Manage;
use deckard_bridge::notifications::{self, NotificationBridge};
use deckard_bridge::packages::PackageBridge;
use deckard_bridge::prompts::PermissionPrompts;
use deckard_bridge::reclaim::Reclaimer;
use deckard_bridge::runtime::{self, Runtime};
use deckard_bridge::sharing::Sharing;
use deckard_bridge::shell_sync;
use deckard_bridge::storage::SharedStorage;
use deckard_bridge::watchdog::{self, BootWatchdog};
use deckard_core::ids::PackageName;
use deckard_core::runtime_state::UnitCall;
use deckard_core::transport::{BridgeListener, BridgeSession};
use deckard_core::validate;
use deckard_host_tools::shared_folders;
use deckard_mpris::MprisAdapter;
use deckard_packages::{BridgeRoute, BridgeSend};
use futures_lite::future;
use serde_json::{Map, Value, json};
use zbus::blocking::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

type Error = Box<dyn std::error::Error + Send + Sync>;

const READ_POLL: Duration = Duration::from_millis(100);
const MEDIA_POLL: Duration = Duration::from_millis(10);
const SYSTEM_QUEUE: usize = 64;

fn emit_runtime(bus: &Connection, runtime: &Runtime) {
    let snapshot = (runtime.snapshot(),);
    let interface = "org.marathonos.Deckard1";
    let _ = bus.emit_signal(
        None::<&str>,
        OBJECT_PATH,
        interface,
        "RuntimeChanged",
        &snapshot,
    );
}

/// What the authenticated system socket feeds.
#[derive(Clone)]
struct System {
    manage: Manage,
    runtime: Runtime,
    bus: Connection,
    packages: PackageBridge,
    reclaimer: Reclaimer,
    keel: KeelApps,
    intents: Intents,
    sharing: Sharing,
    prompts: PermissionPrompts,
    storage: SharedStorage,
}

impl System {
    fn set_connected(&self, connected: bool) {
        if self.runtime.set_connected(connected) {
            emit_runtime(&self.bus, &self.runtime);
        }
    }

    fn handle(&self, generation: u64, message: &Map<String, Value>) {
        self.manage.handle(generation, message);
        self.reclaimer.handle(message);
        self.keel.handle(BridgeRoute::System, message);
        let packages = self.packages.handle(BridgeRoute::System, message);
        deckard_bridge::report("system package event failed", packages);
        self.intents.handle(generation, message);
        self.sharing.handle(generation, message);
        self.prompts.handle(message);
    }

    fn disconnected(&self, generation: u64) {
        self.manage.disconnect(generation);
        let _ = self.packages.disconnected(BridgeRoute::System);
        self.keel.set_connected(BridgeRoute::System, false);
        self.intents.disconnected(generation);
        self.sharing.disconnected(generation);
        self.prompts.system_lost();
        self.storage.set_connected(false);
        if !self.manage.connected() {
            self.reclaimer.disconnected();
        }
        self.set_connected(self.manage.connected());
    }

    /// Android's release, for Settings.
    fn fetch_version(&self, generation: u64) {
        let Ok(reply) = future::block_on(self.manage.request("info", Map::new())) else {
            return;
        };
        let release = reply.get("release").and_then(Value::as_str).unwrap_or("");
        if self.manage.is_generation(generation)
            && self.runtime.set_version(&validate::clean_text(release, 16))
        {
            emit_runtime(&self.bus, &self.runtime);
        }
    }
}

fn serve_system(
    mut session: BridgeSession,
    generation: u64,
    outgoing: async_channel::Receiver<Map<String, Value>>,
    system: &System,
) {
    let writer = session.set_read_timeout(Some(READ_POLL));
    if let Ok(mut writer) = writer.and_then(|()| session.writer()) {
        // Manage closes the queue when a new DeckardSystem connects.
        thread::spawn(move || {
            while let Ok(message) = outgoing.recv_blocking() {
                if let Err(error) = writer.send_message(&message) {
                    eprintln!("deckard-bridge: system bridge write failed: {error}");
                    break;
                }
            }
            writer.close();
        });
        loop {
            match session.read_message() {
                Ok(Some(message)) => system.handle(generation, &message),
                Ok(None) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => {
                    eprintln!("deckard-bridge: system bridge read failed: {error}");
                    break;
                }
            }
        }
    }
    system.disconnected(generation);
}

/// Serves one DeckardSystem at a time; a new one replaces the old once the
/// old one's session has wound down.
fn listen_system(mut listener: BridgeListener, system: &System) {
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    loop {
        let session = match listener.accept() {
            Ok(session) => session,
            Err(error) => {
                eprintln!("deckard-bridge: rejected system peer: {error}");
                continue;
            }
        };
        eprintln!("deckard-bridge: org.marathonos.deckard.system connected");
        let (sender, outgoing) = async_channel::bounded(SYSTEM_QUEUE);
        let generation = system.manage.connect(sender);
        system.intents.connected(generation);
        system.sharing.connected(generation);
        for worker in workers.drain(..) {
            let _ = worker.join();
        }
        system.keel.set_connected(BridgeRoute::System, true);
        system.storage.set_connected(true);
        system.set_connected(true);
        system.reclaimer.connected();
        let (info, served) = (system.clone(), system.clone());
        workers.push(thread::spawn(move || info.fetch_version(generation)));
        workers.push(thread::spawn(move || {
            serve_system(session, generation, outgoing, &served);
        }));
    }
}

/// What the authenticated app socket (DeckardBridge) feeds.
struct App {
    handle: ChannelHandle,
    packages: PackageBridge,
    notifications: NotificationBridge,
    media: MprisAdapter,
    audio: AudioWake,
    keel: KeelApps,
    intents: Intents,
    location: Option<LocationSync>,
}

impl App {
    fn handle(&mut self, message: &Map<String, Value>) {
        let kind = message.get("t").and_then(Value::as_str).unwrap_or("");
        let what = "app event failed";
        match kind {
            "location_request" => {
                if let Some(location) = &self.location {
                    location.handle(message);
                }
            }
            "pick_done" => {
                let id = message.get("id").and_then(Value::as_str).unwrap_or("");
                self.intents.pick_done(id);
            }
            _ if KeelApps::handles(kind) => self.keel.handle(BridgeRoute::User, message),
            _ if kind.starts_with("media") => {
                deckard_bridge::report(what, self.media.handle(message))
            }
            "inspect_result" | "device_result" | "packages_result" | "install_result"
            | "install_progress" | "package_changed" => {
                deckard_bridge::report(what, self.packages.handle(BridgeRoute::User, message));
            }
            _ => deckard_bridge::report(what, self.notifications.handle(message)),
        }
    }

    fn disconnected(&mut self) {
        self.keel.set_connected(BridgeRoute::User, false);
        self.media.clear();
        let _ = self.packages.disconnected(BridgeRoute::User);
        if let Some(location) = &self.location {
            location.bridge_lost();
        }
    }

    /// Polls for MPRIS commands between Android's messages.
    fn serve(mut self, events: &Receiver<ChannelEvent>) {
        let mut current = None;
        loop {
            match events.recv_timeout(MEDIA_POLL) {
                Ok(ChannelEvent::Connected(generation)) => {
                    if current.replace(generation).is_some() {
                        self.keel.set_connected(BridgeRoute::User, false);
                    }
                    self.keel.set_connected(BridgeRoute::User, true);
                    self.media.clear();
                    let hello = deckard_bridge::object(json!({"t": "hello", "v": 1}));
                    let _ = self.handle.send_to(generation, hello);
                    self.audio.wake();
                }
                Ok(ChannelEvent::Message(generation, message)) if current == Some(generation) => {
                    self.handle(&message);
                }
                Ok(ChannelEvent::Disconnected(generation)) if current == Some(generation) => {
                    current = None;
                    self.disconnected();
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let Some(generation) = current else { continue };
            for command in self.media.take_commands() {
                let _ = self.handle.send_to(generation, command);
            }
        }
    }
}

fn bind_app(
    data: &Path,
    socket: &str,
    package: &str,
) -> Result<(AppChannel, ChannelHandle, Receiver<ChannelEvent>), Error> {
    let packages = data.join("system/packages.list");
    let package = PackageName::try_from(package)?;
    Ok(AppChannel::bind(data.join(socket), packages, package)?)
}

fn main() -> Result<(), Error> {
    let home = dirs::home_dir().ok_or("HOME is missing")?;
    let data_home = dirs::data_dir().ok_or("no XDG data directory")?;
    let data = env::var_os("DECKARD_WAYDROID_DATA")
        .map_or_else(|| data_home.join("waydroid/data"), PathBuf::from);
    if !data.is_dir() {
        return Err(format!("Waydroid data directory missing: {}", data.display()).into());
    }
    let state = dirs::state_dir()
        .ok_or("no XDG state directory")?
        .join("deckard");
    let cache = dirs::cache_dir()
        .ok_or("no XDG cache directory")?
        .join("deckard/icons");
    let cgroup = env::var_os("DECKARD_CGROUP").map_or_else(
        || PathBuf::from("/sys/fs/cgroup/lxc.payload.waydroid"),
        PathBuf::from,
    );
    let applications = data_home.join("applications");
    let runtime = Runtime::new(
        state.join("settings.json"),
        applications,
        cgroup.join("memory.current"),
    );
    let manage = Manage::default();
    let bus = Connection::session()?;
    let api = SettingsApi {
        bus: bus.inner().clone(),
        runtime: runtime.clone(),
        manage: manage.clone(),
        icon_dirs: vec![data.join("icons"), cache.clone()],
    };
    bus.object_server().at(OBJECT_PATH, api)?;
    if bus.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())?
        != RequestNameReply::PrimaryOwner
    {
        return Err("org.marathonos.Deckard is already owned".into());
    }
    let listener = BridgeListener::bind(
        data.join("deckard/system.sock"),
        data.join("system/packages.list"),
        PackageName::try_from("org.marathonos.deckard.system")?,
    )?;
    let (ime_channel, ime_handle, ime_events) =
        bind_app(&data, "deckard/ime.sock", "org.marathonos.deckard.ime")?;
    let (app_channel, app_handle, app_events) = bind_app(
        &data,
        "deckard/bridge.sock",
        "org.marathonos.deckard.bridge",
    )?;
    let to_system: Arc<Outbound> = {
        let manage = manage.clone();
        Arc::new(move |message| manage.send(message))
    };
    let to_user: Arc<Outbound> = {
        let handle = app_handle.clone();
        Arc::new(move |message| handle.send(message).is_ok())
    };
    let package_send: Arc<BridgeSend> = {
        let (to_system, to_user) = (Arc::clone(&to_system), Arc::clone(&to_user));
        Arc::new(move |route, value| {
            let Some(message) = value.as_object().cloned() else {
                return false;
            };
            match route {
                BridgeRoute::User => to_user(message),
                BridgeRoute::System => to_system(message),
            }
        })
    };
    let keel_tools = vec![
        PathBuf::from("/usr/bin/keel-mcp"),
        PathBuf::from("/usr/bin/pilotd"),
    ];
    let keel = KeelApps::start(
        bus.inner().clone(),
        Arc::clone(&package_send),
        keel_tools,
        state.join("apps.json"),
    )?;
    let store_handler = PathBuf::from("/usr/bin/marathon-store-handler");
    let packages = PackageBridge::register(
        bus.clone(),
        data.join("deckard/install"),
        package_send,
        store_handler,
    )?;
    let media = MprisAdapter::new(None, cache.with_file_name("art"))?;
    let actions = Arc::clone(&to_user);
    let notifications = NotificationBridge::new(
        bus.clone(),
        state.join("notifications.json"),
        cache,
        move |action| {
            actions(notifications::bridge_event(&action));
        },
    )?;
    let _hub_listener = notifications.listen_hub()?;
    let xfer = data.join("deckard/xfer");
    let camera = dirs::picture_dir()
        .unwrap_or_else(|| home.join("Pictures"))
        .join("Marathon");
    let intents = Intents::start(
        bus.inner().clone(),
        Arc::clone(&to_system),
        xfer.clone(),
        camera,
    );
    let sharing = Sharing::new(bus.inner().clone(), Arc::clone(&to_system), xfer);
    bus.object_server().at(OBJECT_PATH, sharing.clone())?;
    let location = match Connection::system() {
        Ok(system) => Some(LocationSync::start(system.inner().clone(), to_user)),
        Err(error) => {
            eprintln!("deckard-bridge: no system bus, so no location for Android: {error}");
            None
        }
    };
    let app = App {
        handle: app_handle.clone(),
        packages: packages.clone(),
        notifications,
        media,
        audio: audio::spawn(bus.clone(), app_handle)?,
        keel: keel.clone(),
        intents: intents.clone(),
        location,
    };
    thread::spawn(move || app.serve(&app_events));
    deckard_bridge::spawn("app socket failed", move || app_channel.run());
    let system = System {
        manage,
        runtime: runtime.clone(),
        bus: bus.clone(),
        packages,
        reclaimer: Reclaimer::new(
            cgroup,
            PathBuf::from("/proc/pressure/memory"),
            Arc::clone(&to_system),
        ),
        keel,
        intents,
        sharing,
        prompts: PermissionPrompts::new(bus.inner().clone(), Arc::clone(&to_system)),
        storage: SharedStorage::start(
            shared_folders::host_folders(&home),
            state.join("shared-acl-v1"),
            to_system,
        ),
    };
    thread::spawn(move || listen_system(listener, &system));
    let ime_bus = bus.clone();
    thread::spawn(move || ime::serve(&ime_events, &ime_handle, &ime_bus));
    deckard_bridge::spawn("IME socket failed", move || ime_channel.run());
    let evidence = Path::new("/usr/libexec/deckard-boot-evidence");
    let evidence = evidence
        .exists()
        .then(|| vec!["pkexec".into(), evidence.into()]);
    let restart_bus = bus.clone();
    let boot_watchdog = BootWatchdog::spawn(data.join("deckard/booted"), evidence, move || {
        let restarted =
            future::block_on(runtime::call_unit(restart_bus.inner(), UnitCall::Restart));
        deckard_bridge::report("restarting Android failed", restarted);
    });
    let follow_bus = bus.clone();
    deckard_bridge::spawn("Shell follower failed", move || {
        let restart_bus = follow_bus.clone();
        watchdog::follow_shell(&follow_bus, move || {
            let restarted = future::block_on(runtime::try_restart_unit(restart_bus.inner()));
            deckard_bridge::report("restarting Android failed", restarted);
        })
    });
    let (watch_bus, watch_runtime) = (bus.clone(), runtime.clone());
    deckard_bridge::spawn("unit watch failed", move || {
        runtime::watch_unit(&watch_bus, &watch_runtime, || {
            emit_runtime(&watch_bus, &watch_runtime);
            boot_watchdog.set_active_since(watch_runtime.active_since_ms());
        })
    });
    let (doze_bus, doze) = (bus.clone(), data.join("deckard/doze"));
    deckard_bridge::spawn("Doze sync failed", move || {
        shell_sync::sync_doze(&doze_bus, &doze)
    });
    let (palette_bus, palette) = (bus.clone(), data.join("deckard/palette"));
    deckard_bridge::spawn("palette sync failed", move || {
        shell_sync::sync_palette(palette_bus.inner(), &palette)
    });
    let marker = dirs::runtime_dir()
        .unwrap_or_else(env::temp_dir)
        .join("deckard-login");
    if !marker.exists() && fs::write(marker, []).is_ok() {
        thread::sleep(Duration::from_secs(3));
        if runtime.at_login() {
            let _ = future::block_on(runtime::call_unit(bus.inner(), UnitCall::Start));
        }
    }
    while !bus.is_closed() {
        thread::sleep(Duration::from_secs(60));
    }
    Ok(())
}
