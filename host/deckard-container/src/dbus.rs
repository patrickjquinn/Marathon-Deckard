//! The system-bus `id.waydro.ContainerManager` contract (upstream
//! `tools/services/container_manager.py`) and its `/Initializer`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use deckard_host_tools::cfg;
use deckard_launch::binder::BinderConfig;
use rustix::fd::OwnedFd;
use rustix::process::{Pid, PidfdFlags, Signal};
use zbus::connection::{Builder, Connection};
use zbus::fdo::{self, DBusProxy};
use zbus::message::Header;
use zbus::names::BusName;

use crate::command::ProcessRunner;
use crate::drivers::{self, DriverNodes};
use crate::hardware::{self, HardwareRequest};
use crate::initializer_dbus::{self, InitializerService};
use crate::lxc::{self, ContainerManager, State};
use crate::session_config::{self, Stage};
use crate::{Result, config, properties};

pub const BUS_NAME: &str = "id.waydro.Container";
pub const OBJECT_PATH: &str = "/ContainerManager";

/// A process on the bus: a D-Bus caller or the session's owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Caller {
    pub uid: u32,
    pub pid: u32,
}

impl Caller {
    /// The sender's credentials, from the bus daemon.
    ///
    /// # Errors
    /// The sender is missing or the bus cannot name its credentials.
    pub async fn of(conn: &Connection, header: &Header<'_>) -> fdo::Result<Self> {
        fn denied(error: impl std::fmt::Display) -> fdo::Error {
            fdo::Error::AccessDenied(error.to_string())
        }
        let sender = header
            .sender()
            .ok_or_else(|| denied("missing D-Bus sender"))?;
        let name = BusName::try_from(sender.as_str()).map_err(denied)?;
        let proxy = DBusProxy::new(conn).await.map_err(denied)?;
        let uid = proxy
            .get_connection_unix_user(name.clone())
            .await
            .map_err(denied)?;
        let pid = proxy
            .get_connection_unix_process_id(name)
            .await
            .map_err(denied)?;
        Ok(Self { uid, pid })
    }

    /// Root, or this very session owner.
    fn controls(self, owner: Self) -> bool {
        self.uid == 0 || self == owner
    }
}

/// The session owner named by `Start`'s `user_id` and `pid`.
fn session_owner(fields: &HashMap<String, String>) -> fdo::Result<Caller> {
    let decimal = |key: &str| -> fdo::Result<u32> {
        let value = fields
            .get(key)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("missing {key}")))?;
        value
            .parse()
            .map_err(|_| fdo::Error::InvalidArgs(format!("invalid {key}")))
    };
    let owner = Caller {
        uid: decimal("user_id")?,
        pid: decimal("pid")?,
    };
    if owner.pid <= 1 || owner.pid > i32::MAX as u32 {
        return Err(fdo::Error::InvalidArgs("invalid session pid".into()));
    }
    Ok(owner)
}

/// Which same-user process, besides the owner, may read the session and
/// thaw it: the root-owned launcher, running in the user's manager.
#[derive(Clone)]
struct LauncherPolicy {
    executable: PathBuf,
    trust_root: PathBuf,
    proc_root: PathBuf,
    executable_owner: u32,
}

impl LauncherPolicy {
    fn production() -> Self {
        Self {
            executable: PathBuf::from("/usr/libexec/deckard-launch"),
            trust_root: PathBuf::from("/usr"),
            proc_root: PathBuf::from("/proc"),
            executable_owner: 0,
        }
    }

    /// Owned by the executable's owner and not writable by others.
    fn guarded(&self, metadata: &fs::Metadata) -> bool {
        metadata.uid() == self.executable_owner && metadata.mode() & 0o022 == 0
    }

    fn in_user_manager(&self, pid: u32, owner: Caller) -> bool {
        let root = format!("/user.slice/user-{0}.slice/user@{0}.service/", owner.uid);
        let mut cgroup = String::new();
        let path = self.proc_root.join(pid.to_string()).join("cgroup");
        let read =
            File::open(path).and_then(|file| file.take(16 * 1024 + 1).read_to_string(&mut cgroup));
        read.is_ok()
            && cgroup.len() <= 16 * 1024
            && cgroup.lines().any(|line| {
                line.strip_prefix("0::")
                    .is_some_and(|path| path.starts_with(&root))
            })
    }

    /// The launcher's metadata, when it and every directory up to the trust
    /// root are real and guarded.
    fn trusted_executable(&self) -> Option<fs::Metadata> {
        let executable = fs::symlink_metadata(&self.executable).ok()?;
        let plain = !self
            .executable
            .components()
            .any(|part| part == Component::ParentDir);
        let runnable = executable.is_file() && executable.mode() & 0o111 != 0;
        if !plain
            || !runnable
            || !self.guarded(&executable)
            || !self.executable.starts_with(&self.trust_root)
        {
            return None;
        }
        let parents = self.executable.ancestors().skip(1);
        for path in parents {
            let metadata = fs::symlink_metadata(path).ok()?;
            if !metadata.is_dir() || !self.guarded(&metadata) {
                return None;
            }
            if path == self.trust_root {
                return Some(executable);
            }
        }
        None
    }

    fn permits(&self, caller: Caller, owner: Caller) -> bool {
        if caller.uid != owner.uid || caller.pid == owner.pid {
            return false;
        }
        let Some(executable) = self.trusted_executable() else {
            return false;
        };
        let process = self.proc_root.join(caller.pid.to_string());
        let mine = fs::metadata(&process).is_ok_and(|metadata| metadata.uid() == caller.uid);
        let running = fs::metadata(process.join("exe"));
        mine && running.is_ok_and(|running| {
            (running.dev(), running.ino()) == (executable.dev(), executable.ino())
        }) && self.in_user_manager(owner.pid, owner)
            && self.in_user_manager(caller.pid, owner)
    }
}

struct Session {
    fields: HashMap<String, String>,
    owner: Caller,
    pidfd: OwnedFd,
}

impl Session {
    /// Tells the session process to exit. A pidfd cannot signal an
    /// unrelated process after PID reuse.
    fn quit(self) {
        let _ = rustix::process::pidfd_send_signal(&self.pidfd, Signal::USR1);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HardwareOutcome {
    Active,
    Stopped,
}

trait Backend: Send {
    fn start(&mut self, fields: &HashMap<String, String>, uid: u32) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn freeze(&mut self) -> Result<()>;
    fn unfreeze(&mut self) -> Result<()>;
    fn status(&mut self) -> Result<State>;
    fn get_property(&mut self, key: &str) -> Result<String>;
    fn set_property(&mut self, key: &str, value: &str) -> Result<()>;
    fn hardware(&mut self, _request: HardwareRequest) -> Result<HardwareOutcome> {
        Err("hardware service unavailable".into())
    }
    /// Whether the LXC instance exists, so that its status can be asked.
    fn initialized(&mut self) -> bool {
        true
    }
}

struct DirectBackend {
    manager: ContainerManager,
    hardware_sender: SyncSender<HardwareRequest>,
    hardware_stop: Option<Arc<AtomicBool>>,
    suspend_stops: bool,
}

impl DirectBackend {
    fn config_path(&self) -> PathBuf {
        self.manager.mounts.work.join("waydroid.cfg")
    }

    /// Reads `waydroid.cfg` and prepares the Binder and device nodes.
    fn prepare_production(&mut self) -> Result<()> {
        let base = self.manager.mounts.work.join("waydroid_base.prop");
        let initialized = self.initialized_instance() && base.is_file();
        crate::ensure(initialized, "Waydroid is not initialized")?;
        let text = config::read(&self.config_path())?;
        let images =
            PathBuf::from(cfg::get(&text, "images_path").unwrap_or("/var/lib/waydroid/images"));
        crate::ensure(images.is_absolute(), "images_path must be absolute")?;
        self.manager.mounts.images = images;
        self.manager.mounts.overlays = match config::get_or_default(&text, "mount_overlays") {
            Some("True") => true,
            Some("False") => false,
            _ => return Err("invalid mount_overlays".into()),
        };
        self.suspend_stops = config::get_or_default(&text, "suspend_action") == Some("stop");
        let dev = Path::new("/dev");
        drivers::ensure_binder_nodes(&mut ProcessRunner)?;
        DriverNodes::from_config(&self.config_path())?.prepare(dev)?;
        drivers::open_device_access(dev, Path::new("/sys/kernel/debug/sync/sw_sync"))
    }

    fn initialized_instance(&self) -> bool {
        let lxc = &self.manager.lxc;
        lxc.root.join(&lxc.name).join("config").is_file()
    }

    fn start_hardware(&mut self) {
        let config_path = self.config_path();
        if let Err(error) = drivers::record_protocols(&config_path, &self.manager.mounts.rootfs()) {
            eprintln!("deckard-containerd: cannot record Binder protocols: {error}");
        }
        let config = match BinderConfig::from_file(&config_path) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("deckard-containerd: hardware service disabled: {error}");
                return;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let sender = self.hardware_sender.clone();
        thread::spawn(move || {
            if let Err(error) = hardware::run(&config, sender, worker_stop) {
                eprintln!("deckard-containerd: hardware service failed: {error}");
            }
        });
        self.hardware_stop = Some(stop);
    }
}

impl Backend for DirectBackend {
    fn initialized(&mut self) -> bool {
        self.initialized_instance()
    }

    fn start(&mut self, fields: &HashMap<String, String>, uid: u32) -> Result<()> {
        let stopped = self.manager.lxc.status(&mut ProcessRunner)? == State::Stopped;
        crate::ensure(
            stopped,
            "container must be stopped before updating session files",
        )?;
        self.prepare_production()?;
        let stage = Stage::new(&self.manager.network.runtime);
        let instance = self.manager.lxc.root.join(&self.manager.lxc.name);
        session_config::write(&instance, fields, uid, Path::new("/run/user"), &stage)?;
        let started = properties::write(&self.manager.mounts.work, fields, uid)
            .and_then(|()| self.manager.start(&mut ProcessRunner));
        if let Err(error) = started {
            if self.manager.lxc.status(&mut ProcessRunner)? == State::Stopped {
                let _ = stage.release();
            }
            return Err(error);
        }
        self.start_hardware();
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(stop) = self.hardware_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.manager.stop(&mut ProcessRunner)?;
        Stage::new(&self.manager.network.runtime).release()
    }

    fn freeze(&mut self) -> Result<()> {
        self.manager.lxc.freeze(&mut ProcessRunner)
    }

    fn unfreeze(&mut self) -> Result<()> {
        self.manager.lxc.unfreeze(&mut ProcessRunner)
    }

    fn status(&mut self) -> Result<State> {
        self.manager.lxc.status(&mut ProcessRunner)
    }

    fn get_property(&mut self, key: &str) -> Result<String> {
        self.manager.lxc.get_property(&mut ProcessRunner, key)
    }

    fn set_property(&mut self, key: &str, value: &str) -> Result<()> {
        self.manager
            .lxc
            .set_property(&mut ProcessRunner, key, value)
    }

    fn hardware(&mut self, request: HardwareRequest) -> Result<HardwareOutcome> {
        let lxc = &self.manager.lxc;
        match request {
            HardwareRequest::Suspend if self.suspend_stops => {
                self.stop()?;
                Ok(HardwareOutcome::Stopped)
            }
            HardwareRequest::Suspend => {
                self.freeze()?;
                Ok(HardwareOutcome::Active)
            }
            HardwareRequest::Reboot => {
                lxc.stop(&mut ProcessRunner)?;
                lxc.start(&mut ProcessRunner)?;
                Ok(HardwareOutcome::Active)
            }
            HardwareRequest::ShutdownRequest(reason) => {
                lxc.wait_for(&mut ProcessRunner, State::Stopped, Duration::from_secs(30))?;
                if reason.starts_with('1') {
                    lxc.start(&mut ProcessRunner)?;
                    return Ok(HardwareOutcome::Active);
                }
                self.stop()?;
                Ok(HardwareOutcome::Stopped)
            }
        }
    }
}

struct ServiceState {
    backend: Box<dyn Backend>,
    session: Option<Session>,
}

impl ServiceState {
    /// Root or the session owner.
    fn authorize_control(&self, caller: Caller) -> fdo::Result<()> {
        let owner = self.session.as_ref().map(|session| session.owner);
        if caller.uid == 0 || owner.is_some_and(|owner| caller.controls(owner)) {
            return Ok(());
        }
        let reason = if owner.is_some() {
            "session UID/PID does not match D-Bus caller"
        } else {
            "no session owner"
        };
        Err(fdo::Error::AccessDenied(reason.into()))
    }

    /// Root, the session owner or the trusted launcher.
    fn authorize_read(&self, caller: Caller, launcher: &LauncherPolicy) -> fdo::Result<()> {
        let owner = self.session.as_ref().map(|session| session.owner);
        let launcher_ok = owner.is_some_and(|owner| launcher.permits(caller, owner));
        if self.authorize_control(caller).is_ok() || launcher_ok {
            return Ok(());
        }
        if owner.is_none() {
            return Err(fdo::Error::AccessDenied("no session owner".into()));
        }
        let reason = "caller is neither session owner nor trusted session launcher";
        Err(fdo::Error::AccessDenied(reason.into()))
    }
}

fn failed(error: Box<dyn std::error::Error + Send + Sync>) -> fdo::Error {
    fdo::Error::Failed(error.to_string())
}

fn poisoned<T>(_: T) -> fdo::Error {
    fdo::Error::Failed("state lock poisoned".into())
}

fn authorize_root(caller: Caller) -> fdo::Result<()> {
    if caller.uid != 0 {
        return Err(fdo::Error::AccessDenied(
            "property access requires root".into(),
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct ContainerService {
    state: Arc<Mutex<ServiceState>>,
    launcher: LauncherPolicy,
}

impl ContainerService {
    #[must_use]
    pub fn new(manager: ContainerManager, hardware_sender: SyncSender<HardwareRequest>) -> Self {
        let backend = DirectBackend {
            manager,
            hardware_sender,
            hardware_stop: None,
            suspend_stops: false,
        };
        Self::with_backend(Box::new(backend), LauncherPolicy::production())
    }

    fn with_backend(backend: Box<dyn Backend>, launcher: LauncherPolicy) -> Self {
        let state = ServiceState {
            backend,
            session: None,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            launcher,
        }
    }

    /// Runs `work` on the state off the bus's executor.
    async fn locked<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut ServiceState) -> fdo::Result<T> + Send + 'static,
    ) -> fdo::Result<T> {
        let state = Arc::clone(&self.state);
        blocking::unblock(move || work(&mut *state.lock().map_err(poisoned)?)).await
    }

    /// Stops a tracked session's container and tells its process to exit.
    ///
    /// # Errors
    /// The container cannot be stopped.
    pub async fn shutdown(&self) -> fdo::Result<()> {
        self.locked(|state| {
            if state.session.is_some() {
                state.backend.stop().map_err(failed)?;
            }
            if let Some(session) = state.session.take() {
                session.quit();
            }
            Ok(())
        })
        .await
    }

    /// Runs `work` with the container stopped. A tracked session is stopped
    /// first and started again afterwards; if that restart fails, the session
    /// is dropped and its process told to exit (SIGUSR1). An untracked running
    /// container is refused. Blocks other calls while `work` runs.
    ///
    /// # Errors
    /// The container is running untracked, stopping or restarting fails, or
    /// `work` fails.
    pub fn with_stopped<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut state = self.state.lock().map_err(|_| "state lock poisoned")?;
        let resume = state
            .session
            .as_ref()
            .map(|session| (session.fields.clone(), session.owner.uid));
        if resume.is_some() {
            state.backend.stop()?;
        } else if state.backend.initialized() && state.backend.status()? != State::Stopped {
            return Err("the container is running without a session; stop it first".into());
        }
        let result = work();
        if let Some((fields, uid)) = resume
            && let Err(error) = state.backend.start(&fields, uid)
        {
            if let Some(session) = state.session.take() {
                session.quit();
            }
            return Err(match result {
                Ok(_) => error,
                Err(failure) => {
                    format!("{failure}; restarting the session also failed: {error}").into()
                }
            });
        }
        result
    }

    /// Acts on one admitted `waydroidhardware` request.
    ///
    /// # Errors
    /// No session is tracked or the request fails.
    pub async fn handle_hardware_request(&self, request: HardwareRequest) -> fdo::Result<()> {
        self.locked(move |state| {
            if state.session.is_none() {
                return Err(fdo::Error::Failed("no active container session".into()));
            }
            if state.backend.hardware(request).map_err(failed)? == HardwareOutcome::Stopped
                && let Some(session) = state.session.take()
            {
                session.quit();
            }
            Ok(())
        })
        .await
    }

    async fn get_property_authorized(&self, key: String, caller: Caller) -> fdo::Result<String> {
        authorize_root(caller)?;
        lxc::validate_property_key(&key)
            .map_err(|error| fdo::Error::InvalidArgs(error.to_string()))?;
        self.locked(move |state| state.backend.get_property(&key).map_err(failed))
            .await
    }

    async fn set_property_authorized(
        &self,
        key: String,
        value: String,
        caller: Caller,
    ) -> fdo::Result<()> {
        authorize_root(caller)?;
        let invalid = |error: Box<dyn std::error::Error + Send + Sync>| {
            fdo::Error::InvalidArgs(error.to_string())
        };
        lxc::validate_property_key(&key).map_err(invalid)?;
        lxc::validate_property_value(&value).map_err(invalid)?;
        self.locked(move |state| state.backend.set_property(&key, &value).map_err(failed))
            .await
    }
}

#[zbus::interface(name = "id.waydro.ContainerManager")]
impl ContainerService {
    async fn start(
        &self,
        session: HashMap<String, String>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = Caller::of(conn, &header).await?;
        let owner = session_owner(&session)?;
        if !caller.controls(owner) {
            let reason = "session UID/PID does not match D-Bus caller";
            return Err(fdo::Error::AccessDenied(reason.into()));
        }
        let invalid = |message: String| fdo::Error::InvalidArgs(message);
        let pid =
            Pid::from_raw(owner.pid as i32).ok_or_else(|| invalid("invalid session pid".into()))?;
        let pidfd = rustix::process::pidfd_open(pid, PidfdFlags::empty())
            .map_err(|error| invalid(format!("session process unavailable: {error}")))?;
        self.locked(move |state| {
            if state.session.is_some() {
                return Err(fdo::Error::Failed("already tracking a session".into()));
            }
            state.backend.start(&session, owner.uid).map_err(failed)?;
            state.session = Some(Session {
                fields: session,
                owner,
                pidfd,
            });
            Ok(())
        })
        .await
    }

    async fn stop(
        &self,
        quit_session: bool,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = Caller::of(conn, &header).await?;
        self.locked(move |state| {
            state.authorize_control(caller)?;
            state.backend.stop().map_err(failed)?;
            if let Some(session) = state.session.take()
                && quit_session
            {
                session.quit();
            }
            Ok(())
        })
        .await
    }

    async fn freeze(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = Caller::of(conn, &header).await?;
        self.locked(move |state| {
            state.authorize_control(caller)?;
            state.backend.freeze().map_err(failed)
        })
        .await
    }

    async fn unfreeze(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = Caller::of(conn, &header).await?;
        let launcher = self.launcher.clone();
        self.locked(move |state| {
            state.authorize_read(caller, &launcher)?;
            state.backend.unfreeze().map_err(failed)
        })
        .await
    }

    async fn get_session(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<HashMap<String, String>> {
        let caller = Caller::of(conn, &header).await?;
        let launcher = self.launcher.clone();
        self.locked(move |state| {
            let Some(session) = state.session.as_ref() else {
                return Ok(HashMap::new());
            };
            state.authorize_read(caller, &launcher)?;
            let mut fields = session.fields.clone();
            let status = state.backend.status().map_err(failed)?;
            fields.insert("state".into(), format!("{status:?}").to_uppercase());
            Ok(fields)
        })
        .await
    }

    async fn get_property(
        &self,
        key: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<String> {
        self.get_property_authorized(key, Caller::of(conn, &header).await?)
            .await
    }

    async fn set_property(
        &self,
        key: String,
        value: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        self.set_property_authorized(key, value, Caller::of(conn, &header).await?)
            .await
    }
}

/// Owns `id.waydro.Container` and serves the manager and the initializer.
///
/// # Errors
/// The system bus or the name is unavailable.
pub async fn serve_system_bus(
    manager: ContainerManager,
    hardware_sender: SyncSender<HardwareRequest>,
) -> zbus::Result<(Connection, ContainerService)> {
    let service = ContainerService::new(manager, hardware_sender);
    let initializer = InitializerService::production(service.clone());
    let connection = Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, service.clone())?
        .serve_at(initializer_dbus::OBJECT_PATH, initializer)?
        .build()
        .await?;
    Ok((connection, service))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as StdError;
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::{Child, Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};
    use zbus::object_server::Interface;

    struct BusProcess(Child);

    impl Drop for BusProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[derive(Clone)]
    struct RecordingBackend(Arc<Mutex<Vec<&'static str>>>);

    impl Backend for RecordingBackend {
        fn start(&mut self, _fields: &HashMap<String, String>, _uid: u32) -> Result<()> {
            self.0
                .lock()
                .map_err(|_| "record lock poisoned")?
                .push("start");
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            self.0
                .lock()
                .map_err(|_| "record lock poisoned")?
                .push("stop");
            Ok(())
        }
        fn freeze(&mut self) -> Result<()> {
            self.0
                .lock()
                .map_err(|_| "record lock poisoned")?
                .push("freeze");
            Ok(())
        }
        fn unfreeze(&mut self) -> Result<()> {
            self.0
                .lock()
                .map_err(|_| "record lock poisoned")?
                .push("unfreeze");
            Ok(())
        }
        fn status(&mut self) -> Result<State> {
            Ok(State::Running)
        }
        fn get_property(&mut self, key: &str) -> Result<String> {
            if key != "sys.boot_completed" {
                return Err("unexpected test property".into());
            }
            self.0
                .lock()
                .map_err(|_| "record lock poisoned")?
                .push("get_property");
            Ok("1".into())
        }
        fn set_property(&mut self, key: &str, value: &str) -> Result<()> {
            if key != "persist.deckard.test" || value != "ready" {
                return Err("unexpected test property".into());
            }
            self.0
                .lock()
                .map_err(|_| "record lock poisoned")?
                .push("set_property");
            Ok(())
        }
        fn hardware(&mut self, request: HardwareRequest) -> Result<HardwareOutcome> {
            if request != HardwareRequest::Suspend {
                return Err("unexpected hardware request".into());
            }
            self.0
                .lock()
                .map_err(|_| "record lock poisoned")?
                .push("hardware_suspend");
            Ok(HardwareOutcome::Active)
        }
    }

    fn tracked(service: &ContainerService) -> Result<Child> {
        let process = Command::new("sleep").arg("30").spawn()?;
        let pid = process.id();
        let pidfd = rustix::process::pidfd_open(
            Pid::from_raw(i32::try_from(pid)?).ok_or("invalid test PID")?,
            PidfdFlags::empty(),
        )?;
        let fields = HashMap::from([
            ("user_id".to_owned(), "1000".to_owned()),
            ("pid".to_owned(), pid.to_string()),
        ]);
        service
            .state
            .lock()
            .map_err(|_| "state lock poisoned")?
            .session = Some(Session {
            owner: session_owner(&fields)?,
            fields,
            pidfd,
        });
        Ok(process)
    }

    #[test]
    fn init_coordination_restarts_the_tracked_session() -> Result<()> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = ContainerService::with_backend(
            Box::new(RecordingBackend(calls.clone())),
            LauncherPolicy::production(),
        );
        assert!(service.with_stopped(|| Ok(())).is_err());
        let mut session = tracked(&service)?;
        let record = calls.clone();
        service.with_stopped(|| {
            record
                .lock()
                .map_err(|_| "record lock poisoned")?
                .push("init");
            Ok(())
        })?;
        assert!(
            service
                .with_stopped(|| Err::<(), _>("failed init".into()))
                .is_err()
        );
        assert_eq!(
            *calls.lock().map_err(|_| "record lock poisoned")?,
            ["stop", "init", "start", "stop", "start"]
        );
        assert!(
            service
                .state
                .lock()
                .map_err(|_| "poisoned")?
                .session
                .is_some()
        );
        let _ = session.kill();
        let _ = session.wait();
        Ok(())
    }

    #[test]
    fn shutdown_does_not_stop_an_unowned_container() -> fdo::Result<()> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = ContainerService::with_backend(
            Box::new(RecordingBackend(calls.clone())),
            LauncherPolicy::production(),
        );
        async_io::block_on(service.shutdown())?;
        assert!(
            calls
                .lock()
                .map_err(|_| fdo::Error::Failed("record lock poisoned".into()))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn hardware_request_requires_a_tracked_session() -> fdo::Result<()> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = ContainerService::with_backend(
            Box::new(RecordingBackend(calls.clone())),
            LauncherPolicy::production(),
        );
        assert!(
            async_io::block_on(service.handle_hardware_request(HardwareRequest::Suspend)).is_err()
        );
        assert!(
            calls
                .lock()
                .map_err(|_| fdo::Error::Failed("record lock poisoned".into()))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn private_bus_exercises_sender_credentials_and_methods() -> Result<()> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or("dbus-daemon stdout unavailable")?;
        let bus = BusProcess(child);
        let mut reader = BufReader::new(stdout);
        let mut address = String::new();
        reader.read_line(&mut address)?;
        if address.trim().is_empty() {
            return Err("dbus-daemon did not provide an address".into());
        }
        let address = address.trim();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = ContainerService::with_backend(
            Box::new(RecordingBackend(calls.clone())),
            LauncherPolicy::production(),
        );
        async_io::block_on(async {
            let server = Builder::address(address)?
                .name(BUS_NAME)?
                .serve_at(OBJECT_PATH, service)?
                .build()
                .await?;
            let client = Builder::address(address)?.build().await?;
            let proxy =
                zbus::Proxy::new(&client, BUS_NAME, OBJECT_PATH, "id.waydro.ContainerManager")
                    .await?;
            let uid = rustix::process::geteuid().as_raw();
            let pid = std::process::id();
            let mut fields = HashMap::from([
                ("user_id".to_owned(), uid.to_string()),
                ("pid".to_owned(), pid.to_string()),
            ]);
            fields.insert("wayland_display".into(), "wayland-0".into());
            let mut bad = fields.clone();
            bad.insert("pid".into(), (pid + 1).to_string());
            let rejected: zbus::Result<()> = proxy.call("Start", &(bad,)).await;
            assert!(rejected.is_err());
            if uid != 0 {
                let denied_get: zbus::Result<String> = proxy
                    .call("GetProperty", &(String::from("sys.boot_completed"),))
                    .await;
                assert!(denied_get.is_err());
                let denied_set: zbus::Result<()> = proxy
                    .call(
                        "SetProperty",
                        &(String::from("persist.deckard.test"), String::from("ready")),
                    )
                    .await;
                assert!(denied_set.is_err());
            }
            let _: () = proxy.call("Start", &(fields.clone(),)).await?;
            let session: HashMap<String, String> = proxy.call("GetSession", &()).await?;
            assert_eq!(session.get("pid"), Some(&pid.to_string()));
            assert_eq!(session.get("state"), Some(&"RUNNING".to_owned()));
            let _: () = proxy.call("Freeze", &()).await?;
            let _: () = proxy.call("Unfreeze", &()).await?;
            let _: () = proxy.call("Stop", &(false,)).await?;
            let empty: HashMap<String, String> = proxy.call("GetSession", &()).await?;
            assert!(empty.is_empty());
            drop(client);
            drop(server);
            Ok::<(), zbus::Error>(())
        })?;
        assert_eq!(
            *calls.lock().map_err(|_| "record lock poisoned")?,
            ["start", "freeze", "unfreeze", "stop"]
        );
        drop(bus);
        Ok(())
    }

    #[test]
    fn private_bus_allows_trusted_launcher_but_not_another_same_uid_process() -> Result<()> {
        let uid = rustix::process::geteuid().as_raw();
        if uid == 0 {
            return Ok(());
        }
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let fixture = std::env::temp_dir().join(format!(
            "deckard-launch-policy-{}-{nonce}",
            std::process::id()
        ));
        let proc_root = fixture.join("proc");
        let binary = fixture.join("bin/deckard-launch");
        fs::create_dir_all(binary.parent().ok_or("launcher binary has no parent")?)?;
        fs::create_dir(&proc_root)?;
        fs::write(&binary, b"trusted executable identity")?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
        let session_process = BusProcess(Command::new("sleep").arg("30").spawn()?);
        let session_pid = session_process.0.id();
        let launcher_pid = std::process::id();
        let cgroup = format!(
            "0::/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/marathon-waydroid.service\n"
        );
        let launcher_cgroup = format!(
            "0::/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/marathon-shell.service\n"
        );
        for (pid, text) in [(session_pid, cgroup), (launcher_pid, launcher_cgroup)] {
            let process = proc_root.join(pid.to_string());
            fs::create_dir(&process)?;
            fs::write(process.join("cgroup"), text)?;
        }
        symlink(
            &binary,
            proc_root.join(launcher_pid.to_string()).join("exe"),
        )?;
        let policy = LauncherPolicy {
            executable: binary,
            trust_root: fixture.clone(),
            proc_root,
            executable_owner: uid,
        };
        assert!(policy.permits(
            Caller {
                uid,
                pid: launcher_pid
            },
            Caller {
                uid,
                pid: session_pid
            }
        ));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service =
            ContainerService::with_backend(Box::new(RecordingBackend(calls.clone())), policy);
        let owner = Caller {
            uid,
            pid: session_pid,
        };
        let pidfd = rustix::process::pidfd_open(
            Pid::from_raw(session_pid as i32).ok_or("invalid test session PID")?,
            PidfdFlags::empty(),
        )?;
        service
            .state
            .lock()
            .map_err(|_| "state lock poisoned")?
            .session = Some(Session {
            fields: HashMap::from([
                ("user_id".into(), uid.to_string()),
                ("pid".into(), session_pid.to_string()),
            ]),
            owner,
            pidfd,
        });

        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or("dbus-daemon stdout unavailable")?;
        let bus = BusProcess(child);
        let mut address = String::new();
        BufReader::new(stdout).read_line(&mut address)?;
        let address = address.trim().to_owned();
        if address.is_empty() {
            return Err("dbus-daemon did not provide an address".into());
        }
        async_io::block_on(async {
            let server = Builder::address(address.as_str())?
                .name(BUS_NAME)?
                .serve_at(OBJECT_PATH, service)?
                .build()
                .await?;
            let client = Builder::address(address.as_str())?.build().await?;
            let proxy =
                zbus::Proxy::new(&client, BUS_NAME, OBJECT_PATH, "id.waydro.ContainerManager")
                    .await?;
            let session: HashMap<String, String> = proxy.call("GetSession", &()).await?;
            assert_eq!(session.get("pid"), Some(&session_pid.to_string()));
            let _: () = proxy.call("Unfreeze", &()).await?;
            let denied_freeze: zbus::Result<()> = proxy.call("Freeze", &()).await;
            assert!(denied_freeze.is_err());
            let denied_stop: zbus::Result<()> = proxy.call("Stop", &(false,)).await;
            assert!(denied_stop.is_err());
            let address_for_call = address.clone();
            let unrelated = blocking::unblock(move || {
                Command::new("busctl")
                    .arg(format!("--address={address_for_call}"))
                    .args([
                        "call",
                        BUS_NAME,
                        OBJECT_PATH,
                        "id.waydro.ContainerManager",
                        "GetSession",
                    ])
                    .output()
            })
            .await?;
            assert!(!unrelated.status.success());
            assert!(
                String::from_utf8_lossy(&unrelated.stderr).contains("Access denied"),
                "unexpected busctl error: {}",
                String::from_utf8_lossy(&unrelated.stderr)
            );
            drop(client);
            drop(server);
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        })?;
        assert_eq!(
            *calls.lock().map_err(|_| "record lock poisoned")?,
            ["unfreeze"]
        );
        drop(bus);
        drop(session_process);
        fs::remove_dir_all(fixture)?;
        Ok(())
    }

    #[test]
    fn auth_requires_exact_uid_and_pid_except_root() -> fdo::Result<()> {
        let fields = HashMap::from([
            ("user_id".into(), "1000".into()),
            ("pid".into(), "2345".into()),
        ]);
        let owner = session_owner(&fields)?;
        assert!(
            Caller {
                uid: 1000,
                pid: 2345
            }
            .controls(owner)
        );
        assert!(
            !Caller {
                uid: 1000,
                pid: 2346
            }
            .controls(owner)
        );
        assert!(
            !Caller {
                uid: 1001,
                pid: 2345
            }
            .controls(owner)
        );
        assert!(Caller { uid: 0, pid: 9999 }.controls(owner));
        Ok(())
    }

    #[test]
    fn native_property_path_requires_root_and_validates_inputs() -> fdo::Result<()> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = ContainerService::with_backend(
            Box::new(RecordingBackend(calls.clone())),
            LauncherPolicy::production(),
        );
        let non_root = Caller {
            uid: 1000,
            pid: 123,
        };
        let root = Caller { uid: 0, pid: 1 };
        async_io::block_on(async {
            assert!(
                service
                    .get_property_authorized("sys.boot_completed".into(), non_root)
                    .await
                    .is_err()
            );
            assert!(
                service
                    .set_property_authorized(
                        "persist.deckard.test".into(),
                        "ready".into(),
                        non_root
                    )
                    .await
                    .is_err()
            );
            assert!(
                service
                    .get_property_authorized("bad;key".into(), root)
                    .await
                    .is_err()
            );
            assert!(
                service
                    .set_property_authorized(
                        "persist.deckard.test".into(),
                        "bad\nvalue".into(),
                        root
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                service
                    .get_property_authorized("sys.boot_completed".into(), root)
                    .await?,
                "1"
            );
            service
                .set_property_authorized("persist.deckard.test".into(), "ready".into(), root)
                .await?;
            Ok::<(), fdo::Error>(())
        })?;
        assert_eq!(
            *calls
                .lock()
                .map_err(|_| fdo::Error::Failed("record lock poisoned".into()))?,
            ["get_property", "set_property"]
        );
        Ok(())
    }

    #[test]
    fn invalid_session_identity_is_rejected() {
        for fields in [
            HashMap::new(),
            HashMap::from([
                ("user_id".into(), "1000".into()),
                ("pid".into(), "0".into()),
            ]),
            HashMap::from([("user_id".into(), "x".into()), ("pid".into(), "42".into())]),
        ] {
            assert!(session_owner(&fields).is_err());
        }
    }

    #[test]
    fn introspection_preserves_python_method_signatures() {
        struct InertBackend;
        impl Backend for InertBackend {
            fn start(&mut self, _fields: &HashMap<String, String>, _uid: u32) -> Result<()> {
                Ok(())
            }
            fn stop(&mut self) -> Result<()> {
                Ok(())
            }
            fn freeze(&mut self) -> Result<()> {
                Ok(())
            }
            fn unfreeze(&mut self) -> Result<()> {
                Ok(())
            }
            fn status(&mut self) -> Result<State> {
                Ok(State::Stopped)
            }
            fn get_property(&mut self, _key: &str) -> Result<String> {
                Ok(String::new())
            }
            fn set_property(&mut self, _key: &str, _value: &str) -> Result<()> {
                Ok(())
            }
        }
        let service =
            ContainerService::with_backend(Box::new(InertBackend), LauncherPolicy::production());
        let mut xml = String::new();
        service.introspect_to_writer(&mut xml, 0);
        for method in [
            "Start",
            "Stop",
            "Freeze",
            "Unfreeze",
            "GetSession",
            "GetProperty",
            "SetProperty",
        ] {
            assert!(
                xml.contains(&format!("name=\"{method}\"")),
                "missing {method}: {xml}"
            );
        }
        assert!(xml.contains("type=\"a{ss}\""));
        assert!(xml.contains("type=\"b\""));
    }
}
