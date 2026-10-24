//! Binder for the host binaries, over rsbinder: the `IPlatform` client and
//! the local services Android calls (`waydroidhardware`,
//! `waydroidusermonitor`). Requests carry libgbinder's `aidl3` header and use
//! Android 11's service-manager codes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use deckard_host_tools::cfg;
use deckard_host_tools::names::android;
use rsbinder::{
    Binder, DeathRecipient, Interface, Parcel, ProcessState, Remotable, SIBinder, StatusCode,
    WIBinder,
};

use crate::Result;

const PLATFORM: &str = android::PLATFORM.0;
const PLATFORM_INTERFACE: &str = android::PLATFORM.1;
const SERVICE_MANAGER: &str = "android.os.IServiceManager";
const CHECK_SERVICE: u32 = 2;
const ADD_SERVICE: u32 = 3;
const DUMP_FLAG_PRIORITY_DEFAULT: i32 = 8;
const STRICT_MODE_PENALTY_GATHER: i32 = 0x40 << 16;
const UNSET_WORK_SOURCE: i32 = -1;
const SYSTEM_HEADER: i32 = 0x5359_5354;
const MANAGER_TIMEOUT: Duration = Duration::from_secs(30);
const SERVICE_TIMEOUT: Duration = Duration::from_secs(180);
const TICK: Duration = Duration::from_millis(100);
const MAX_STRING_BYTES: usize = 4096;
const MAX_APPS: usize = 4096;
const MAX_CATEGORIES: usize = 64;

/// The Binder device of `waydroid.cfg`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinderConfig {
    pub device: PathBuf,
}

impl BinderConfig {
    /// Reads the `[waydroid]` Binder settings of `waydroid.cfg`.
    ///
    /// # Errors
    /// The file is unreadable, its device name is missing or unsafe, or it
    /// names a protocol other than `aidl3`.
    pub fn from_file(path: &Path) -> Result<Self> {
        Self::parse(&fs::read_to_string(path)?)
    }

    /// # Errors
    /// As for [`from_file`](Self::from_file).
    pub fn parse(text: &str) -> Result<Self> {
        let binder = cfg::get(text, "binder").ok_or("missing waydroid.binder")?;
        let safe = !binder.is_empty()
            && binder
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if !safe {
            return Err("invalid Binder device name".into());
        }
        for key in ["service_manager_protocol", "binder_protocol"] {
            if cfg::get(text, key).is_some_and(|protocol| protocol != "aidl3") {
                return Err(format!("unsupported {key}: only aidl3 is spoken").into());
            }
        }
        Ok(Self {
            device: Path::new("/dev").join(binder),
        })
    }
}

fn init(config: &BinderConfig) -> Result<()> {
    let device = config.device.to_str().ok_or("Binder device is not UTF-8")?;
    ProcessState::init(device, 1).map_err(|error| error.to_string())?;
    Ok(())
}

fn manager() -> Result<SIBinder> {
    Ok(ProcessState::as_self().context_object()?)
}

/// One two-way call; the reply is positioned after its exception code.
fn call(
    binder: &SIBinder,
    interface: &str,
    code: u32,
    write: impl FnOnce(&mut Parcel) -> rsbinder::Result<()>,
) -> Result<Parcel> {
    let proxy = binder.as_proxy().ok_or("Binder object is not remote")?;
    let mut data = Parcel::new();
    data.write(&STRICT_MODE_PENALTY_GATHER)?;
    data.write(&UNSET_WORK_SOURCE)?;
    data.write(&SYSTEM_HEADER)?;
    data.write(interface)?;
    write(&mut data)?;
    let mut reply = proxy
        .submit_transact(code, &data, 0)?
        .ok_or("Binder call returned no reply")?;
    let exception: i32 = reply.read()?;
    if exception != 0 {
        return Err(format!("{interface} call {code} raised exception {exception}").into());
    }
    Ok(reply)
}

fn check_service(name: &str) -> Result<Option<SIBinder>> {
    let mut reply = call(&manager()?, SERVICE_MANAGER, CHECK_SERVICE, |data| {
        data.write(name)
    })?;
    Ok(reply.read()?)
}

/// Whether the service manager on the configured device is up.
///
/// # Errors
/// The Binder device cannot be opened.
pub fn service_manager_present(config: &BinderConfig) -> Result<bool> {
    init(config)?;
    Ok(manager().is_ok())
}

/// A UTF-16 string of at most 4096 UTF-8 bytes.
///
/// # Errors
/// The string is missing, null, invalid or too long.
pub fn string(parcel: &mut Parcel) -> Result<String> {
    let text: String = parcel.read()?;
    if text.len() > MAX_STRING_BYTES {
        return Err("Binder string is too long".into());
    }
    Ok(text)
}

fn count(parcel: &mut Parcel, limit: usize) -> Result<usize> {
    let length: i32 = parcel.read()?;
    usize::try_from(length)
        .ok()
        .filter(|length| *length <= limit)
        .ok_or_else(|| "invalid Binder list length".into())
}

/// One launchable entry of `IPlatform.getAppsInfo` / `getAppInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    pub name: String,
    pub package: String,
    pub categories: Vec<String>,
}

fn read_app(parcel: &mut Parcel) -> Result<Option<AppInfo>> {
    if parcel.read::<i32>()? != 1 {
        return Ok(None);
    }
    let name = string(parcel)?;
    let package = string(parcel)?;
    // action, launchIntent, componentPackageName, componentClassName
    for _ in 0..4 {
        string(parcel)?;
    }
    let length = count(parcel, MAX_CATEGORIES)?;
    let mut categories = Vec::with_capacity(length);
    for _ in 0..length {
        categories.push(string(parcel)?);
    }
    Ok(Some(AppInfo {
        name,
        package,
        categories,
    }))
}

/// Client of Android's `waydroidplatform` service. Every call fails when
/// Binder or Android refuses it or the reply is malformed.
pub struct Platform(SIBinder);

impl Platform {
    /// Waits up to 30 seconds for the service manager and three minutes for
    /// `waydroidplatform`.
    ///
    /// # Errors
    /// Binder or the service is unavailable.
    pub fn connect(config: &BinderConfig) -> Result<Self> {
        init(config)?;
        let start = Instant::now();
        loop {
            match check_service(PLATFORM) {
                Ok(Some(binder)) => return Ok(Self(binder)),
                Err(error) if start.elapsed() >= MANAGER_TIMEOUT => {
                    return Err(format!("Binder service manager did not appear: {error}").into());
                }
                _ if start.elapsed() >= SERVICE_TIMEOUT => {
                    return Err(format!("{PLATFORM} unavailable").into());
                }
                _ => thread::sleep(Duration::from_millis(150)),
            }
        }
    }

    fn call(
        &self,
        code: u32,
        write: impl FnOnce(&mut Parcel) -> rsbinder::Result<()>,
    ) -> Result<Parcel> {
        call(&self.0, PLATFORM_INTERFACE, code, write)
    }

    pub fn getprop(&self, key: &str, fallback: &str) -> Result<String> {
        string(&mut self.call(1, |data| {
            data.write(key)?;
            data.write(fallback)
        })?)
    }

    pub fn setprop(&self, key: &str, value: &str) -> Result<()> {
        self.call(2, |data| {
            data.write(key)?;
            data.write(value)
        })
        .map(drop)
    }

    /// Every app Android lists; absent entries are skipped.
    pub fn apps(&self) -> Result<Vec<AppInfo>> {
        let mut reply = self.call(3, |_| Ok(()))?;
        let length = count(&mut reply, MAX_APPS)?;
        let mut apps = Vec::with_capacity(length);
        for _ in 0..length {
            apps.extend(read_app(&mut reply)?);
        }
        Ok(apps)
    }

    /// One package, or `None` when Android has no entry for it.
    pub fn app(&self, package: &str) -> Result<Option<AppInfo>> {
        read_app(&mut self.call(4, |data| data.write(package))?)
    }

    pub fn launch_app(&self, package: &str) -> Result<()> {
        self.call(7, |data| data.write(package)).map(drop)
    }

    pub fn settings_put_string(&self, table: i32, key: &str, value: &str) -> Result<()> {
        self.call(9, |data| {
            data.write(&table)?;
            data.write(key)?;
            data.write(value)
        })
        .map(drop)
    }

    /// The package that handles the intent, or empty.
    pub fn launch_intent(&self, action: &str, uri: &str) -> Result<String> {
        string(&mut self.call(13, |data| {
            data.write(action)?;
            data.write(uri)
        })?)
    }
}

/// What a local service answers to one transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// A reply parcel of these int32 words.
    Words(&'static [i32]),
    /// No reply parcel; the transaction fails with this status.
    Status(StatusCode),
}

/// A local service that Android calls.
pub trait Service: Send + Sync + 'static {
    const NAME: &'static str;
    const INTERFACE: &'static str;
    /// Handles one transaction on a Binder thread. It must return promptly:
    /// hand real work to another thread.
    fn transact(&self, code: u32, request: &mut Parcel) -> Reply;
}

struct Local<S> {
    service: S,
    stop: Arc<AtomicBool>,
}

impl<S: Service> Remotable for Local<S> {
    fn descriptor() -> &'static str {
        S::INTERFACE
    }

    fn on_transact(
        &self,
        code: u32,
        request: &mut Parcel,
        reply: &mut Parcel,
    ) -> rsbinder::Result<()> {
        if self.stop.load(Ordering::Acquire) {
            return Err(StatusCode::WouldBlock);
        }
        match self.service.transact(code, request) {
            Reply::Status(status) => Err(status),
            Reply::Words(words) => {
                for word in words {
                    reply.write(word)?;
                }
                Ok(())
            }
        }
    }

    fn on_dump(&self, _writer: &mut dyn Write, _args: &[String]) -> rsbinder::Result<()> {
        Ok(())
    }
}

struct Died(mpsc::Sender<()>);

impl DeathRecipient for Died {
    fn binder_died(&self, _who: &WIBinder) {
        let _ = self.0.send(());
    }
}

fn add_service(manager: &SIBinder, name: &str, service: &SIBinder) -> Result<()> {
    call(manager, SERVICE_MANAGER, ADD_SERVICE, |data| {
        data.write(name)?;
        data.write(service)?;
        data.write(&false)?;
        data.write(&DUMP_FLAG_PRIORITY_DEFAULT)
    })
    .map(drop)
}

/// Registers `service`, re-registering whenever the service manager comes
/// back, and serves it on a Binder thread. Once `stop` is set it answers
/// `WOULD_BLOCK`, and returns at the manager's next death.
///
/// # Errors
/// The Binder device cannot be opened.
pub fn serve<S: Service>(config: &BinderConfig, service: S, stop: Arc<AtomicBool>) -> Result<()> {
    init(config)?;
    ProcessState::start_thread_pool();
    let local = Local {
        service,
        stop: Arc::clone(&stop),
    };
    let binder = Binder::new(local).as_binder();
    let (sender, deaths) = mpsc::channel();
    let died = Arc::new(Died(sender));
    while !stop.load(Ordering::Acquire) {
        let Ok(manager) = manager() else {
            thread::sleep(TICK);
            continue;
        };
        if let Err(error) = manager.link_to_death_arc(&died) {
            eprintln!("{}: cannot watch the service manager: {error}", S::NAME);
        }
        if let Err(error) = add_service(&manager, S::NAME, &binder) {
            eprintln!("{}: registration failed: {error}", S::NAME);
        }
        let _ = deaths.recv();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_names_the_device_and_refuses_traversal_or_other_protocols() -> Result<()> {
        let config = BinderConfig::parse(
            "[waydroid]\nbinder = anbox-binder\nservice_manager_protocol = aidl3\n[properties]\nbinder = x\n",
        )?;
        assert_eq!(config.device, Path::new("/dev/anbox-binder"));
        assert!(BinderConfig::parse("[waydroid]\nbinder = ../mem\n").is_err());
        assert!(BinderConfig::parse("[waydroid]\nbinder_protocol = aidl3\n").is_err());
        assert!(BinderConfig::parse("[waydroid]\nbinder = b\nbinder_protocol = aidl4\n").is_err());
        Ok(())
    }

    fn parcel(write: impl FnOnce(&mut Parcel) -> rsbinder::Result<()>) -> Result<Parcel> {
        let mut parcel = Parcel::new();
        write(&mut parcel)?;
        parcel.set_data_position(0);
        Ok(parcel)
    }

    fn app(categories: i32) -> Result<Parcel> {
        parcel(|p| {
            p.write(&1i32)?;
            for text in ["Feeder", "org.example", "a", "b", "c", "d"] {
                p.write(text)?;
            }
            p.write(&categories)?;
            p.write("x")?;
            p.write("y")
        })
    }

    #[test]
    fn app_info_layout_matches_python_reader() -> Result<()> {
        let parsed = read_app(&mut app(2)?)?.ok_or("missing app")?;
        assert_eq!(parsed.package, "org.example");
        assert_eq!(parsed.categories, ["x", "y"]);
        assert_eq!(read_app(&mut parcel(|p| p.write(&0i32))?)?, None);
        assert!(read_app(&mut app(65)?).is_err());
        Ok(())
    }
}
