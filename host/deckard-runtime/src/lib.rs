#![deny(unsafe_code)]

mod desktop;
#[allow(unsafe_code)]
mod platform;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use deckard_launch::binder::{BinderConfig, Platform as PropertyPlatform};
use nix::unistd::{User, getegid, geteuid};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM, SIGUSR1};
use zbus::blocking::{Connection, Proxy};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::interface;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

const CONTAINER_NAME: &str = "id.waydro.Container";
const CONTAINER_PATH: &str = "/ContainerManager";
const CONTAINER_INTERFACE: &str = "id.waydro.ContainerManager";
const SESSION_NAME: &str = "id.waydro.Session";
const SESSION_PATH: &str = "/SessionManager";
const SESSION_INTERFACE: &str = "id.waydro.SessionManager";
const CONTAINER_UNIT: &str = "deckard-container.service";

#[derive(Clone)]
struct SessionManager {
    stop: Arc<AtomicBool>,
}

#[interface(name = "id.waydro.SessionManager")]
impl SessionManager {
    #[zbus(name = "Stop")]
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn invalid(message: impl Into<String>) -> Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into()).into()
}

fn unavailable(message: impl Into<String>) -> Error {
    io::Error::new(io::ErrorKind::NotConnected, message.into()).into()
}

fn env_nonempty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

fn wayland_socket(runtime_dir: &Path, display: &str) -> PathBuf {
    let path = Path::new(display);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        runtime_dir.join(path)
    }
}

fn socket_identity(path: &Path) -> io::Result<(u64, u64)> {
    let metadata = fs::metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Wayland display is not a socket",
        ));
    }
    Ok((metadata.dev(), metadata.ino()))
}

fn density(host_property: Option<&str>, grid_unit_px: Option<&str>) -> Result<String> {
    if let Some(value) = host_property.filter(|value| !value.is_empty()) {
        return Ok(value.to_owned());
    }
    match grid_unit_px {
        Some(value) => {
            let pixels: u32 = value.parse()?;
            Ok(pixels
                .checked_mul(20)
                .ok_or_else(|| invalid("GRID_UNIT_PX overflows LCD density"))?
                .to_string())
        }
        None => Ok("0".to_owned()),
    }
}

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

pub fn session_payload(spec: &SessionSpec<'_>) -> Result<HashMap<String, String>> {
    if spec.display.is_empty() || spec.display == "None" {
        return Err(invalid("WAYLAND_DISPLAY is not set"));
    }
    let socket = wayland_socket(spec.runtime_dir, spec.display);
    if !socket
        .metadata()
        .is_ok_and(|metadata| metadata.file_type().is_socket())
    {
        return Err(unavailable(format!(
            "Wayland socket {} is missing or is not a socket",
            socket.display()
        )));
    }
    let user_state = spec.data_home.join("waydroid");
    let data = user_state.join("data");
    fs::create_dir_all(&data)?;
    let fields = [
        ("user_name", spec.user_name.to_owned()),
        ("user_id", spec.uid.to_string()),
        ("group_id", spec.gid.to_string()),
        ("host_user", spec.home.to_string_lossy().into_owned()),
        ("pid", spec.pid.to_string()),
        (
            "xdg_data_home",
            spec.data_home.to_string_lossy().into_owned(),
        ),
        (
            "xdg_runtime_dir",
            spec.runtime_dir.to_string_lossy().into_owned(),
        ),
        ("wayland_display", spec.display.to_owned()),
        (
            "pulse_runtime_path",
            spec.pulse_runtime_path.to_string_lossy().into_owned(),
        ),
        ("state", "STOPPED".to_owned()),
        ("lcd_density", spec.lcd_density.to_owned()),
        ("background_start", "true".to_owned()),
        (
            "waydroid_user_state",
            user_state.to_string_lossy().into_owned(),
        ),
        ("waydroid_data", data.to_string_lossy().into_owned()),
    ];
    Ok(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
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

fn make_payload() -> Result<(HashMap<String, String>, PathBuf)> {
    let uid = geteuid().as_raw();
    if uid == 0 {
        return Err(invalid(
            "session start must run as the graphical user, not root",
        ));
    }
    let user =
        User::from_uid(geteuid())?.ok_or_else(|| invalid("current user has no passwd entry"))?;
    let runtime_dir = PathBuf::from(
        env_nonempty("XDG_RUNTIME_DIR").ok_or_else(|| invalid("XDG_RUNTIME_DIR is not set"))?,
    );
    let display = env_nonempty("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".to_owned());
    let home = user.dir;
    let data_home = env_nonempty("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let pulse_runtime_path = env_nonempty("PULSE_RUNTIME_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_dir.join("pulse"));
    let lcd_density = host_density()?;
    let payload = session_payload(&SessionSpec {
        runtime_dir: &runtime_dir,
        display: &display,
        data_home: &data_home,
        home: &home,
        user_name: &user.name,
        uid,
        gid: getegid().as_raw(),
        pid: std::process::id(),
        pulse_runtime_path: &pulse_runtime_path,
        lcd_density: &lcd_density,
    })?;
    Ok((payload, runtime_dir.join("marathon-waydroid-requested")))
}

fn container_proxy(connection: &Connection) -> Result<Proxy<'_>> {
    Ok(Proxy::new(
        connection,
        CONTAINER_NAME,
        CONTAINER_PATH,
        CONTAINER_INTERFACE,
    )?)
}

fn stop_container(connection: &Connection, quit_session: bool) -> Result<()> {
    container_proxy(connection)?.call::<_, _, ()>("Stop", &(quit_session,))?;
    Ok(())
}

fn start_container(connection: &Connection, payload: &HashMap<String, String>) -> Result<()> {
    container_proxy(connection)?.call::<_, _, ()>("Start", &(payload,))?;
    Ok(())
}

fn start_system_container() -> Result<()> {
    let status = Command::new("systemctl")
        .args(["start", CONTAINER_UNIT])
        .status()?;
    if !status.success() {
        return Err(unavailable(format!(
            "systemctl start {CONTAINER_UNIT} failed: {status}"
        )));
    }
    Ok(())
}

fn android_ready(boot: &str, credential_encrypted: &str) -> bool {
    boot == "1" && credential_encrypted == "true"
}

fn export_installed_apps(data_home: &Path, stop: &AtomicBool) -> Result<()> {
    let config = BinderConfig::from_file(Path::new("/var/lib/waydroid/waydroid.cfg"))?;
    let properties = PropertyPlatform::connect(&config)?;
    let catalog = platform::Platform::connect(&config)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
        let boot = properties.getprop("sys.boot_completed", "");
        let unlocked = properties.getprop("sys.user.0.ce_available", "");
        if let (Ok(boot), Ok(unlocked)) = (boot, unlocked)
            && android_ready(&boot, &unlocked)
            && let Ok(apps) = catalog.apps()
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
    Err(unavailable("Android app catalog did not become ready"))
}

fn monitor_installed_apps(data_home: &Path, stop: &AtomicBool) -> Result<()> {
    export_installed_apps(data_home, stop)?;
    let config = BinderConfig::from_file(Path::new("/var/lib/waydroid/waydroid.cfg"))?;
    while !stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_secs(15));
        if stop.load(Ordering::Acquire) {
            break;
        }
        let result = platform::Platform::connect(&config)
            .and_then(|catalog| catalog.apps())
            .and_then(|apps| desktop::export(data_home, &apps));
        if let Err(error) = result {
            eprintln!("deckard-runtime: Android desktop refresh failed: {error}");
        }
    }
    Ok(())
}

pub fn export_current_user_apps() -> Result<()> {
    if geteuid().as_raw() == 0 {
        return Err(invalid("desktop export must run as the graphical user"));
    }
    let user =
        User::from_uid(geteuid())?.ok_or_else(|| invalid("current user has no passwd entry"))?;
    let data_home = env_nonempty("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user.dir.join(".local/share"));
    export_installed_apps(&data_home, &AtomicBool::new(false))
}

pub fn session_start() -> Result<()> {
    let (payload, marker) = make_payload()?;
    let socket = wayland_socket(
        Path::new(&payload["xdg_runtime_dir"]),
        &payload["wayland_display"],
    );
    let initial_socket = socket_identity(&socket)?;
    let stop = Arc::new(AtomicBool::new(false));
    let leave_container = Arc::new(AtomicBool::new(false));
    for signal in [SIGHUP, SIGINT, SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    signal_hook::flag::register(SIGUSR1, Arc::clone(&leave_container))?;
    let session_bus = Connection::session()?;
    session_bus.object_server().at(
        SESSION_PATH,
        SessionManager {
            stop: Arc::clone(&stop),
        },
    )?;
    let name_reply =
        session_bus.request_name_with_flags(SESSION_NAME, RequestNameFlags::DoNotQueue.into())?;
    if name_reply != RequestNameReply::PrimaryOwner {
        return Err(unavailable(
            "another Waydroid session already owns the bus name",
        ));
    }

    fs::write(&marker, [])?;
    if let Err(error) = start_system_container() {
        let _ = fs::remove_file(&marker);
        return Err(error);
    }
    fs::remove_file(&marker)?;
    let system_bus = Connection::system()?;
    start_container(&system_bus, &payload)?;

    let data_home = PathBuf::from(&payload["xdg_data_home"]);
    let export_stop = Arc::clone(&stop);
    thread::spawn(move || {
        if let Err(error) = monitor_installed_apps(&data_home, &export_stop) {
            eprintln!("deckard-runtime: Android desktop export failed: {error}");
        }
    });

    let mut compositor_changed = false;
    while !stop.load(Ordering::Acquire)
        && !leave_container.load(Ordering::Acquire)
        && !session_bus.is_closed()
    {
        if socket_identity(&socket).ok() != Some(initial_socket) {
            compositor_changed = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if !leave_container.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        stop_container(&system_bus, false)?;
    }
    if compositor_changed {
        return Err(unavailable(
            "Wayland compositor socket changed; restarting Android session",
        ));
    }
    Ok(())
}

pub fn session_stop() -> Result<()> {
    if let Ok(session_bus) = Connection::session() {
        let session = Proxy::new(&session_bus, SESSION_NAME, SESSION_PATH, SESSION_INTERFACE);
        if session
            .and_then(|proxy| proxy.call::<_, _, ()>("Stop", &()))
            .is_ok()
        {
            return Ok(());
        }
    }
    stop_container(&Connection::system()?, true)
}

fn valid_property_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn property_command(key: &str, value: Option<&str>) -> Result<Command> {
    if !valid_property_key(key) {
        return Err(invalid("invalid Android property key"));
    }
    if let Some(value) = value
        && (value.len() > 91 || value.contains(['\n', '\r', '\0']))
    {
        return Err(invalid("invalid Android property value"));
    }
    let mut command = Command::new("timeout");
    command.args([
        "20",
        "lxc-attach",
        "-P",
        "/var/lib/waydroid/lxc",
        "-n",
        "waydroid",
        "--clear-env",
        "--",
        if value.is_some() {
            "/system/bin/setprop"
        } else {
            "/system/bin/getprop"
        },
        key,
    ]);
    if let Some(value) = value {
        command.arg(value);
    }
    Ok(command)
}

fn run_property(key: &str, value: Option<&str>) -> Result<Output> {
    let mut command = property_command(key, value)?;
    if geteuid().as_raw() != 0 {
        return Err(unavailable(
            "Android property access requires root; unprivileged Binder adapter is not implemented",
        ));
    }
    let bus = Connection::system()?;
    let container = container_proxy(&bus)?;
    let session: HashMap<String, String> = container.call("GetSession", &())?;
    if session.is_empty() {
        return Err(unavailable("Android session is stopped"));
    }
    let frozen = session.get("state").is_some_and(|state| state == "FROZEN");
    if frozen {
        container.call::<_, _, ()>("Unfreeze", &())?;
    }
    let result = command.output();
    if frozen {
        container.call::<_, _, ()>("Freeze", &())?;
    }
    let output = result?;
    if !output.status.success() {
        return Err(unavailable(format!(
            "Android property command failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
}

pub fn property_get(key: &str) -> Result<String> {
    Ok(String::from_utf8(run_property(key, None)?.stdout)?
        .trim_end()
        .to_owned())
}

pub fn property_set(key: &str, value: &str) -> Result<()> {
    run_property(key, Some(value))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockContainer(Arc<Mutex<Option<HashMap<String, String>>>>);

    #[interface(name = "id.waydro.ContainerManager")]
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
            data_home.join("waydroid/data").to_string_lossy()
        );
        assert!(data_home.join("waydroid/data").is_dir());
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
        assert!(!root.path().join("waydroid").exists());
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
    fn absolute_wayland_display_is_preserved() {
        assert_eq!(
            wayland_socket(Path::new("/run/user/1000"), "/tmp/socket"),
            PathBuf::from("/tmp/socket")
        );
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
    fn property_argv_is_not_shell_interpreted() -> Result<()> {
        let command = property_command("persist.sys.timezone", Some("Europe/Dublin"))?;
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args.last().map(|arg| arg.to_string_lossy().to_string()),
            Some("Europe/Dublin".to_owned())
        );
        assert_eq!(command.get_program(), "timeout");
        assert_eq!(args[1], "lxc-attach");
        assert!(property_command("a;touch /tmp/x", None).is_err());
        assert!(property_command("a.b", Some("x\ny")).is_err());
        Ok(())
    }

    #[test]
    fn session_stop_dbus_contract() -> Result<()> {
        if env::var_os("DECKARD_RUNTIME_PRIVATE_BUS").is_none() {
            return Ok(());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let owner = Connection::session()?;
        owner.object_server().at(
            SESSION_PATH,
            SessionManager {
                stop: Arc::clone(&stop),
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
            .at(CONTAINER_PATH, MockContainer(Arc::clone(&captured)))?;
        let reply =
            owner.request_name_with_flags(CONTAINER_NAME, RequestNameFlags::DoNotQueue.into())?;
        assert_eq!(reply, RequestNameReply::PrimaryOwner);
        let mut payload = HashMap::new();
        payload.insert("user_id".to_owned(), "1000".to_owned());
        let caller = Connection::session()?;
        start_container(&caller, &payload)?;
        let actual = captured.lock().map_err(|_| invalid("poisoned mock"))?;
        assert_eq!(*actual, Some(payload));
        Ok(())
    }
}
