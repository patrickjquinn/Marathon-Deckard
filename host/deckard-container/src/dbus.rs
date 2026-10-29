//! The system-bus `org.marathonos.Deckard.ContainerManager` contract (upstream
//! `tools/services/container_manager.py`) and its `/Initializer`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use async_channel::Sender;
use deckard_host_tools::{bus, cfg};
use deckard_launch::binder::BinderConfig;
use rustix::fd::OwnedFd;
use rustix::process::{Pid, PidfdFlags, Signal};
use zbus::connection::Connection;
use zbus::fdo::{self, DBusProxy};
use zbus::message::Header;
use zbus::names::BusName;

use crate::drivers::{self, DriverNodes};
use crate::hardware::{self, HardwareRequest};
use crate::initializer_dbus::{self, InitializerService};
use crate::lxc::{self, ContainerManager, State};
use crate::session_config::{self, Stage};
use crate::{Result, config, properties};

pub const BUS_NAME: &str = "org.marathonos.Deckard.Container";
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

struct Backend {
    manager: ContainerManager,
    hardware_sender: Sender<HardwareRequest>,
    hardware_stop: Option<Arc<AtomicBool>>,
    suspend_stops: bool,
    /// Android is stopped but its mounts and network are still up.
    teardown_pending: bool,
}

impl Backend {
    fn config_path(&self) -> PathBuf {
        self.manager.mounts.work.join("deckard.cfg")
    }

    /// Reads `deckard.cfg` and prepares the Binder and device nodes.
    fn prepare_production(&mut self) -> Result<()> {
        let base = self.manager.mounts.work.join("deckard_base.prop");
        let initialized = self.initialized() && base.is_file();
        crate::ensure(initialized, "Android is not initialized")?;
        let text = config::read(&self.config_path())?;
        let images = cfg::get(&text, "images_path")
            .map_or_else(|| self.manager.mounts.work.join("images"), PathBuf::from);
        crate::ensure(images.is_absolute(), "images_path must be absolute")?;
        self.manager.mounts.images = images;
        self.manager.mounts.overlays = match config::get_or_default(&text, "mount_overlays") {
            Some("True") => true,
            Some("False") => false,
            _ => return Err("invalid mount_overlays".into()),
        };
        self.suspend_stops = config::get_or_default(&text, "suspend_action") == Some("stop");
        let dev = Path::new("/dev");
        drivers::ensure_binder_nodes()?;
        DriverNodes::from_config(&self.config_path())?.prepare(dev)?;
        drivers::open_device_access(dev, Path::new("/sys/kernel/debug/sync/sw_sync"))
    }

    /// Whether the LXC instance exists, so that its status can be asked.
    fn initialized(&self) -> bool {
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

    fn start(&mut self, fields: &HashMap<String, String>, uid: u32) -> Result<()> {
        self.finish_stop()?;
        let stopped = self.manager.lxc.status()? == State::Stopped;
        crate::ensure(
            stopped,
            "container must be stopped before updating session files",
        )?;
        self.prepare_production()?;
        let stage = Stage::new(&self.manager.network.runtime);
        let instance = self.manager.lxc.root.join(&self.manager.lxc.name);
        session_config::write(&instance, fields, uid, Path::new("/run/user"), &stage)?;
        let started = properties::write(&self.manager.mounts.work, fields, uid)
            .and_then(|()| self.manager.start());
        if let Err(error) = started {
            if self.manager.lxc.status()? == State::Stopped {
                let _ = stage.release();
            }
            return Err(error);
        }
        self.start_hardware();
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.halt()?;
        self.finish_stop()
    }

    /// Stops Android, leaving its mounts and network to [`Self::finish_stop`].
    fn halt(&mut self) -> Result<()> {
        if let Some(stop) = self.hardware_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        let lxc = &self.manager.lxc;
        lxc.stop()?;
        crate::ensure(
            lxc.status()? == State::Stopped,
            "refusing to unmount a live container",
        )?;
        self.teardown_pending = true;
        Ok(())
    }

    /// Unmounting syncs the overlays' upper filesystem, all of /var: seconds
    /// after Android wrote a lot.
    fn finish_stop(&mut self) -> Result<()> {
        if !self.teardown_pending {
            return Ok(());
        }
        self.manager.mounts.unmount()?;
        self.manager.network.stop()?;
        Stage::new(&self.manager.network.runtime).release()?;
        self.teardown_pending = false;
        Ok(())
    }

    fn hardware(&mut self, request: HardwareRequest) -> Result<HardwareOutcome> {
        let lxc = &self.manager.lxc;
        match request {
            HardwareRequest::Suspend if self.suspend_stops => {
                self.stop()?;
                Ok(HardwareOutcome::Stopped)
            }
            HardwareRequest::Suspend => {
                lxc.freeze()?;
                Ok(HardwareOutcome::Active)
            }
            HardwareRequest::Reboot => {
                lxc.stop()?;
                lxc.start()?;
                Ok(HardwareOutcome::Active)
            }
            HardwareRequest::ShutdownRequest(reason) => {
                lxc.wait_for(State::Stopped, Duration::from_secs(30))?;
                if reason.starts_with('1') {
                    lxc.start()?;
                    return Ok(HardwareOutcome::Active);
                }
                self.stop()?;
                Ok(HardwareOutcome::Stopped)
            }
        }
    }
}

struct ServiceState {
    backend: Backend,
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
    pub fn new(manager: ContainerManager, hardware_sender: Sender<HardwareRequest>) -> Self {
        let backend = Backend {
            manager,
            hardware_sender,
            hardware_stop: None,
            suspend_stops: false,
            teardown_pending: false,
        };
        let state = ServiceState {
            backend,
            session: None,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            launcher: LauncherPolicy::production(),
        }
    }

    /// Runs `work` on the state off the bus's executor.
    async fn locked<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut ServiceState) -> fdo::Result<T> + Send + 'static,
    ) -> fdo::Result<T> {
        let state = Arc::clone(&self.state);
        bus::unblock(move || work(&mut *state.lock().map_err(poisoned)?)).await
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
        } else if state.backend.initialized()
            && state.backend.manager.lxc.status()? != State::Stopped
        {
            return Err("the container is running without a session; stop it first".into());
        }
        state.backend.finish_stop()?;
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

    /// Acts on one admitted `deckardhardware` request.
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
        self.locked(move |state| state.backend.manager.lxc.get_property(&key).map_err(failed))
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
        self.locked(move |state| {
            state
                .backend
                .manager
                .lxc
                .set_property(&key, &value)
                .map_err(failed)
        })
        .await
    }
}

#[zbus::interface(name = "org.marathonos.Deckard.ContainerManager")]
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
            state.backend.halt().map_err(failed)?;
            if let Some(session) = state.session.take()
                && quit_session
            {
                session.quit();
            }
            Ok(())
        })
        .await?;
        let state = Arc::clone(&self.state);
        thread::spawn(move || {
            let finished = match state.lock() {
                Ok(mut state) => state.backend.finish_stop(),
                Err(_) => Err("state lock poisoned".into()),
            };
            if let Err(error) = finished {
                eprintln!("deckard-containerd: cannot finish stopping Android: {error}");
            }
        });
        Ok(())
    }

    async fn freeze(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let caller = Caller::of(conn, &header).await?;
        self.locked(move |state| {
            state.authorize_control(caller)?;
            state.backend.manager.lxc.freeze().map_err(failed)
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
            state.backend.manager.lxc.unfreeze().map_err(failed)
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
            let status = state.backend.manager.lxc.status().map_err(failed)?;
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

/// Owns [`BUS_NAME`] and serves the manager and the initializer.
///
/// # Errors
/// The system bus or the name is unavailable.
pub async fn serve_system_bus(
    manager: ContainerManager,
    hardware_sender: Sender<HardwareRequest>,
) -> zbus::Result<(Connection, ContainerService)> {
    let service = ContainerService::new(manager, hardware_sender);
    let initializer = InitializerService::production(service.clone());
    let connection = zbus::connection::Builder::system()
        .name(BUS_NAME)
        .serve_at(OBJECT_PATH, service.clone())
        .serve_at(initializer_dbus::OBJECT_PATH, initializer)
        .build()
        .await?;
    Ok((connection, service))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};
    use zbus::object_server::Interface;

    use crate::mounts::MountLayout;
    use crate::network::Network;

    fn service() -> Result<ContainerService> {
        let manager = ContainerManager {
            lxc: lxc::Lxc::new("/nonexistent/lxc".into(), "deckard".into())?,
            mounts: MountLayout {
                work: "/nonexistent".into(),
                images: "/nonexistent/images".into(),
                overlays: true,
            },
            network: Network::new("deckard0".into(), "/nonexistent/run".into())?,
        };
        Ok(ContainerService::new(manager, async_channel::bounded(1).0))
    }

    #[test]
    fn hardware_request_requires_a_tracked_session() -> Result<()> {
        let service = service()?;
        let request = service.handle_hardware_request(HardwareRequest::Suspend);
        assert!(async_io::block_on(request).is_err());
        Ok(())
    }

    #[test]
    fn trusted_launcher_is_the_guarded_executable_in_the_users_manager() -> Result<()> {
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
        let mut session = Command::new("sleep").arg("30").spawn()?;
        let session_pid = session.id();
        let launcher_pid = std::process::id();
        let slice = format!("0::/user.slice/user-{uid}.slice/user@{uid}.service/app.slice");
        for (pid, unit) in [
            (session_pid, "deckard-session"),
            (launcher_pid, "marathon-shell"),
        ] {
            let process = proc_root.join(pid.to_string());
            fs::create_dir(&process)?;
            fs::write(process.join("cgroup"), format!("{slice}/{unit}.service\n"))?;
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
        let owner = Caller {
            uid,
            pid: session_pid,
        };
        let launcher = Caller {
            uid,
            pid: launcher_pid,
        };
        assert!(policy.permits(launcher, owner));
        assert!(!policy.permits(owner, owner));
        let _ = session.kill();
        let _ = session.wait();
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
    fn native_property_path_requires_root_and_validates_inputs() -> Result<()> {
        let service = service()?;
        let non_root = Caller {
            uid: 1000,
            pid: 123,
        };
        let root = Caller { uid: 0, pid: 1 };
        async_io::block_on(async {
            let key = || String::from("persist.deckard.test");
            assert!(
                service
                    .get_property_authorized(key(), non_root)
                    .await
                    .is_err()
            );
            assert!(
                service
                    .set_property_authorized(key(), "ready".into(), non_root)
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
                    .set_property_authorized(key(), "bad\nvalue".into(), root)
                    .await
                    .is_err()
            );
        });
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
    fn introspection_preserves_python_method_signatures() -> Result<()> {
        let mut xml = String::new();
        service()?.introspect_to_writer(&mut xml, 0);
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
        Ok(())
    }
}
