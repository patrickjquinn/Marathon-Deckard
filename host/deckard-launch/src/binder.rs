//! The one libgbinder binding the host binaries share: the `IPlatform` client
//! and the local services Android calls (`waydroidhardware`,
//! `waydroidusermonitor`). libgbinder and GLib are loaded at run time.

use crate::Result;
use libloading::Library;
use std::cell::Cell;
use std::ffi::{c_char, c_int, c_ulong, c_void, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const INTERFACE: &str = "lineageos.waydroid.IPlatform";
const SERVICE: &str = "waydroidplatform";
const SERVICE_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_STRING_BYTES: usize = 4096;
const MAX_APPS: usize = 4096;
const MAX_CATEGORIES: usize = 64;

/// Binder status: the request was malformed.
pub const BAD_VALUE: i32 = -22;
/// Binder status: the operation is not supported.
pub const INVALID_OPERATION: i32 = -38;
/// Binder status: the request was refused for now (queue full or stopping).
pub const WOULD_BLOCK: i32 = -11;
/// Binder status: the transaction code is unknown.
pub const UNKNOWN_TRANSACTION: i32 = -74;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinderConfig {
    pub device: PathBuf,
    pub service_manager_protocol: Option<String>,
    pub binder_protocol: Option<String>,
}

impl BinderConfig {
    /// Reads the `[waydroid]` Binder settings of `waydroid.cfg`.
    ///
    /// # Errors
    /// The file is unreadable or its Binder names are missing or unsafe.
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        Self::parse(&content)
    }

    pub fn parse(content: &str) -> Result<Self> {
        let mut section = "";
        let mut binder = None;
        let mut manager = None;
        let mut protocol = None;
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
                section = name;
            } else if section == "waydroid" {
                if let Some((key, value)) = line.split_once('=') {
                    let value = value.trim().to_owned();
                    match key.trim() {
                        "binder" => binder = Some(value),
                        "service_manager_protocol" => manager = Some(value),
                        "binder_protocol" => protocol = Some(value),
                        _ => {}
                    }
                }
            }
        }
        let binder = binder.ok_or("missing waydroid.binder")?;
        if !safe_name(&binder) {
            return Err("invalid Binder device name".into());
        }
        let protocols = [manager.as_deref(), protocol.as_deref()];
        for value in protocols {
            if value.is_some_and(|value| !safe_name(value)) {
                return Err("invalid Binder protocol name".into());
            }
        }
        Ok(Self {
            device: Path::new("/dev").join(binder),
            service_manager_protocol: manager,
            binder_protocol: protocol,
        })
    }
}

fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[repr(C)]
struct Writer([*const c_void; 4]);
#[repr(C)]
struct Reader([*const c_void; 6]);

type Ptr = *mut c_void;
type Transact = unsafe extern "C" fn(Ptr, Ptr, u32, u32, *mut c_int, Ptr) -> Ptr;
type Presence = unsafe extern "C" fn(Ptr, Ptr);
type SourceFunc = unsafe extern "C" fn(Ptr) -> c_int;

struct Api {
    _gbinder: Library,
    _glib: Library,
    sm_new: unsafe extern "C" fn(*const c_char, *const c_char, *const c_char) -> Ptr,
    sm_unref: unsafe extern "C" fn(Ptr),
    sm_wait: unsafe extern "C" fn(Ptr, i64) -> c_int,
    sm_present: unsafe extern "C" fn(Ptr) -> c_int,
    sm_get: unsafe extern "C" fn(Ptr, *const c_char, *mut c_int) -> Ptr,
    sm_object: unsafe extern "C" fn(Ptr, *const c_char, Transact, Ptr) -> Ptr,
    sm_add: unsafe extern "C" fn(Ptr, *const c_char, Ptr) -> c_int,
    sm_presence: unsafe extern "C" fn(Ptr, Presence, Ptr) -> c_ulong,
    sm_remove: unsafe extern "C" fn(Ptr, c_ulong),
    remote_ref: unsafe extern "C" fn(Ptr) -> Ptr,
    remote_unref: unsafe extern "C" fn(Ptr),
    client_new: unsafe extern "C" fn(Ptr, *const c_char) -> Ptr,
    client_unref: unsafe extern "C" fn(Ptr),
    request_new: unsafe extern "C" fn(Ptr) -> Ptr,
    request_unref: unsafe extern "C" fn(Ptr),
    request_writer: unsafe extern "C" fn(Ptr, *mut Writer),
    append_string: unsafe extern "C" fn(*mut Writer, *const c_char),
    append_int: unsafe extern "C" fn(*mut Writer, u32),
    transact: unsafe extern "C" fn(Ptr, u32, Ptr, *mut c_int) -> Ptr,
    reply_unref: unsafe extern "C" fn(Ptr),
    reply_reader: unsafe extern "C" fn(Ptr, *mut Reader),
    request_reader: unsafe extern "C" fn(Ptr, *mut Reader),
    object_drop: unsafe extern "C" fn(Ptr),
    new_reply: unsafe extern "C" fn(Ptr) -> Ptr,
    reply_append_int: unsafe extern "C" fn(Ptr, u32) -> Ptr,
    read_int: unsafe extern "C" fn(*mut Reader, *mut i32) -> c_int,
    read_long: unsafe extern "C" fn(*mut Reader, *mut i64) -> c_int,
    read_string: unsafe extern "C" fn(*mut Reader) -> *mut c_char,
    g_free: unsafe extern "C" fn(Ptr),
    loop_new: unsafe extern "C" fn(Ptr, c_int) -> Ptr,
    loop_run: unsafe extern "C" fn(Ptr),
    loop_quit: unsafe extern "C" fn(Ptr),
    loop_unref: unsafe extern "C" fn(Ptr),
    timeout_add: unsafe extern "C" fn(u32, SourceFunc, Ptr) -> u32,
    source_remove: unsafe extern "C" fn(u32) -> c_int,
}

impl Api {
    fn load() -> Result<Self> {
        // SAFETY: every signature below matches the libgbinder 1.1 and GLib
        // headers, and both libraries stay loaded as long as this Api lives.
        unsafe {
            let gbinder = Library::new("libgbinder.so.1")?;
            let glib = Library::new("libglib-2.0.so.0")?;
            macro_rules! gb {
                ($name:literal) => {
                    *gbinder.get(concat!($name, "\0").as_bytes())?
                };
            }
            macro_rules! gl {
                ($name:literal) => {
                    *glib.get(concat!($name, "\0").as_bytes())?
                };
            }
            Ok(Self {
                sm_new: gb!("gbinder_servicemanager_new2"),
                sm_unref: gb!("gbinder_servicemanager_unref"),
                sm_wait: gb!("gbinder_servicemanager_wait"),
                sm_present: gb!("gbinder_servicemanager_is_present"),
                sm_get: gb!("gbinder_servicemanager_get_service_sync"),
                sm_object: gb!("gbinder_servicemanager_new_local_object"),
                sm_add: gb!("gbinder_servicemanager_add_service_sync"),
                sm_presence: gb!("gbinder_servicemanager_add_presence_handler"),
                sm_remove: gb!("gbinder_servicemanager_remove_handler"),
                remote_ref: gb!("gbinder_remote_object_ref"),
                remote_unref: gb!("gbinder_remote_object_unref"),
                client_new: gb!("gbinder_client_new"),
                client_unref: gb!("gbinder_client_unref"),
                request_new: gb!("gbinder_client_new_request"),
                request_unref: gb!("gbinder_local_request_unref"),
                request_writer: gb!("gbinder_local_request_init_writer"),
                append_string: gb!("gbinder_writer_append_string16"),
                append_int: gb!("gbinder_writer_append_int32"),
                transact: gb!("gbinder_client_transact_sync_reply"),
                reply_unref: gb!("gbinder_remote_reply_unref"),
                reply_reader: gb!("gbinder_remote_reply_init_reader"),
                request_reader: gb!("gbinder_remote_request_init_reader"),
                object_drop: gb!("gbinder_local_object_drop"),
                new_reply: gb!("gbinder_local_object_new_reply"),
                reply_append_int: gb!("gbinder_local_reply_append_int32"),
                read_int: gb!("gbinder_reader_read_int32"),
                read_long: gb!("gbinder_reader_read_int64"),
                read_string: gb!("gbinder_reader_read_string16"),
                g_free: gl!("g_free"),
                loop_new: gl!("g_main_loop_new"),
                loop_run: gl!("g_main_loop_run"),
                loop_quit: gl!("g_main_loop_quit"),
                loop_unref: gl!("g_main_loop_unref"),
                timeout_add: gl!("g_timeout_add"),
                source_remove: gl!("g_source_remove"),
                _gbinder: gbinder,
                _glib: glib,
            })
        }
    }
}

/// A service manager on the configured Binder device.
struct Manager {
    api: Api,
    handle: Ptr,
}

impl Manager {
    fn open(config: &BinderConfig) -> Result<Self> {
        let api = Api::load()?;
        let device = CString::new(config.device.to_string_lossy().as_bytes())?;
        let manager_protocol = config
            .service_manager_protocol
            .as_deref()
            .map(CString::new)
            .transpose()?;
        let binder_protocol = config
            .binder_protocol
            .as_deref()
            .map(CString::new)
            .transpose()?;
        let as_ptr = |value: &Option<CString>| value.as_ref().map_or(ptr::null(), |v| v.as_ptr());
        // SAFETY: the C strings outlive the call; a null result is an error.
        let handle = unsafe {
            (api.sm_new)(
                device.as_ptr(),
                as_ptr(&manager_protocol),
                as_ptr(&binder_protocol),
            )
        };
        if handle.is_null() {
            return Err("cannot open Binder service manager".into());
        }
        Ok(Self { api, handle })
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        // SAFETY: handle came from gbinder_servicemanager_new2 and is released once.
        unsafe { (self.api.sm_unref)(self.handle) }
    }
}

/// Whether the service manager on the configured device is up, without
/// registering anything.
///
/// # Errors
/// libgbinder cannot be loaded or the device cannot be opened.
pub fn service_manager_present(config: &BinderConfig) -> Result<bool> {
    let manager = Manager::open(config)?;
    // SAFETY: handle is live for the call.
    Ok(unsafe { (manager.api.sm_present)(manager.handle) } != 0)
}

/// A Binder parcel being read: a request to a local service or a reply.
pub struct Parcel<'a> {
    api: &'a Api,
    reader: Reader,
}

/// The reading side of a parcel, so decoders can be tested without Binder.
pub trait ParcelRead {
    /// # Errors
    /// The parcel is truncated.
    fn int(&mut self) -> Result<i32>;
    /// # Errors
    /// The parcel is truncated.
    fn long(&mut self) -> Result<i64>;
    /// A UTF-16 string of at most 4096 UTF-8 bytes.
    ///
    /// # Errors
    /// The string is missing, null, invalid or too long.
    fn string(&mut self) -> Result<String>;
}

impl ParcelRead for Parcel<'_> {
    fn int(&mut self) -> Result<i32> {
        let mut value = 0;
        // SAFETY: reader was initialized from a parcel that outlives self.
        if unsafe { (self.api.read_int)(&mut self.reader, &mut value) } == 0 {
            return Err("truncated Binder parcel".into());
        }
        Ok(value)
    }

    fn long(&mut self) -> Result<i64> {
        let mut value = 0;
        // SAFETY: as for int.
        if unsafe { (self.api.read_long)(&mut self.reader, &mut value) } == 0 {
            return Err("truncated Binder parcel".into());
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<String> {
        // SAFETY: a non-null result is a GLib-allocated, NUL-terminated copy
        // that is freed exactly once after its bytes are copied.
        unsafe {
            let value = (self.api.read_string)(&mut self.reader);
            if value.is_null() {
                return Err("missing Binder string".into());
            }
            let text = CStr::from_ptr(value).to_str().map(str::to_owned);
            (self.api.g_free)(value.cast());
            let text = text?;
            if text.len() > MAX_STRING_BYTES {
                return Err("Binder string is too long".into());
            }
            Ok(text)
        }
    }
}

fn count(parcel: &mut impl ParcelRead, limit: usize) -> Result<usize> {
    usize::try_from(parcel.int()?)
        .ok()
        .filter(|count| *count <= limit)
        .ok_or_else(|| "invalid Binder list length".into())
}

/// One launchable entry of `IPlatform.getAppsInfo` / `getAppInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    pub name: String,
    pub package: String,
    pub categories: Vec<String>,
}

fn read_app(parcel: &mut impl ParcelRead) -> Result<Option<AppInfo>> {
    if parcel.int()? != 1 {
        return Ok(None);
    }
    let name = parcel.string()?;
    let package = parcel.string()?;
    // action, launchIntent, componentPackageName, componentClassName
    for _ in 0..4 {
        parcel.string()?;
    }
    let length = count(parcel, MAX_CATEGORIES)?;
    let mut categories = Vec::with_capacity(length);
    for _ in 0..length {
        categories.push(parcel.string()?);
    }
    Ok(Some(AppInfo {
        name,
        package,
        categories,
    }))
}

enum Arg<'a> {
    Int(i32),
    Str(&'a str),
}

/// Client of Android's `waydroidplatform` service.
pub struct Platform {
    manager: Manager,
    remote: Ptr,
    client: Ptr,
}

impl Platform {
    /// Waits up to three minutes for `waydroidplatform`.
    ///
    /// # Errors
    /// Binder or the service is unavailable.
    pub fn connect(config: &BinderConfig) -> Result<Self> {
        let manager = Manager::open(config)?;
        let mut platform = Self {
            manager,
            remote: ptr::null_mut(),
            client: ptr::null_mut(),
        };
        let handle = platform.manager.handle;
        // SAFETY: handle is live; the autoreleased remote object is referenced
        // before use and released in Drop, as is the client.
        unsafe {
            if (platform.manager.api.sm_wait)(handle, 30_000) == 0 {
                return Err("Binder service manager did not appear".into());
            }
            let deadline = Instant::now() + SERVICE_TIMEOUT;
            let name = CString::new(SERVICE)?;
            loop {
                let mut status = 0;
                let remote = (platform.manager.api.sm_get)(handle, name.as_ptr(), &mut status);
                if !remote.is_null() {
                    platform.remote = (platform.manager.api.remote_ref)(remote);
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!("{SERVICE} unavailable (Binder status {status})").into());
                }
                thread::sleep(Duration::from_millis(150));
            }
            let interface = CString::new(INTERFACE)?;
            platform.client =
                (platform.manager.api.client_new)(platform.remote, interface.as_ptr());
        }
        if platform.client.is_null() {
            return Err("cannot open IPlatform Binder client".into());
        }
        Ok(platform)
    }

    fn call<T>(
        &self,
        code: u32,
        args: &[Arg<'_>],
        read: impl FnOnce(&mut Parcel<'_>) -> Result<T>,
    ) -> Result<T> {
        let api = &self.manager.api;
        let mut strings = Vec::with_capacity(args.len());
        for arg in args {
            if let Arg::Str(value) = arg {
                strings.push(CString::new(*value)?);
            }
        }
        // SAFETY: request and reply are live until their guards drop; the
        // reader only borrows the reply, which outlives the parcel.
        unsafe {
            let request = (api.request_new)(self.client);
            if request.is_null() {
                return Err("cannot allocate Binder request".into());
            }
            let _request = Guard(api.request_unref, request);
            let mut writer = Writer([ptr::null(); 4]);
            (api.request_writer)(request, &mut writer);
            let mut next_string = strings.iter();
            for arg in args {
                match arg {
                    Arg::Int(value) => (api.append_int)(&mut writer, *value as u32),
                    Arg::Str(_) => {
                        let text = next_string.next().ok_or("Binder argument mismatch")?;
                        (api.append_string)(&mut writer, text.as_ptr());
                    }
                }
            }
            let mut status = 0;
            let reply = (api.transact)(self.client, code, request, &mut status);
            if status != 0 || reply.is_null() {
                return Err(format!(
                    "IPlatform transaction {code} failed with transport status {status}"
                )
                .into());
            }
            let _reply = Guard(api.reply_unref, reply);
            let mut parcel = Parcel {
                api,
                reader: Reader([ptr::null(); 6]),
            };
            (api.reply_reader)(reply, &mut parcel.reader);
            let exception = parcel.int()?;
            if exception != 0 {
                return Err(
                    format!("IPlatform transaction {code} raised exception {exception}").into(),
                );
            }
            read(&mut parcel)
        }
    }

    pub fn getprop(&self, key: &str, fallback: &str) -> Result<String> {
        self.call(1, &[Arg::Str(key), Arg::Str(fallback)], |parcel| {
            parcel.string()
        })
    }
    pub fn setprop(&self, key: &str, value: &str) -> Result<()> {
        self.call(2, &[Arg::Str(key), Arg::Str(value)], |_| Ok(()))
    }
    /// Every app Android lists; absent entries are skipped.
    pub fn apps(&self) -> Result<Vec<AppInfo>> {
        self.call(3, &[], |parcel| {
            let length = count(parcel, MAX_APPS)?;
            let mut apps = Vec::with_capacity(length);
            for _ in 0..length {
                if let Some(app) = read_app(parcel)? {
                    apps.push(app);
                }
            }
            Ok(apps)
        })
    }
    /// One package, or `None` when Android has no entry for it.
    pub fn app(&self, package: &str) -> Result<Option<AppInfo>> {
        self.call(4, &[Arg::Str(package)], |parcel| read_app(parcel))
    }
    pub fn launch_app(&self, package: &str) -> Result<()> {
        self.call(7, &[Arg::Str(package)], |_| Ok(()))
    }
    pub fn settings_put_string(&self, table: i32, key: &str, value: &str) -> Result<()> {
        self.call(
            9,
            &[Arg::Int(table), Arg::Str(key), Arg::Str(value)],
            |_| Ok(()),
        )
    }
    pub fn launch_intent(&self, action: &str, uri: &str) -> Result<String> {
        self.call(13, &[Arg::Str(action), Arg::Str(uri)], |parcel| {
            parcel.string()
        })
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        let api = &self.manager.api;
        // SAFETY: both handles were retained by connect and are released once,
        // before the manager and its libraries.
        unsafe {
            if !self.client.is_null() {
                (api.client_unref)(self.client);
            }
            if !self.remote.is_null() {
                (api.remote_unref)(self.remote);
            }
        }
    }
}

struct Guard(unsafe extern "C" fn(Ptr), Ptr);

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: the pointer is a live handle owned by this guard.
        unsafe { (self.0)(self.1) }
    }
}

/// What a local service answers to one transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// A reply parcel of these int32 words.
    Words(&'static [i32]),
    /// No reply parcel; the transaction fails with this Binder status.
    Status(i32),
}

/// Handles one transaction on the service's main-loop thread. It must return
/// promptly: hand real work to another thread.
pub type Handler = dyn Fn(u32, &mut Parcel<'_>) -> Reply + Send;

struct Service {
    manager: Manager,
    name: CString,
    object: Cell<Ptr>,
    main_loop: Ptr,
    stop: Arc<AtomicBool>,
    handler: Box<Handler>,
}

impl Service {
    fn register(&self) {
        let api = &self.manager.api;
        // SAFETY: manager and object stay live for the whole main loop.
        unsafe {
            if (api.sm_present)(self.manager.handle) != 0 {
                let status =
                    (api.sm_add)(self.manager.handle, self.name.as_ptr(), self.object.get());
                if status != 0 {
                    eprintln!(
                        "{}: registration failed: {status}",
                        self.name.to_string_lossy()
                    );
                }
            }
        }
    }
}

unsafe extern "C" fn on_presence(_manager: Ptr, data: Ptr) {
    // SAFETY: data is the boxed Service, live until the handler is removed.
    unsafe { (*(data as *const Service)).register() }
}

unsafe extern "C" fn on_tick(data: Ptr) -> c_int {
    // SAFETY: data is the boxed Service; the source is removed before it drops.
    let service = unsafe { &*(data as *const Service) };
    if service.stop.load(Ordering::Acquire) {
        // SAFETY: main_loop is live while the source exists.
        unsafe { (service.manager.api.loop_quit)(service.main_loop) };
    }
    1
}

unsafe extern "C" fn on_transaction(
    object: Ptr,
    request: Ptr,
    code: u32,
    _flags: u32,
    status: *mut c_int,
    data: Ptr,
) -> Ptr {
    if status.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: data is the boxed Service, live until the object is dropped;
    // libgbinder owns object, request and status for this call.
    unsafe {
        let service = &*(data as *const Service);
        let api = &service.manager.api;
        let answer = if service.stop.load(Ordering::Acquire) {
            Reply::Status(WOULD_BLOCK)
        } else {
            let mut parcel = Parcel {
                api,
                reader: Reader([ptr::null(); 6]),
            };
            (api.request_reader)(request, &mut parcel.reader);
            (service.handler)(code, &mut parcel)
        };
        match answer {
            Reply::Status(code) => {
                *status = code;
                ptr::null_mut()
            }
            Reply::Words(words) => {
                let reply = (api.new_reply)(object);
                if reply.is_null() {
                    *status = BAD_VALUE;
                    return ptr::null_mut();
                }
                for word in words {
                    (api.reply_append_int)(reply, *word as u32);
                }
                *status = 0;
                reply
            }
        }
    }
}

/// Registers `name` with `interface` on the service manager, re-registering
/// whenever the manager reappears, and runs a GLib main loop on the calling
/// thread until `stop` is set (checked every 100 ms).
///
/// # Errors
/// libgbinder, the service manager or the main loop cannot be set up.
pub fn serve(
    config: &BinderConfig,
    name: &str,
    interface: &str,
    handler: Box<Handler>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let manager = Manager::open(config)?;
    let name = CString::new(name)?;
    let interface = CString::new(interface)?;
    // SAFETY: a null context selects GLib's default main context.
    let main_loop = unsafe { (manager.api.loop_new)(ptr::null_mut(), 0) };
    if main_loop.is_null() {
        return Err("cannot create GLib main loop".into());
    }
    let service = Box::new(Service {
        manager,
        name,
        object: Cell::new(ptr::null_mut()),
        main_loop,
        stop,
        handler,
    });
    let data: Ptr = ptr::from_ref::<Service>(&service).cast_mut().cast();
    let api = &service.manager.api;
    let handle = service.manager.handle;
    // SAFETY: data points at the boxed Service, which is only read through
    // shared references and outlives the object, presence handler and timer,
    // all released below before it drops.
    unsafe {
        let object = (api.sm_object)(handle, interface.as_ptr(), on_transaction, data);
        if object.is_null() {
            (api.loop_unref)(main_loop);
            return Err("cannot create local Binder object".into());
        }
        service.object.set(object);
        let presence = (api.sm_presence)(handle, on_presence, data);
        let timer = if presence == 0 {
            0
        } else {
            (api.timeout_add)(100, on_tick, data)
        };
        if timer != 0 {
            service.register();
            (api.loop_run)(main_loop);
            (api.source_remove)(timer);
        }
        if presence != 0 {
            (api.sm_remove)(handle, presence);
        }
        (api.object_drop)(object);
        (api.loop_unref)(main_loop);
        if timer == 0 {
            return Err("cannot watch the Binder service manager".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_protocol_and_ignores_properties() -> Result<()> {
        let cfg = BinderConfig::parse("[waydroid]\nbinder = anbox-binder\nservice_manager_protocol = aidl3\nbinder_protocol = aidl3\n[properties]\nbinder = malicious\n")?;
        assert_eq!(cfg.device, Path::new("/dev/anbox-binder"));
        assert_eq!(cfg.binder_protocol.as_deref(), Some("aidl3"));
        Ok(())
    }

    #[test]
    fn refuses_binder_path_traversal() {
        assert!(BinderConfig::parse("[waydroid]\nbinder = ../mem\n").is_err());
        assert!(BinderConfig::parse("[waydroid]\nbinder_protocol = aidl3\n").is_err());
    }

    struct Values(std::collections::VecDeque<Result<String>>);

    impl ParcelRead for Values {
        fn int(&mut self) -> Result<i32> {
            Ok(self.string()?.parse()?)
        }
        fn long(&mut self) -> Result<i64> {
            Ok(self.string()?.parse()?)
        }
        fn string(&mut self) -> Result<String> {
            self.0
                .pop_front()
                .unwrap_or_else(|| Err("truncated".into()))
        }
    }

    fn values(items: &[&str]) -> Values {
        Values(items.iter().map(|item| Ok((*item).to_owned())).collect())
    }

    #[test]
    fn app_info_layout_matches_python_reader() -> Result<()> {
        let mut parcel = values(&[
            "1",
            "Feeder",
            "org.example",
            "a",
            "b",
            "c",
            "d",
            "2",
            "x",
            "y",
        ]);
        let app = read_app(&mut parcel)?.ok_or("missing app")?;
        assert_eq!(app.package, "org.example");
        assert_eq!(app.categories, ["x", "y"]);
        assert_eq!(read_app(&mut values(&["0"]))?, None);
        assert!(read_app(&mut values(&["1", "A", "p", "a", "b", "c", "d", "65"])).is_err());
        Ok(())
    }

    #[test]
    fn parcel_handles_match_libgbinder_abi() {
        assert_eq!(
            std::mem::size_of::<Reader>(),
            6 * std::mem::size_of::<Ptr>()
        );
        assert_eq!(
            std::mem::size_of::<Writer>(),
            4 * std::mem::size_of::<Ptr>()
        );
    }
}
