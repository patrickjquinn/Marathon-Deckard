use std::env;
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use deckard_bridge::api::{BUS_NAME, OBJECT_PATH, SettingsApi};
use deckard_bridge::manage::Manage;
use deckard_bridge::runtime::{Runtime, call_unit, watch_unit};
use deckard_core::ids::PackageName;
use deckard_core::runtime_state::UnitCall;
use deckard_core::transport::{BridgeListener, BridgeSession};
use futures_lite::future;
use serde_json::{Map, Value};
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
            Ok(Some(message)) => manage.handle(generation, &message),
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
    if runtime.set_connected(manage.connected()) {
        emit_runtime(&bus, &runtime)
    }
}

fn listen_system(mut listener: BridgeListener, manage: Manage, runtime: Runtime, bus: Connection) {
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
                reader = Some(thread::spawn(move || {
                    serve_session(
                        session,
                        generation,
                        rx,
                        thread_manage,
                        thread_runtime,
                        thread_bus,
                    )
                }));
            }
            Err(error) => eprintln!("deckard-bridge: rejected system peer: {error}"),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        icon_dirs: vec![data.join("icons"), cache],
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
    let bridge_bus = bus.clone();
    let bridge_runtime = runtime.clone();
    thread::spawn(move || listen_system(listener, manage, bridge_runtime, bridge_bus));
    let watch_bus = bus.clone();
    let watch_runtime = runtime.clone();
    thread::spawn(move || {
        if let Err(error) = watch_unit(&watch_bus, &watch_runtime) {
            eprintln!("deckard-bridge: unit watch failed: {error}")
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
