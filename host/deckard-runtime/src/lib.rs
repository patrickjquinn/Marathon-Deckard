//! The native Deckard user session (upstream `waydroid session`).

mod desktop;
mod monitor;
mod timezone;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use deckard_host_tools::{bus, names};
use deckard_launch::binder::{self, BinderConfig, Platform};
use deckard_launch::session::{ContainerManagerProxyBlocking, SESSION_NAME};
use nix::unistd::User;
use rustix::event::{self, PollFd, PollFlags};
use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
use rustix::io::Errno;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM, SIGUSR1};
use zbus::blocking::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

pub use deckard_launch::Result;

const SESSION_PATH: &str = "/SessionManager";
const CONTAINER_UNIT: &str = "deckard-container.service";
const EVENT_QUEUE: usize = 64;

struct SessionManager {
    stop: Arc<AtomicBool>,
    wake: UnixStream,
}

#[zbus::interface(name = "org.marathonos.Deckard.SessionManager")]
impl SessionManager {
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        let _ = (&self.wake).write(&[0]);
    }
}

#[zbus::proxy(
    interface = "org.marathonos.Deckard.SessionManager",
    default_service = "org.marathonos.Deckard.Session",
    default_path = "/SessionManager"
)]
trait Session {
    fn stop(&self) -> zbus::Result<()>;
}

fn env_nonempty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

fn socket_identity(path: &Path) -> Result<(u64, u64)> {
    let metadata = fs::metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err("Wayland display is not a socket".into());
    }
    Ok((metadata.dev(), metadata.ino()))
}

fn density(host_property: Option<&str>, grid_unit_px: Option<&str>) -> Result<String> {
    if let Some(value) = host_property.filter(|value| !value.is_empty()) {
        return Ok(value.to_owned());
    }
    let Some(value) = grid_unit_px else {
        return Ok("0".to_owned());
    };
    let pixels: u32 = value.parse()?;
    let density = pixels
        .checked_mul(20)
        .ok_or("GRID_UNIT_PX overflows LCD density")?;
    Ok(density.to_string())
}

/// What the session tells the container manager about the user.
pub struct SessionSpec<'a> {
    pub runtime_dir: &'a Path,
    pub display: &'a str,
    pub data_home: &'a Path,
    pub home: &'a Path,
    pub user_name: &'a str,
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
    pub pulse_runtime_path: &'a Path,
    pub lcd_density: &'a str,
}

/// The `Start` payload, upstream's fourteen session fields.
///
/// # Errors
/// The Wayland display is unset or not a socket, or the data directory
/// cannot be created.
pub fn session_payload(spec: &SessionSpec<'_>) -> Result<HashMap<String, String>> {
    if spec.display.is_empty() || spec.display == "None" {
        return Err("WAYLAND_DISPLAY is not set".into());
    }
    let socket = spec.runtime_dir.join(spec.display);
    if !socket
        .metadata()
        .is_ok_and(|metadata| metadata.file_type().is_socket())
    {
        let socket = socket.display();
        return Err(format!("Wayland socket {socket} is missing or is not a socket").into());
    }
    let user_state = spec.data_home.join(names::USER_STATE);
    let data = user_state.join("data");
    fs::create_dir_all(&data)?;
    let path = |path: &Path| path.to_string_lossy().into_owned();
    let fields = [
        ("user_name", spec.user_name.to_owned()),
        ("user_id", spec.uid.to_string()),
        ("group_id", spec.gid.to_string()),
        ("host_user", path(spec.home)),
        ("pid", spec.pid.to_string()),
        ("xdg_data_home", path(spec.data_home)),
        ("xdg_runtime_dir", path(spec.runtime_dir)),
        ("wayland_display", spec.display.to_owned()),
        ("pulse_runtime_path", path(spec.pulse_runtime_path)),
        ("state", "STOPPED".to_owned()),
        ("lcd_density", spec.lcd_density.to_owned()),
        ("background_start", "true".to_owned()),
        ("waydroid_user_state", path(&user_state)),
        ("waydroid_data", path(&data)),
    ];
    Ok(fields
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect())
}

fn host_density() -> Result<String> {
    let property = match Command::new("getprop").arg("ro.sf.lcd_density").output() {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8(output.stdout)?.trim().to_owned())
        }
        Ok(_) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    density(property.as_deref(), env_nonempty("GRID_UNIT_PX").as_deref())
}

fn make_payload() -> Result<HashMap<String, String>> {
    let uid = nix::unistd::geteuid();
    if uid.is_root() {
        return Err("session start must run as the graphical user, not root".into());
    }
    let user = User::from_uid(uid)?.ok_or("current user has no passwd entry")?;
    let runtime_dir =
        PathBuf::from(env_nonempty("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is not set")?);
    let display = env_nonempty("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".to_owned());
    let data_home =
        env_nonempty("XDG_DATA_HOME").map_or_else(|| user.dir.join(".local/share"), PathBuf::from);
    let pulse =
        env_nonempty("PULSE_RUNTIME_PATH").map_or_else(|| runtime_dir.join("pulse"), PathBuf::from);
    session_payload(&SessionSpec {
        runtime_dir: &runtime_dir,
        display: &display,
        data_home: &data_home,
        home: &user.dir,
        user_name: &user.name,
        uid: uid.as_raw(),
        gid: nix::unistd::getegid().as_raw(),
        pid: std::process::id(),
        pulse_runtime_path: &pulse,
        lcd_density: &host_density()?,
    })
}

fn start_container(connection: &Connection, payload: &HashMap<String, String>) -> Result<()> {
    Ok(ContainerManagerProxyBlocking::new(connection)?.start(payload.clone())?)
}

fn start_system_container() -> Result<()> {
    let status = Command::new("systemctl")
        .args(["start", CONTAINER_UNIT])
        .status()?;
    if !status.success() {
        return Err(format!("systemctl start {CONTAINER_UNIT} failed: {status}").into());
    }
    Ok(())
}

fn android_ready(boot: &str, credential_encrypted: &str) -> bool {
    boot == "1" && credential_encrypted == "true"
}

/// Waits for Android to boot and unlock user 0, then exports its launchers.
fn export_installed_apps(config: &BinderConfig, data_home: &Path, stop: &AtomicBool) -> Result<()> {
    let platform = Platform::connect(config)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
        let boot = platform.getprop("sys.boot_completed", "");
        let unlocked = platform.getprop("sys.user.0.ce_available", "");
        if let (Ok(boot), Ok(unlocked)) = (boot, unlocked)
            && android_ready(&boot, &unlocked)
            && let Ok(apps) = platform.apps()
            && !apps.is_empty()
        {
            let count = desktop::export(data_home, &apps)?;
            eprintln!("deckard-runtime: exported {count} Android launchers");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(500));
    }
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    Err("Android app catalog did not become ready".into())
}

fn apply(data_home: &Path, config: &BinderConfig, event: Option<monitor::Event>) -> Result<()> {
    let platform = Platform::connect(config)?;
    match event {
        Some(monitor::Event::Package {
            package,
            removed: true,
        }) => desktop::update(data_home, &package, None),
        Some(monitor::Event::Package {
            package,
            removed: false,
        }) => desktop::update(data_home, &package, platform.app(&package)?.as_ref()),
        Some(monitor::Event::UserUnlocked) | None => {
            desktop::export(data_home, &platform.apps()?).map(drop)
        }
    }
}

/// Android 16's package callback to the user monitor never fires, but the
/// bridge owns `deckard.<package>` on the session bus while an app is
/// installed. Any change of owner asks Android about that package again.
fn follow_bridge(events: &mpsc::SyncSender<monitor::Event>, resync: &AtomicBool) -> Result<()> {
    let bus = Connection::session()?;
    let changes = zbus::blocking::fdo::DBusProxy::new(&bus)?.receive_name_owner_changed()?;
    for change in changes {
        let Ok(args) = change.args() else { continue };
        let Some(package) = args.name().strip_prefix(names::APP_ID_PREFIX) else {
            continue;
        };
        let event = monitor::Event::Package {
            package: package.to_owned(),
            removed: false,
        };
        if events.try_send(event).is_err() {
            resync.store(true, Ordering::Release);
        }
    }
    Ok(())
}

/// Exports the launchers once Android is ready, then follows Android's
/// user monitor callbacks instead of polling the catalog.
fn monitor_installed_apps(data_home: &Path, stop: &Arc<AtomicBool>) -> Result<()> {
    let config = BinderConfig::from_file(Path::new(names::CONFIG))?;
    let (sender, events) = mpsc::sync_channel(EVENT_QUEUE);
    let resync = Arc::new(AtomicBool::new(false));
    let service = monitor::Monitor {
        events: sender,
        resync: Arc::clone(&resync),
    };
    let service_config = config.clone();
    let service_stop = Arc::clone(stop);
    let bridge_events = service.events.clone();
    let bridge_resync = Arc::clone(&resync);
    thread::spawn(move || {
        if let Err(error) = binder::serve(&service_config, service, service_stop) {
            eprintln!("deckard-runtime: Android user monitor unavailable: {error}");
        }
    });
    thread::spawn(move || {
        if let Err(error) = follow_bridge(&bridge_events, &bridge_resync) {
            eprintln!("deckard-runtime: not following the bridge's apps: {error}");
        }
    });
    // A later userUnlocked callback exports the catalog.
    if let Err(error) = export_installed_apps(&config, data_home, stop) {
        eprintln!("deckard-runtime: Android desktop export failed: {error}");
    }
    timezone::propagate(&config, stop);
    while !stop.load(Ordering::Acquire) {
        let event = match events.try_recv() {
            Ok(event) => Some(event),
            Err(_) if resync.swap(false, Ordering::AcqRel) => None,
            Err(_) => match events.recv() {
                Ok(event) => Some(event),
                Err(_) if stop.load(Ordering::Acquire) => break,
                Err(_) => return Err("Android package callbacks stopped".into()),
            },
        };
        if let Err(error) = apply(data_home, &config, event) {
            eprintln!("deckard-runtime: Android launcher update failed: {error}");
        }
    }
    Ok(())
}

/// How a session ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    Stopped,
    /// The compositor's socket changed: Android needs a new session.
    CompositorRestarted,
}

/// Owns [`SESSION_NAME`], starts the container for this user and runs
/// until stopped, told to leave (SIGUSR1) or the compositor's socket changes.
///
/// # Errors
/// The session cannot start.
pub fn session_start() -> Result<Ended> {
    let payload = make_payload()?;
    let runtime_dir = Path::new(&payload["xdg_runtime_dir"]);
    let socket = runtime_dir.join(&payload["wayland_display"]);
    let initial_socket = socket_identity(&socket)?;
    let marker = runtime_dir.join(names::REQUESTED);
    let stop = Arc::new(AtomicBool::new(false));
    let leave_container = Arc::new(AtomicBool::new(false));
    let (mut wakes, wake) = UnixStream::pair()?;
    wakes.set_nonblocking(true)?;
    wake.set_nonblocking(true)?;
    for signal in [SIGHUP, SIGINT, SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    signal_hook::flag::register(SIGUSR1, Arc::clone(&leave_container))?;
    for signal in [SIGHUP, SIGINT, SIGTERM, SIGUSR1] {
        signal_hook::low_level::pipe::register(signal, wake.try_clone()?)?;
    }
    let display_dir = socket.parent().ok_or("Wayland socket has no directory")?;
    let watch = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
    let entries = WatchFlags::CREATE | WatchFlags::DELETE | WatchFlags::MOVED_FROM;
    let entries = entries | WatchFlags::MOVED_TO | WatchFlags::DELETE_SELF | WatchFlags::MOVE_SELF;
    inotify::add_watch(&watch, display_dir, entries)?;
    let session_bus = bus::session()?;
    let closed = session_bus.inner().clone();
    let closed_wake = wake.try_clone()?;
    let closed_task = async move {
        closed.closed().await;
        let _ = (&closed_wake).write(&[0]);
    };
    session_bus
        .inner()
        .executor()
        .spawn(closed_task, "session-bus-closed")
        .detach();
    let manager = SessionManager {
        stop: Arc::clone(&stop),
        wake,
    };
    session_bus.object_server().at(SESSION_PATH, manager)?;
    let name_reply =
        session_bus.request_name_with_flags(SESSION_NAME, RequestNameFlags::DoNotQueue.into())?;
    if name_reply != RequestNameReply::PrimaryOwner {
        return Err("another Deckard session already owns the bus name".into());
    }

    fs::write(&marker, [])?;
    if let Err(error) = start_system_container() {
        let _ = fs::remove_file(&marker);
        return Err(error);
    }
    fs::remove_file(&marker)?;
    let system_bus = bus::system()?;
    start_container(&system_bus, &payload)?;

    let data_home = PathBuf::from(&payload["xdg_data_home"]);
    let export_stop = Arc::clone(&stop);
    thread::spawn(move || {
        if let Err(error) = monitor_installed_apps(&data_home, &export_stop) {
            eprintln!("deckard-runtime: Android launcher monitor failed: {error}");
        }
    });

    let mut compositor_changed = false;
    let mut drained = [0_u8; 4096];
    while !stop.load(Ordering::Acquire)
        && !leave_container.load(Ordering::Acquire)
        && !session_bus.is_closed()
    {
        if socket_identity(&socket).ok() != Some(initial_socket) {
            compositor_changed = true;
            break;
        }
        let mut ready = [
            PollFd::new(&wakes, PollFlags::IN),
            PollFd::new(&watch, PollFlags::IN),
        ];
        match event::poll(&mut ready, None) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
        while wakes.read(&mut drained).is_ok_and(|count| count > 0) {}
        while rustix::io::read(&watch, &mut drained).is_ok_and(|count| count > 0) {}
    }
    if !leave_container.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        ContainerManagerProxyBlocking::new(&system_bus)?.stop(false)?;
    }
    Ok(if compositor_changed {
        Ended::CompositorRestarted
    } else {
        Ended::Stopped
    })
}

/// Asks the running session to stop, else stops the container directly.
///
/// # Errors
/// Neither the session nor the container manager answers.
pub fn session_stop() -> Result<()> {
    let session = Connection::session().and_then(|bus| SessionProxyBlocking::new(&bus)?.stop());
    if session.is_ok() {
        return Ok(());
    }
    Ok(ContainerManagerProxyBlocking::new(&Connection::system()?)?.stop(true)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockContainer(Arc<Mutex<Option<HashMap<String, String>>>>);

    #[zbus::interface(name = "org.marathonos.Deckard.ContainerManager")]
    impl MockContainer {
        #[zbus(name = "Start")]
        fn start(&self, payload: HashMap<String, String>) -> zbus::fdo::Result<()> {
            *self
                .0
                .lock()
                .map_err(|_| zbus::fdo::Error::Failed("poisoned".into()))? = Some(payload);
            Ok(())
        }
    }

    #[test]
    fn payload_matches_upstream_container_contract() -> Result<()> {
        let root = tempfile::tempdir()?;
        let runtime = root.path().join("run");
        fs::create_dir(&runtime)?;
        let _socket = std::os::unix::net::UnixListener::bind(runtime.join("marathon-wayland-0"))?;
        let home = root.path().join("home");
        let data_home = home.join(".local/share");
        let map = session_payload(&SessionSpec {
            runtime_dir: &runtime,
            display: "marathon-wayland-0",
            data_home: &data_home,
            home: &home,
            user_name: "user",
            uid: 1000,
            gid: 1000,
            pid: 1234,
            pulse_runtime_path: &runtime.join("pulse"),
            lcd_density: "320",
        })?;
        assert_eq!(map.len(), 14);
        assert_eq!(map["pid"], "1234");
        assert_eq!(map["state"], "STOPPED");
        assert_eq!(map["background_start"], "true");
        assert_eq!(
            map["waydroid_data"],
            data_home.join("deckard/data").to_string_lossy()
        );
        assert!(data_home.join("deckard/data").is_dir());
        Ok(())
    }

    #[test]
    fn missing_wayland_socket_fails_before_start() -> Result<()> {
        let root = tempfile::tempdir()?;
        let result = session_payload(&SessionSpec {
            runtime_dir: root.path(),
            display: "wayland-0",
            data_home: root.path(),
            home: root.path(),
            user_name: "user",
            uid: 1,
            gid: 1,
            pid: 1,
            pulse_runtime_path: root.path(),
            lcd_density: "0",
        });
        assert!(result.is_err());
        assert!(!root.path().join("deckard").exists());
        Ok(())
    }

    #[test]
    fn regular_file_is_not_a_wayland_socket() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("wayland-0"), [])?;
        let result = session_payload(&SessionSpec {
            runtime_dir: root.path(),
            display: "wayland-0",
            data_home: root.path(),
            home: root.path(),
            user_name: "user",
            uid: 1,
            gid: 1,
            pid: 1,
            pulse_runtime_path: root.path(),
            lcd_density: "0",
        });
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn compositor_socket_identity_is_not_a_regular_file() -> Result<()> {
        let root = tempfile::tempdir()?;
        let first = root.path().join("first");
        let second = root.path().join("second");
        let _first = std::os::unix::net::UnixListener::bind(&first)?;
        let _second = std::os::unix::net::UnixListener::bind(&second)?;
        assert_ne!(socket_identity(&first)?, socket_identity(&second)?);
        fs::write(root.path().join("regular"), [])?;
        assert!(socket_identity(&root.path().join("regular")).is_err());
        Ok(())
    }

    #[test]
    fn density_fallback_and_overflow() -> Result<()> {
        assert_eq!(density(Some("420"), Some("12"))?, "420");
        assert_eq!(density(None, Some("18"))?, "360");
        assert_eq!(density(None, None)?, "0");
        assert!(density(None, Some("4294967295")).is_err());
        Ok(())
    }

    #[test]
    fn export_waits_for_boot_and_user_unlock() {
        assert!(!android_ready("0", "true"));
        assert!(!android_ready("1", "false"));
        assert!(android_ready("1", "true"));
    }

    #[test]
    fn session_stop_dbus_contract() -> Result<()> {
        if env::var_os("DECKARD_RUNTIME_PRIVATE_BUS").is_none() {
            return Ok(());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (_wakes, wake) = UnixStream::pair()?;
        let owner = Connection::session()?;
        owner.object_server().at(
            SESSION_PATH,
            SessionManager {
                stop: Arc::clone(&stop),
                wake,
            },
        )?;
        let reply =
            owner.request_name_with_flags(SESSION_NAME, RequestNameFlags::DoNotQueue.into())?;
        assert_eq!(reply, RequestNameReply::PrimaryOwner);
        session_stop()?;
        assert!(stop.load(Ordering::Acquire));
        Ok(())
    }

    #[test]
    fn container_start_dbus_contract() -> Result<()> {
        if env::var_os("DECKARD_RUNTIME_PRIVATE_BUS").is_none() {
            return Ok(());
        }
        let captured = Arc::new(Mutex::new(None));
        let owner = Connection::session()?;
        owner
            .object_server()
            .at("/ContainerManager", MockContainer(Arc::clone(&captured)))?;
        let reply = owner.request_name_with_flags(
            "org.marathonos.Deckard.Container",
            RequestNameFlags::DoNotQueue.into(),
        )?;
        assert_eq!(reply, RequestNameReply::PrimaryOwner);
        let mut payload = HashMap::new();
        payload.insert("user_id".to_owned(), "1000".to_owned());
        let caller = Connection::session()?;
        start_container(&caller, &payload)?;
        let actual = captured.lock().map_err(|_| "poisoned mock")?;
        assert_eq!(*actual, Some(payload));
        Ok(())
    }
}
