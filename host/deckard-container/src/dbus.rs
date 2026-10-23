//! Uninstalled system-bus adapter for the installed ContainerManager contract.

use crate::command::ProcessRunner;
use crate::drivers::DriverNodes;
use crate::hardware::{self, HardwareRequest};
use crate::initializer_dbus::{InitializerService, OBJECT_PATH as INITIALIZER_PATH};
use crate::lxc::{State, validate_property_key, validate_property_value};
use crate::manager::ContainerManager;
use crate::properties;
use crate::session_config;
use crate::{Error, Result};
use deckard_launch::binder::BinderConfig;
use rustix::fd::OwnedFd;
use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use zbus::connection::{Builder, Connection};
use zbus::fdo::{self, DBusProxy};
use zbus::message::Header;

pub const BUS_NAME: &str = "id.waydro.Container";
pub const OBJECT_PATH: &str = "/ContainerManager";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Caller {
    pub uid: u32,
    pub pid: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SessionIdentity {
    uid: u32,
    pid: u32,
}

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

    fn permits(&self, caller: Caller, session: SessionIdentity) -> bool {
        if caller.uid != session.uid || caller.pid == session.pid {
            return false;
        }
        let Ok(executable) = fs::metadata(&self.executable) else {
            return false;
        };
        let Ok(executable_entry) = fs::symlink_metadata(&self.executable) else {
            return false;
        };
        if executable_entry.file_type().is_symlink()
            || !self.executable.starts_with(&self.trust_root)
            || self
                .executable
                .components()
                .any(|component| component == Component::ParentDir)
        {
            return false;
        }
        let mut directory = self.executable.parent();
        let mut trusted_directories = false;
        while let Some(path) = directory {
            let Ok(metadata) = fs::symlink_metadata(path) else {
                return false;
            };
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.uid() != self.executable_owner
                || metadata.mode() & 0o022 != 0
            {
                return false;
            }
            if path == self.trust_root {
                trusted_directories = true;
                break;
            }
            directory = path.parent();
        }
        if !trusted_directories
            || !executable.is_file()
            || executable.uid() != self.executable_owner
            || executable.mode() & 0o022 != 0
            || executable.mode() & 0o111 == 0
        {
            return false;
        }
        let process = self.proc_root.join(caller.pid.to_string());
        if !fs::metadata(&process).is_ok_and(|metadata| metadata.uid() == caller.uid) {
            return false;
        }
        let Ok(running_executable) = fs::metadata(process.join("exe")) else {
            return false;
        };
        if executable.dev() != running_executable.dev()
            || executable.ino() != running_executable.ino()
        {
            return false;
        }
        let root = format!(
            "/user.slice/user-{}.slice/user@{}.service/",
            session.uid, session.uid
        );
        let in_user_manager = |pid: u32| -> bool {
            let path = self.proc_root.join(pid.to_string()).join("cgroup");
            let Ok(mut file) = File::open(path) else {
                return false;
            };
            let mut cgroup = String::new();
            if file
                .by_ref()
                .take(16 * 1024 + 1)
                .read_to_string(&mut cgroup)
                .is_err()
                || cgroup.len() > 16 * 1024
            {
                return false;
            }
            cgroup.lines().any(|line| {
                line.strip_prefix("0::")
                    .is_some_and(|path| path.starts_with(&root))
            })
        };
        in_user_manager(session.pid) && in_user_manager(caller.pid)
    }
}

impl SessionIdentity {
    fn parse(fields: &HashMap<String, String>) -> fdo::Result<Self> {
        fn decimal(fields: &HashMap<String, String>, key: &str) -> fdo::Result<u32> {
            fields
                .get(key)
                .ok_or_else(|| fdo::Error::InvalidArgs(format!("missing {key}")))?
                .parse()
                .map_err(|_| fdo::Error::InvalidArgs(format!("invalid {key}")))
        }
        let identity = Self {
            uid: decimal(fields, "user_id")?,
            pid: decimal(fields, "pid")?,
        };
        if identity.pid <= 1 || identity.pid > i32::MAX as u32 {
            return Err(fdo::Error::InvalidArgs("invalid session pid".into()));
        }
        Ok(identity)
    }

    fn authorize_start(self, caller: Caller) -> fdo::Result<()> {
        if caller.uid == 0 || (caller.uid == self.uid && caller.pid == self.pid) {
            Ok(())
        } else {
            Err(fdo::Error::AccessDenied(
                "session UID/PID does not match D-Bus caller".into(),
            ))
        }
    }

    fn authorize_control(self, caller: Caller) -> fdo::Result<()> {
        self.authorize_start(caller)
    }
}

struct Session {
    fields: HashMap<String, String>,
    identity: SessionIdentity,
    pidfd: OwnedFd,
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
        Err(Error::Invalid("hardware service unavailable".into()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HardwareOutcome {
    Active,
    Stopped,
}

struct DirectBackend {
    manager: ContainerManager,
    hardware_config: Option<BinderConfig>,
    hardware_sender: SyncSender<HardwareRequest>,
    hardware_stop: Option<Arc<AtomicBool>>,
    suspend_stops: bool,
}

impl Backend for DirectBackend {
    fn start(&mut self, fields: &HashMap<String, String>, uid: u32) -> Result<()> {
        if self.manager.lxc.status(&mut ProcessRunner)? != State::Stopped {
            return Err(Error::Invalid(
                "container must be stopped before updating session files".into(),
            ));
        }
        if self.manager.lxc.name == "waydroid" {
            DriverNodes::from_config(&self.manager.mounts.work.join("waydroid.cfg"))?
                .prepare(std::path::Path::new("/dev"))?;
        }
        session_config::write(
            &self.manager.lxc.root.join(&self.manager.lxc.name),
            fields,
            uid,
            std::path::Path::new("/run/user"),
        )?;
        properties::write(&self.manager.mounts.work, fields, uid)?;
        self.manager.start(&mut ProcessRunner)?;
        if let Some(config) = self.hardware_config.clone() {
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = stop.clone();
            let sender = self.hardware_sender.clone();
            thread::spawn(move || {
                if let Err(error) = hardware::run(&config, sender, worker_stop) {
                    eprintln!("deckard-containerd: hardware service failed: {error}");
                }
            });
            self.hardware_stop = Some(stop);
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(stop) = self.hardware_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.manager.stop(&mut ProcessRunner)
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
                self.manager.lxc.stop(&mut ProcessRunner)?;
                self.manager.lxc.start(&mut ProcessRunner)?;
                Ok(HardwareOutcome::Active)
            }
            HardwareRequest::ShutdownRequest(reason) if reason.starts_with('1') => {
                self.manager.lxc.wait_for(
                    &mut ProcessRunner,
                    State::Stopped,
                    Duration::from_secs(30),
                )?;
                self.manager.lxc.start(&mut ProcessRunner)?;
                Ok(HardwareOutcome::Active)
            }
            HardwareRequest::ShutdownRequest(_) => {
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

#[derive(Clone)]
pub struct ContainerService {
    state: Arc<Mutex<ServiceState>>,
    launcher_policy: LauncherPolicy,
}

impl ContainerService {
    pub fn new(
        manager: ContainerManager,
        hardware_config: Option<BinderConfig>,
        hardware_sender: SyncSender<HardwareRequest>,
        suspend_stops: bool,
    ) -> Self {
        Self::with_backend(Box::new(DirectBackend {
            manager,
            hardware_config,
            hardware_sender,
            hardware_stop: None,
            suspend_stops,
        }))
    }

    fn with_backend(backend: Box<dyn Backend>) -> Self {
        Self::with_backend_and_policy(backend, LauncherPolicy::production())
    }

    fn with_backend_and_policy(backend: Box<dyn Backend>, launcher_policy: LauncherPolicy) -> Self {
        Self {
            state: Arc::new(Mutex::new(ServiceState {
                backend,
                session: None,
            })),
            launcher_policy,
        }
    }

    pub async fn shutdown(&self) -> fdo::Result<()> {
        let state = self.state.clone();
        blocking::unblock(move || {
            let mut state = state
                .lock()
                .map_err(|_| fdo::Error::Failed("state lock poisoned".into()))?;
            if state.session.is_none() {
                return Ok(());
            }
            state.backend.stop().map_err(service_error)?;
            if let Some(session) = state.session.take() {
                let _ = pidfd_send_signal(&session.pidfd, Signal::USR1);
            }
            Ok(())
        })
        .await
    }

    pub async fn handle_hardware_request(&self, request: HardwareRequest) -> fdo::Result<()> {
        let state = self.state.clone();
        blocking::unblock(move || {
            let mut state = state
                .lock()
                .map_err(|_| fdo::Error::Failed("state lock poisoned".into()))?;
            if state.session.is_none() {
                return Err(fdo::Error::Failed("no active container session".into()));
            }
            if state.backend.hardware(request).map_err(service_error)? == HardwareOutcome::Stopped
                && let Some(session) = state.session.take()
            {
                let _ = pidfd_send_signal(&session.pidfd, Signal::USR1);
            }
            Ok(())
        })
        .await
    }

    async fn start_authorized(
        &self,
        fields: HashMap<String, String>,
        caller: Caller,
    ) -> fdo::Result<()> {
        let identity = SessionIdentity::parse(&fields)?;
        identity.authorize_start(caller)?;
        let pid = Pid::from_raw(identity.pid as i32)
            .ok_or_else(|| fdo::Error::InvalidArgs("invalid session pid".into()))?;
        let pidfd = pidfd_open(pid, PidfdFlags::empty()).map_err(|error| {
            fdo::Error::InvalidArgs(format!("session process unavailable: {error}"))
        })?;
        let state = self.state.clone();
        blocking::unblock(move || {
            let mut state = state
                .lock()
                .map_err(|_| fdo::Error::Failed("state lock poisoned".into()))?;
            if state.session.is_some() {
                return Err(fdo::Error::Failed("already tracking a session".into()));
            }
            state
                .backend
                .start(&fields, identity.uid)
                .map_err(service_error)?;
            state.session = Some(Session {
                fields,
                identity,
                pidfd,
            });
            Ok(())
        })
        .await
    }

    async fn stop_authorized(&self, quit_session: bool, caller: Caller) -> fdo::Result<()> {
        let state = self.state.clone();
        blocking::unblock(move || {
            let mut state = state
                .lock()
                .map_err(|_| fdo::Error::Failed("state lock poisoned".into()))?;
            authorize_control(state.session.as_ref(), caller)?;
            state.backend.stop().map_err(service_error)?;
            if let Some(session) = state.session.take()
                && quit_session
            {
                // A pidfd cannot signal an unrelated process after PID reuse.
                let _ = pidfd_send_signal(&session.pidfd, Signal::USR1);
            }
            Ok(())
        })
        .await
    }

    async fn change_freeze(&self, freeze: bool, caller: Caller) -> fdo::Result<()> {
        let state = self.state.clone();
        let launcher_policy = self.launcher_policy.clone();
        blocking::unblock(move || {
            let mut state = state
                .lock()
                .map_err(|_| fdo::Error::Failed("state lock poisoned".into()))?;
            if freeze {
                authorize_control(state.session.as_ref(), caller)?;
            } else {
                authorize_session_read(state.session.as_ref(), caller, &launcher_policy)?;
            }
            if freeze {
                state.backend.freeze()
            } else {
                state.backend.unfreeze()
            }
            .map_err(service_error)
        })
        .await
    }

    async fn get_session_authorized(&self, caller: Caller) -> fdo::Result<HashMap<String, String>> {
        let state = self.state.clone();
        let launcher_policy = self.launcher_policy.clone();
        blocking::unblock(move || {
            let mut state = state
                .lock()
                .map_err(|_| fdo::Error::Failed("state lock poisoned".into()))?;
            let Some(session) = state.session.as_ref() else {
                return Ok(HashMap::new());
            };
            authorize_session_read(Some(session), caller, &launcher_policy)?;
            let mut fields = session.fields.clone();
            let status = state.backend.status().map_err(service_error)?;
            fields.insert("state".into(), format!("{status:?}").to_uppercase());
            Ok(fields)
        })
        .await
    }

    async fn get_property_authorized(&self, key: String, caller: Caller) -> fdo::Result<String> {
        authorize_root(caller)?;
        validate_property_key(&key).map_err(invalid_argument)?;
        let state = self.state.clone();
        blocking::unblock(move || {
            let mut state = state
                .lock()
                .map_err(|_| fdo::Error::Failed("state lock poisoned".into()))?;
            state.backend.get_property(&key).map_err(service_error)
        })
        .await
    }

    async fn set_property_authorized(
        &self,
        key: String,
        value: String,
        caller: Caller,
    ) -> fdo::Result<()> {
        authorize_root(caller)?;
        validate_property_key(&key).map_err(invalid_argument)?;
        validate_property_value(&value).map_err(invalid_argument)?;
        let state = self.state.clone();
        blocking::unblock(move || {
            let mut state = state
                .lock()
                .map_err(|_| fdo::Error::Failed("state lock poisoned".into()))?;
            state
                .backend
                .set_property(&key, &value)
                .map_err(service_error)
        })
        .await
    }
}

fn authorize_session_read(
    session: Option<&Session>,
    caller: Caller,
    launcher_policy: &LauncherPolicy,
) -> fdo::Result<()> {
    if caller.uid == 0 {
        return Ok(());
    }
    let session = session.ok_or_else(|| fdo::Error::AccessDenied("no session owner".into()))?;
    if session.identity.authorize_control(caller).is_ok()
        || launcher_policy.permits(caller, session.identity)
    {
        Ok(())
    } else {
        Err(fdo::Error::AccessDenied(
            "caller is neither session owner nor trusted session launcher".into(),
        ))
    }
}

fn authorize_root(caller: Caller) -> fdo::Result<()> {
    if caller.uid == 0 {
        Ok(())
    } else {
        Err(fdo::Error::AccessDenied(
            "property access requires root".into(),
        ))
    }
}

fn invalid_argument(error: Error) -> fdo::Error {
    fdo::Error::InvalidArgs(error.to_string())
}

fn authorize_control(session: Option<&Session>, caller: Caller) -> fdo::Result<()> {
    if caller.uid == 0 {
        return Ok(());
    }
    session
        .ok_or_else(|| fdo::Error::AccessDenied("no session owner".into()))?
        .identity
        .authorize_control(caller)
}

fn service_error(error: Error) -> fdo::Error {
    fdo::Error::Failed(error.to_string())
}

#[zbus::interface(name = "id.waydro.ContainerManager")]
impl ContainerService {
    async fn start(
        &self,
        session: HashMap<String, String>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = caller(conn, &header).await?;
        self.start_authorized(session, caller).await
    }

    async fn stop(
        &self,
        quit_session: bool,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = caller(conn, &header).await?;
        self.stop_authorized(quit_session, caller).await
    }

    async fn freeze(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = caller(conn, &header).await?;
        self.change_freeze(true, caller).await
    }

    async fn unfreeze(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = caller(conn, &header).await?;
        self.change_freeze(false, caller).await
    }

    async fn get_session(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<HashMap<String, String>> {
        let caller = caller(conn, &header).await?;
        self.get_session_authorized(caller).await
    }

    async fn get_property(
        &self,
        key: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<String> {
        let caller = caller(conn, &header).await?;
        self.get_property_authorized(key, caller).await
    }

    async fn set_property(
        &self,
        key: String,
        value: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = caller(conn, &header).await?;
        self.set_property_authorized(key, value, caller).await
    }
}

pub async fn serve_system_bus(
    manager: ContainerManager,
    hardware_config: Option<BinderConfig>,
    hardware_sender: SyncSender<HardwareRequest>,
    suspend_stops: bool,
) -> zbus::Result<(Connection, ContainerService)> {
    let service = ContainerService::new(manager, hardware_config, hardware_sender, suspend_stops);
    let connection = Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, service.clone())?
        .serve_at(INITIALIZER_PATH, InitializerService::new())?
        .build()
        .await?;
    Ok((connection, service))
}

pub(crate) async fn caller(conn: &Connection, header: &Header<'_>) -> fdo::Result<Caller> {
    let sender = header
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("missing D-Bus sender".into()))?;
    let name = zbus::names::BusName::try_from(sender.as_str())
        .map_err(|error| fdo::Error::AccessDenied(error.to_string()))?;
    let proxy = DBusProxy::new(conn)
        .await
        .map_err(|error| fdo::Error::AccessDenied(error.to_string()))?;
    let uid = proxy
        .get_connection_unix_user(name.clone())
        .await
        .map_err(|error| fdo::Error::AccessDenied(error.to_string()))?;
    let pid = proxy
        .get_connection_unix_process_id(name)
        .await
        .map_err(|error| fdo::Error::AccessDenied(error.to_string()))?;
    Ok(Caller { uid, pid })
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
                .map_err(|_| Error::Invalid("record lock poisoned".into()))?
                .push("start");
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            self.0
                .lock()
                .map_err(|_| Error::Invalid("record lock poisoned".into()))?
                .push("stop");
            Ok(())
        }
        fn freeze(&mut self) -> Result<()> {
            self.0
                .lock()
                .map_err(|_| Error::Invalid("record lock poisoned".into()))?
                .push("freeze");
            Ok(())
        }
        fn unfreeze(&mut self) -> Result<()> {
            self.0
                .lock()
                .map_err(|_| Error::Invalid("record lock poisoned".into()))?
                .push("unfreeze");
            Ok(())
        }
        fn status(&mut self) -> Result<State> {
            Ok(State::Running)
        }
        fn get_property(&mut self, key: &str) -> Result<String> {
            if key != "sys.boot_completed" {
                return Err(Error::Invalid("unexpected test property".into()));
            }
            self.0
                .lock()
                .map_err(|_| Error::Invalid("record lock poisoned".into()))?
                .push("get_property");
            Ok("1".into())
        }
        fn set_property(&mut self, key: &str, value: &str) -> Result<()> {
            if key != "persist.deckard.test" || value != "ready" {
                return Err(Error::Invalid("unexpected test property".into()));
            }
            self.0
                .lock()
                .map_err(|_| Error::Invalid("record lock poisoned".into()))?
                .push("set_property");
            Ok(())
        }
        fn hardware(&mut self, request: HardwareRequest) -> Result<HardwareOutcome> {
            if request != HardwareRequest::Suspend {
                return Err(Error::Invalid("unexpected hardware request".into()));
            }
            self.0
                .lock()
                .map_err(|_| Error::Invalid("record lock poisoned".into()))?
                .push("hardware_suspend");
            Ok(HardwareOutcome::Active)
        }
    }

    #[test]
    fn shutdown_does_not_stop_an_unowned_container() -> fdo::Result<()> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = ContainerService::with_backend(Box::new(RecordingBackend(calls.clone())));
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
        let service = ContainerService::with_backend(Box::new(RecordingBackend(calls.clone())));
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
    fn private_bus_exercises_sender_credentials_and_methods()
    -> std::result::Result<(), Box<dyn StdError>> {
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
        let service = ContainerService::with_backend(Box::new(RecordingBackend(calls.clone())));
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
    fn private_bus_allows_trusted_launcher_but_not_another_same_uid_process()
    -> std::result::Result<(), Box<dyn StdError>> {
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
            SessionIdentity {
                uid,
                pid: session_pid
            }
        ));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = ContainerService::with_backend_and_policy(
            Box::new(RecordingBackend(calls.clone())),
            policy,
        );
        let session_identity = SessionIdentity {
            uid,
            pid: session_pid,
        };
        let pidfd = pidfd_open(
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
            identity: session_identity,
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
            Ok::<(), Box<dyn StdError>>(())
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
        let identity = SessionIdentity::parse(&fields)?;
        assert!(
            identity
                .authorize_start(Caller {
                    uid: 1000,
                    pid: 2345
                })
                .is_ok()
        );
        assert!(
            identity
                .authorize_start(Caller {
                    uid: 1000,
                    pid: 2346
                })
                .is_err()
        );
        assert!(
            identity
                .authorize_start(Caller {
                    uid: 1001,
                    pid: 2345
                })
                .is_err()
        );
        assert!(
            identity
                .authorize_start(Caller { uid: 0, pid: 9999 })
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn native_property_path_requires_root_and_validates_inputs() -> fdo::Result<()> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = ContainerService::with_backend(Box::new(RecordingBackend(calls.clone())));
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
            assert!(SessionIdentity::parse(&fields).is_err());
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
        let service = ContainerService::with_backend(Box::new(InertBackend));
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
