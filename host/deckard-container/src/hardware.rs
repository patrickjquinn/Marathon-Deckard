//! Root-side `lineageos.waydroid.IHardware` Binder endpoint. Not activated by this crate.
//!
//! `run` owns a GLib main loop and returns when `stop` becomes true. The supplied
//! callback runs on a separate worker; it must bound its own host operations.

use std::ffi::{CString, c_char, c_int, c_ulong, c_void};
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::time::Duration;

use crate::{Error, Result};

const INTERFACE: &[u8] = b"lineageos.waydroid.IHardware\0";
const SERVICE: &[u8] = b"waydroidhardware\0";
const MAX_STRING_UNITS: usize = 4096;
const QUEUE_DEPTH: usize = 16;
const BAD_VALUE: c_int = -22;
const WOULD_BLOCK: c_int = -11;
const UNKNOWN_TRANSACTION: c_int = -38;

fn reply_words(code: u32) -> &'static [u32] {
    const STANDARD: &[u32] = &[0];
    const TOGGLE: &[u32] = &[0, 0];
    if code <= 2 { TOGGLE } else { STANDARD }
}

type Ptr = *mut c_void;

#[repr(C)]
struct Reader([*const c_void; 6]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareRequest {
    EnableNfc(bool),
    EnableBluetooth(bool),
    Suspend,
    Reboot,
    Upgrade {
        system_zip: String,
        system_time: i64,
        vendor_zip: String,
        vendor_time: i64,
    },
    ShutdownRequest(String),
}

/// Host actions must be bounded and must not panic. Upgrade inputs are untrusted.
/// In particular, callers must validate paths before using them as image sources.
pub trait HardwareActions: Send + Sync + 'static {
    fn handle(&self, request: HardwareRequest);
}

impl<F: Fn(HardwareRequest) + Send + Sync + 'static> HardwareActions for F {
    fn handle(&self, request: HardwareRequest) {
        self(request);
    }
}

#[derive(Debug, Clone)]
pub struct HardwareConfig {
    pub binder_device: String,
    pub service_manager_protocol: Option<String>,
    pub binder_protocol: Option<String>,
}

struct Library(Ptr);

impl Library {
    fn open(name: &'static [u8]) -> Result<Self> {
        // SAFETY: name is a static NUL-terminated C string; dlopen owns the handle.
        let handle = unsafe { dlopen(name.as_ptr().cast(), 2) };
        if handle.is_null() {
            return Err(Error::Invalid(format!(
                "cannot load {}",
                String::from_utf8_lossy(name)
            )));
        }
        Ok(Self(handle))
    }

    unsafe fn symbol<T: Copy>(&self, name: &'static [u8]) -> Result<T> {
        // SAFETY: caller supplies the signature from the libgbinder/GLib header.
        let address = unsafe { dlsym(self.0, name.as_ptr().cast()) };
        if address.is_null() {
            return Err(Error::Invalid(format!(
                "missing symbol {}",
                String::from_utf8_lossy(name)
            )));
        }
        Ok(unsafe { std::mem::transmute_copy(&address) })
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by dlopen and is closed once.
        unsafe { dlclose(self.0) };
    }
}

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(name: *const c_char, flags: c_int) -> Ptr;
    fn dlsym(handle: Ptr, name: *const c_char) -> Ptr;
    fn dlclose(handle: Ptr) -> c_int;
}

type Transaction = unsafe extern "C" fn(Ptr, Ptr, u32, u32, *mut c_int, Ptr) -> Ptr;
type Presence = unsafe extern "C" fn(Ptr, Ptr);

struct Api {
    _gbinder: Library,
    _glib: Library,
    sm_new: unsafe extern "C" fn(*const c_char, *const c_char, *const c_char) -> Ptr,
    sm_unref: unsafe extern "C" fn(Ptr),
    sm_present: unsafe extern "C" fn(Ptr) -> c_int,
    sm_object: unsafe extern "C" fn(Ptr, *const c_char, Transaction, Ptr) -> Ptr,
    sm_add: unsafe extern "C" fn(Ptr, *const c_char, Ptr) -> c_int,
    sm_presence: unsafe extern "C" fn(Ptr, Presence, Ptr) -> c_ulong,
    sm_remove: unsafe extern "C" fn(Ptr, c_ulong),
    object_drop: unsafe extern "C" fn(Ptr),
    object_unref: unsafe extern "C" fn(Ptr),
    new_reply: unsafe extern "C" fn(Ptr) -> Ptr,
    reply_unref: unsafe extern "C" fn(Ptr),
    append_int: unsafe extern "C" fn(Ptr, u32) -> Ptr,
    request_reader: unsafe extern "C" fn(Ptr, *mut Reader),
    read_int: unsafe extern "C" fn(*mut Reader, *mut i32) -> c_int,
    read_long: unsafe extern "C" fn(*mut Reader, *mut i64) -> c_int,
    read_string: unsafe extern "C" fn(*mut Reader, *mut usize) -> *const u16,
    loop_new: unsafe extern "C" fn(Ptr, c_int) -> Ptr,
    loop_run: unsafe extern "C" fn(Ptr),
    loop_quit: unsafe extern "C" fn(Ptr),
    loop_unref: unsafe extern "C" fn(Ptr),
    timeout_add: unsafe extern "C" fn(u32, unsafe extern "C" fn(Ptr) -> c_int, Ptr) -> u32,
    source_remove: unsafe extern "C" fn(u32) -> c_int,
}

impl Api {
    fn load() -> Result<Self> {
        let gbinder = Library::open(b"libgbinder.so.1\0")?;
        let glib = Library::open(b"libglib-2.0.so.0\0")?;
        macro_rules! gb {
            ($name:literal) => {
                unsafe { gbinder.symbol(concat!($name, "\0").as_bytes())? }
            };
        }
        macro_rules! gl {
            ($name:literal) => {
                unsafe { glib.symbol(concat!($name, "\0").as_bytes())? }
            };
        }
        Ok(Self {
            sm_new: gb!("gbinder_servicemanager_new2"),
            sm_unref: gb!("gbinder_servicemanager_unref"),
            sm_present: gb!("gbinder_servicemanager_is_present"),
            sm_object: gb!("gbinder_servicemanager_new_local_object"),
            sm_add: gb!("gbinder_servicemanager_add_service_sync"),
            sm_presence: gb!("gbinder_servicemanager_add_presence_handler"),
            sm_remove: gb!("gbinder_servicemanager_remove_handler"),
            object_drop: gb!("gbinder_local_object_drop"),
            object_unref: gb!("gbinder_local_object_unref"),
            new_reply: gb!("gbinder_local_object_new_reply"),
            reply_unref: gb!("gbinder_local_reply_unref"),
            append_int: gb!("gbinder_local_reply_append_int32"),
            request_reader: gb!("gbinder_remote_request_init_reader"),
            read_int: gb!("gbinder_reader_read_int32"),
            read_long: gb!("gbinder_reader_read_int64"),
            read_string: gb!("gbinder_reader_read_string16_utf16"),
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

/// Check the installed Binder ABI and service-manager connection without
/// registering or replacing the live `waydroidhardware` service.
pub fn probe(config: &HardwareConfig) -> Result<bool> {
    let device = CString::new(config.binder_device.as_str())
        .map_err(|error| Error::Invalid(error.to_string()))?;
    if !Path::new(&config.binder_device).is_absolute() {
        return Err(Error::Invalid("Binder device must be absolute".into()));
    }
    let sm_protocol = config
        .service_manager_protocol
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|error| Error::Invalid(error.to_string()))?;
    let rpc_protocol = config
        .binder_protocol
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|error| Error::Invalid(error.to_string()))?;
    let api = Api::load()?;
    // SAFETY: the C strings stay alive through the call and manager is
    // unreferenced before unloading the library.
    unsafe {
        let manager = (api.sm_new)(
            device.as_ptr(),
            sm_protocol
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            rpc_protocol
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
        );
        if manager.is_null() {
            return Err(Error::Invalid(
                "cannot create Binder service manager".into(),
            ));
        }
        let present = (api.sm_present)(manager) != 0;
        (api.sm_unref)(manager);
        Ok(present)
    }
}

struct State {
    api: Api,
    manager: Ptr,
    object: Ptr,
    loop_: Ptr,
    stop: Arc<AtomicBool>,
    sender: SyncSender<HardwareRequest>,
}

impl State {
    unsafe fn register(&self) {
        // SAFETY: manager and object remain live for the full main-loop lifetime.
        if unsafe { (self.api.sm_present)(self.manager) } != 0 {
            let status =
                unsafe { (self.api.sm_add)(self.manager, SERVICE.as_ptr().cast(), self.object) };
            if status != 0 {
                eprintln!("waydroidhardware registration failed: {status}");
            }
        }
    }
}

unsafe extern "C" fn on_presence(_manager: Ptr, data: Ptr) {
    // SAFETY: data points to State until the handler is removed.
    unsafe { (&*(data as *const State)).register() };
}

unsafe extern "C" fn on_stop(data: Ptr) -> c_int {
    // SAFETY: source is removed before State is dropped.
    let state = unsafe { &*(data as *const State) };
    if state.stop.load(Ordering::Relaxed) {
        unsafe { (state.api.loop_quit)(state.loop_) };
    }
    1
}

unsafe fn string(reader: *mut Reader, api: &Api) -> Option<String> {
    let mut len = 0;
    let data = unsafe { (api.read_string)(reader, &mut len) };
    if data.is_null() || len > MAX_STRING_UNITS {
        return None;
    }
    // SAFETY: libgbinder supplies len UTF-16 units valid while the request lives.
    String::from_utf16(unsafe { std::slice::from_raw_parts(data, len) }).ok()
}

unsafe fn decode(code: u32, reader: *mut Reader, api: &Api) -> Option<HardwareRequest> {
    let mut int = 0;
    let mut time = 0;
    match code {
        1 | 2 => {
            if unsafe { (api.read_int)(reader, &mut int) } == 0 {
                return None;
            }
            Some(if code == 1 {
                HardwareRequest::EnableNfc(int != 0)
            } else {
                HardwareRequest::EnableBluetooth(int != 0)
            })
        }
        3 => Some(HardwareRequest::Suspend),
        4 => Some(HardwareRequest::Reboot),
        5 | 6 => {
            let system_zip = unsafe { string(reader, api) }?;
            if code == 5 {
                if unsafe { (api.read_int)(reader, &mut int) } == 0 {
                    return None;
                }
                time = i64::from(int);
            } else if unsafe { (api.read_long)(reader, &mut time) } == 0 {
                return None;
            }
            let vendor_zip = unsafe { string(reader, api) }?;
            let vendor_time = if code == 5 {
                if unsafe { (api.read_int)(reader, &mut int) } == 0 {
                    return None;
                }
                i64::from(int)
            } else {
                let mut value = 0;
                if unsafe { (api.read_long)(reader, &mut value) } == 0 {
                    return None;
                }
                value
            };
            Some(HardwareRequest::Upgrade {
                system_zip,
                system_time: time,
                vendor_zip,
                vendor_time,
            })
        }
        7 => Some(HardwareRequest::ShutdownRequest(unsafe {
            string(reader, api)
        }?)),
        _ => None,
    }
}

unsafe extern "C" fn on_transaction(
    object: Ptr,
    request: Ptr,
    code: u32,
    _flags: u32,
    status: *mut c_int,
    data: Ptr,
) -> Ptr {
    // SAFETY: libgbinder owns object/request/status and data remains live until object is unref'd.
    let state = unsafe { &*(data as *const State) };
    if status.is_null() {
        return ptr::null_mut();
    }
    if state.stop.load(Ordering::Relaxed) {
        unsafe { *status = WOULD_BLOCK };
        return ptr::null_mut();
    }
    if !(1..=7).contains(&code) {
        unsafe { *status = UNKNOWN_TRANSACTION };
        return ptr::null_mut();
    }
    let mut reader = Reader([ptr::null(); 6]);
    unsafe { (state.api.request_reader)(request, &mut reader) };
    let Some(message) = (unsafe { decode(code, &mut reader, &state.api) }) else {
        unsafe { *status = BAD_VALUE };
        return ptr::null_mut();
    };
    let reply = unsafe { (state.api.new_reply)(object) };
    if reply.is_null() {
        unsafe { *status = BAD_VALUE };
        return ptr::null_mut();
    }
    if state.sender.try_send(message).is_err() {
        unsafe {
            (state.api.reply_unref)(reply);
            *status = WOULD_BLOCK;
        }
        return ptr::null_mut();
    }
    for word in reply_words(code) {
        unsafe { (state.api.append_int)(reply, *word) };
    }
    unsafe { *status = 0 };
    reply
}

/// Run the service on the calling thread. `stop` is polled every 100 ms.
/// The callback queue holds at most 16 requests and rejects excess work with
/// `WOULD_BLOCK`; successful replies acknowledge admission, not completion.
pub fn run(
    config: &HardwareConfig,
    actions: impl HardwareActions,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let device =
        CString::new(config.binder_device.as_str()).map_err(|e| Error::Invalid(e.to_string()))?;
    if !Path::new(&config.binder_device).is_absolute() {
        return Err(Error::Invalid("Binder device must be absolute".into()));
    }
    let sm_protocol = config
        .service_manager_protocol
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let rpc_protocol = config
        .binder_protocol
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let api = Api::load()?;
    let (sender, receiver) = mpsc::sync_channel(QUEUE_DEPTH);
    let worker_stop = Arc::clone(&stop);
    let worker = std::thread::spawn(move || {
        while !worker_stop.load(Ordering::Relaxed) {
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(request) => {
                    if worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    actions.handle(request);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    // SAFETY: all C handles and callback data live until the main loop exits,
    // handlers are removed, and the object/manager have been unref'd.
    unsafe {
        let manager = (api.sm_new)(
            device.as_ptr(),
            sm_protocol.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            rpc_protocol.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        );
        if manager.is_null() {
            stop.store(true, Ordering::Relaxed);
            let _ = worker.join();
            return Err(Error::Invalid(
                "cannot create Binder service manager".into(),
            ));
        }
        let loop_ = (api.loop_new)(ptr::null_mut(), 0);
        if loop_.is_null() {
            (api.sm_unref)(manager);
            stop.store(true, Ordering::Relaxed);
            let _ = worker.join();
            return Err(Error::Invalid("cannot create GLib main loop".into()));
        }
        let mut state = Box::new(State {
            api,
            manager,
            object: ptr::null_mut(),
            loop_,
            stop,
            sender,
        });
        let data = (&mut *state as *mut State).cast();
        state.object =
            (state.api.sm_object)(manager, INTERFACE.as_ptr().cast(), on_transaction, data);
        if state.object.is_null() {
            (state.api.loop_unref)(loop_);
            (state.api.sm_unref)(manager);
            return Err(Error::Invalid("cannot create local Binder object".into()));
        }
        let presence = (state.api.sm_presence)(manager, on_presence, data);
        if presence == 0 {
            (state.api.object_unref)(state.object);
            (state.api.loop_unref)(loop_);
            (state.api.sm_unref)(manager);
            return Err(Error::Invalid("cannot observe Binder presence".into()));
        }
        state.register();
        let timer = (state.api.timeout_add)(100, on_stop, data);
        if timer == 0 {
            (state.api.sm_remove)(manager, presence);
            (state.api.object_unref)(state.object);
            (state.api.loop_unref)(loop_);
            (state.api.sm_unref)(manager);
            return Err(Error::Invalid("cannot create shutdown timer".into()));
        }
        (state.api.loop_run)(loop_);
        (state.api.source_remove)(timer);
        (state.api.sm_remove)(manager, presence);
        (state.api.object_drop)(state.object);
        (state.api.object_unref)(state.object);
        (state.api.loop_unref)(loop_);
        (state.api.sm_unref)(manager);
        // Worker is deliberately detached: a stalled host callback must not
        // prevent service shutdown. It sees stop before taking further work.
        drop(worker);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_codes_match_python_contract() {
        assert_eq!(
            std::mem::size_of::<Reader>(),
            6 * std::mem::size_of::<Ptr>()
        );
        assert_eq!(QUEUE_DEPTH, 16);
        assert_eq!(SERVICE, b"waydroidhardware\0");
        assert_eq!(INTERFACE, b"lineageos.waydroid.IHardware\0");
        assert_eq!(reply_words(1), &[0, 0]);
        assert_eq!(reply_words(2), &[0, 0]);
        for code in 3..=7 {
            assert_eq!(reply_words(code), &[0]);
        }
    }
}
