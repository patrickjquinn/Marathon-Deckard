use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use deckard_bridge::api::{BUS_NAME, OBJECT_PATH, SettingsApi};
use deckard_bridge::app_channel::{AppChannel, ChannelEvent, ChannelHandle};
use deckard_bridge::ime::{
    ImeController, ShellBus, ShellEvent, listen_shell, send_shell_calls, watch_shell_owner,
};
use deckard_bridge::manage::Manage;
use deckard_bridge::notifications::{NotificationBridge, bridge_event};
use deckard_bridge::packages::PackageBridge;
use deckard_bridge::runtime::{Runtime, call_unit, try_restart_unit, watch_unit};
use deckard_bridge::shell_sync;
use deckard_bridge::watchdog::{self, BootWatchdog};
use deckard_core::ids::PackageName;
use deckard_core::runtime_state::UnitCall;
use deckard_core::transport::{BridgeListener, BridgeSession};
use deckard_packages::{BridgeRoute, BridgeSend};
use futures_lite::future;
use serde_json::{Map, Value, json};
use zbus::blocking::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

fn home() -> io::Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is missing"))
}

fn data_dir() -> io::Result<PathBuf> {
    Ok(env::var_os("DECKARD_WAYDROID_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().unwrap_or_default().join(".local/share"))
                .join("waydroid/data")
        }))
}

fn emit_runtime(bus: &Connection, runtime: &Runtime) {
    let _ = bus.emit_signal(
        None::<&str>,
        OBJECT_PATH,
        "org.marathonos.Deckard1",
        "RuntimeChanged",
        &(runtime.snapshot(),),
    );
}

fn serve_session(
    mut session: BridgeSession,
    generation: u64,
    outgoing: async_channel::Receiver<Map<String, Value>>,
    manage: Manage,
    runtime: Runtime,
    bus: Connection,
    packages: PackageBridge,
) {
    if session
        .set_read_timeout(Some(Duration::from_millis(100)))
        .is_err()
    {
        manage.disconnect(generation);
        if runtime.set_connected(manage.connected()) {
            emit_runtime(&bus, &runtime)
        }
        return;
    }
    loop {
        while let Ok(message) = outgoing.try_recv() {
            if let Err(error) = session.send_message(&message) {
                eprintln!("deckard-bridge: system bridge write failed: {error}");
                manage.disconnect(generation);
                if runtime.set_connected(manage.connected()) {
                    emit_runtime(&bus, &runtime)
                }
                return;
            }
        }
        match session.read_message() {
            Ok(Some(message)) => {
                manage.handle(generation, &message);
                if let Err(error) = packages.handle(BridgeRoute::System, &message) {
                    eprintln!("deckard-bridge: system package event failed: {error}");
                }
            }
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
    manage.disconnect(generation);
    let _ = packages.disconnected(BridgeRoute::System);
    if runtime.set_connected(manage.connected()) {
        emit_runtime(&bus, &runtime)
    }
}

fn listen_system(
    mut listener: BridgeListener,
    manage: Manage,
    runtime: Runtime,
    bus: Connection,
    packages: PackageBridge,
) {
    let mut reader: Option<thread::JoinHandle<()>> = None;
    let mut info_worker: Option<thread::JoinHandle<()>> = None;
    loop {
        match listener.accept() {
            Ok(session) => {
                let (tx, rx) = async_channel::bounded(64);
                let generation = manage.connect(tx);
                if let Some(worker) = reader.take() {
                    let _ = worker.join();
                }
                if let Some(worker) = info_worker.take() {
                    let _ = worker.join();
                }
                if runtime.set_connected(true) {
                    emit_runtime(&bus, &runtime)
                }
                let info_manage = manage.clone();
                let info_runtime = runtime.clone();
                let info_bus = bus.clone();
                info_worker = Some(thread::spawn(move || {
                    if let Ok(info) = future::block_on(info_manage.request("info", Map::new())) {
                        if !info_manage.is_generation(generation) {
                            return;
                        }
                        let release = info.get("release").and_then(Value::as_str).unwrap_or("");
                        let release = deckard_core::validate::clean_text(release, 16);
                        if info_runtime.set_version(&release) {
                            emit_runtime(&info_bus, &info_runtime)
                        }
                    }
                }));
                let thread_manage = manage.clone();
                let thread_runtime = runtime.clone();
                let thread_bus = bus.clone();
                let thread_packages = packages.clone();
                reader = Some(thread::spawn(move || {
                    serve_session(
                        session,
                        generation,
                        rx,
                        thread_manage,
                        thread_runtime,
                        thread_bus,
                        thread_packages,
                    )
                }));
            }
            Err(error) => eprintln!("deckard-bridge: rejected system peer: {error}"),
        }
    }
}

fn serve_app(
    events: Receiver<ChannelEvent>,
    handle: ChannelHandle,
    packages: PackageBridge,
    notifications: NotificationBridge,
    actions: Receiver<deckard_core::notification_state::BridgeEvent>,
) {
    let mut current = None;
    loop {
        match events.recv_timeout(Duration::from_millis(10)) {
            Ok(ChannelEvent::Connected(generation)) => {
                current = Some(generation);
                if let Some(hello) = json!({"t":"hello","v":1}).as_object().cloned() {
                    let _ = handle.send_to(generation, hello);
                }
            }
            Ok(ChannelEvent::Message(generation, message)) if current == Some(generation) => {
                let kind = message.get("t").and_then(Value::as_str).unwrap_or("");
                let result = if matches!(
                    kind,
                    "inspect_result"
                        | "device_result"
                        | "packages_result"
                        | "install_result"
                        | "install_progress"
                        | "package_changed"
                ) {
                    packages
                        .handle(BridgeRoute::User, &message)
                        .map_err(|error| error.to_string())
                } else {
                    notifications
                        .handle(&message)
                        .map_err(|error| error.to_string())
                };
                if let Err(error) = result {
                    eprintln!("deckard-bridge: app event failed: {error}");
                }
            }
            Ok(ChannelEvent::Disconnected(generation)) if current == Some(generation) => {
                current = None;
                let _ = packages.disconnected(BridgeRoute::User);
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(action) = actions.try_recv() {
            let _ = handle.send(bridge_event(&action));
        }
    }
}

fn serve_ime(events: Receiver<ChannelEvent>, handle: ChannelHandle, bus: Connection) {
    let (shell_sender, shell_events) = mpsc::channel();
    let signal_bus = bus.clone();
    let signal_sender = shell_sender.clone();
    thread::spawn(move || {
        if let Err(error) = listen_shell(signal_bus, signal_sender) {
            eprintln!("deckard-bridge: IME signal listener failed: {error}");
        }
    });
    let owner_bus = bus.clone();
    thread::spawn(move || {
        if let Err(error) = watch_shell_owner(owner_bus, shell_sender) {
            eprintln!("deckard-bridge: Shell owner watch failed: {error}");
        }
    });
    let shell = ShellBus::new(bus);
    let mut controller = ImeController::default();
    let mut current = None;
    loop {
        match events.recv_timeout(Duration::from_millis(10)) {
            Ok(ChannelEvent::Connected(generation)) => {
                if current.replace(generation).is_some() {
                    let calls = controller.disconnected();
                    let _ = send_shell_calls(&shell, &calls);
                }
                if let Err(error) = shell.register() {
                    eprintln!("deckard-bridge: IME register failed: {error}");
                }
            }
            Ok(ChannelEvent::Message(generation, message)) if current == Some(generation) => {
                let effects = controller.android_message(&message);
                if let Err(error) = send_shell_calls(&shell, &effects.shell_calls) {
                    eprintln!("deckard-bridge: IME Shell call failed: {error}");
                }
                if let Some(reply) = effects.to_android {
                    let _ = handle.send_to(generation, reply);
                }
            }
            Ok(ChannelEvent::Disconnected(generation)) if current == Some(generation) => {
                current = None;
                let calls = controller.disconnected();
                let _ = send_shell_calls(&shell, &calls);
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        loop {
            match shell_events.try_recv() {
                Ok(ShellEvent::Listening | ShellEvent::Appeared) => {
                    if let Err(error) = shell.register() {
                        eprintln!("deckard-bridge: IME register failed: {error}");
                    }
                }
                Ok(ShellEvent::Signal(signal)) => {
                    if let Some(message) = controller.shell_signal(signal) {
                        let _ = handle.send(message);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let data = data_dir()?;
    if !data.is_dir() {
        return Err(format!("Waydroid data directory missing: {}", data.display()).into());
    }
    let home = home()?;
    let state = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"))
        .join("deckard");
    let applications = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("applications");
    let cgroup = env::var_os("DECKARD_CGROUP")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup/lxc.payload.waydroid"));
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"))
        .join("deckard/icons");
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
    let package = PackageName::try_from("org.marathonos.deckard.system")?;
    let listener = BridgeListener::bind(
        data.join("deckard/system.sock"),
        data.join("system/packages.list"),
        package,
    )?;
    let (ime_channel, ime_handle, ime_events) = AppChannel::bind(
        data.join("deckard/ime.sock"),
        data.join("system/packages.list"),
        PackageName::try_from("org.marathonos.deckard.ime")?,
    )?;
    let (app_channel, app_handle, app_events) = AppChannel::bind(
        data.join("deckard/bridge.sock"),
        data.join("system/packages.list"),
        PackageName::try_from("org.marathonos.deckard.bridge")?,
    )?;
    let user_send = app_handle.clone();
    let system_send = manage.clone();
    let package_send: Arc<BridgeSend> = Arc::new(move |route, value| {
        let Some(message) = value.as_object().cloned() else {
            return false;
        };
        match route {
            BridgeRoute::User => user_send.send(message).is_ok(),
            BridgeRoute::System => system_send.send(message),
        }
    });
    let packages = PackageBridge::register(
        bus.clone(),
        data.join("deckard/install"),
        package_send,
        PathBuf::from("/usr/bin/marathon-store-handler"),
    )?;
    let (notifications, actions) =
        NotificationBridge::new(bus.clone(), state.join("notifications.json"), cache)?;
    let _hub_listener = notifications.listen_hub()?;
    let app_packages = packages.clone();
    thread::spawn(move || serve_app(app_events, app_handle, app_packages, notifications, actions));
    thread::spawn(move || {
        if let Err(error) = app_channel.run() {
            eprintln!("deckard-bridge: app socket failed: {error}");
        }
    });
    let bridge_bus = bus.clone();
    let bridge_runtime = runtime.clone();
    thread::spawn(move || listen_system(listener, manage, bridge_runtime, bridge_bus, packages));
    let ime_bus = bus.clone();
    thread::spawn(move || serve_ime(ime_events, ime_handle, ime_bus));
    thread::spawn(move || {
        if let Err(error) = ime_channel.run() {
            eprintln!("deckard-bridge: IME socket failed: {error}");
        }
    });
    let evidence = Path::new("/usr/libexec/deckard-boot-evidence");
    let evidence = evidence
        .exists()
        .then(|| vec!["pkexec".into(), evidence.into()]);
    let restart_bus = bus.clone();
    let boot_watchdog = BootWatchdog::spawn(data.join("deckard/booted"), evidence, move || {
        if let Err(error) = future::block_on(call_unit(restart_bus.inner(), UnitCall::Restart)) {
            eprintln!("deckard-bridge: restarting Android failed: {error}");
        }
    });
    let follow_bus = bus.clone();
    thread::spawn(move || {
        let restart_bus = follow_bus.clone();
        let restart = move || {
            if let Err(error) = future::block_on(try_restart_unit(restart_bus.inner())) {
                eprintln!("deckard-bridge: restarting Android failed: {error}");
            }
        };
        if let Err(error) = watchdog::follow_shell(&follow_bus, restart) {
            eprintln!("deckard-bridge: Shell follower failed: {error}");
        }
    });
    let watch_bus = bus.clone();
    let watch_runtime = runtime.clone();
    thread::spawn(move || {
        let changed = || {
            emit_runtime(&watch_bus, &watch_runtime);
            boot_watchdog.set_active_since(watch_runtime.active_since_ms());
        };
        if let Err(error) = watch_unit(&watch_bus, &watch_runtime, changed) {
            eprintln!("deckard-bridge: unit watch failed: {error}")
        }
    });
    let doze_bus = bus.clone();
    let doze = data.join("deckard/doze");
    thread::spawn(move || {
        if let Err(error) = shell_sync::sync_doze(&doze_bus, &doze) {
            eprintln!("deckard-bridge: Doze sync failed: {error}");
        }
    });
    let palette_bus = bus.clone();
    let palette = data.join("deckard/palette");
    thread::spawn(move || {
        if let Err(error) = shell_sync::sync_palette(palette_bus.inner(), &palette) {
            eprintln!("deckard-bridge: palette sync failed: {error}");
        }
    });
    let marker = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("deckard-login");
    if !marker.exists() && std::fs::write(marker, []).is_ok() {
        thread::sleep(Duration::from_secs(3));
        if runtime.at_login() {
            let _ = future::block_on(call_unit(bus.inner(), UnitCall::Start));
        }
    }
    loop {
        thread::sleep(Duration::from_secs(60));
        if bus.is_closed() {
            break;
        }
    }
    Ok(())
}
