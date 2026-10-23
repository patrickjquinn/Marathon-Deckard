use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use deckard_launch::binder::BinderConfig;
use libloading::Library;

use crate::{Result, invalid, unavailable};

const INTERFACE: &str = "lineageos.waydroid.IPlatform";
const SERVICE: &str = "waydroidplatform";
const GET_APPS_INFO: u32 = 3;
const MAX_APPS: i32 = 4096;
const MAX_CATEGORIES: i32 = 64;

type Ptr = *mut c_void;

#[repr(C)]
struct Reader([*const c_void; 6]);

pub struct AppInfo {
    pub name: String,
    pub package: String,
    pub categories: Vec<String>,
}

struct Api {
    _gbinder: Library,
    _glib: Library,
    sm_new: unsafe extern "C" fn(*const c_char, *const c_char, *const c_char) -> Ptr,
    sm_unref: unsafe extern "C" fn(Ptr),
    sm_wait: unsafe extern "C" fn(Ptr, i64) -> c_int,
    sm_get: unsafe extern "C" fn(Ptr, *const c_char, *mut c_int) -> Ptr,
    remote_ref: unsafe extern "C" fn(Ptr) -> Ptr,
    remote_unref: unsafe extern "C" fn(Ptr),
    client_new: unsafe extern "C" fn(Ptr, *const c_char) -> Ptr,
    client_unref: unsafe extern "C" fn(Ptr),
    request_new: unsafe extern "C" fn(Ptr) -> Ptr,
    request_unref: unsafe extern "C" fn(Ptr),
    transact: unsafe extern "C" fn(Ptr, u32, Ptr, *mut c_int) -> Ptr,
    reply_unref: unsafe extern "C" fn(Ptr),
    reply_reader: unsafe extern "C" fn(Ptr, *mut Reader),
    read_int: unsafe extern "C" fn(*mut Reader, *mut i32) -> c_int,
    read_string: unsafe extern "C" fn(*mut Reader) -> *mut c_char,
    g_free: unsafe extern "C" fn(Ptr),
}

impl Api {
    fn load() -> Result<Self> {
        // SAFETY: Symbol signatures match the installed libgbinder/GLib C API,
        // and both libraries remain owned by Api for the pointers' lifetime.
        unsafe {
            let gbinder = Library::new("libgbinder.so.1")?;
            let glib = Library::new("libglib-2.0.so.0")?;
            macro_rules! symbol {
                ($name:literal, $type:ty) => {
                    *gbinder.get::<$type>(concat!($name, "\0").as_bytes())?
                };
            }
            let g_free = *glib.get::<unsafe extern "C" fn(Ptr)>(b"g_free\0")?;
            Ok(Self {
                sm_new: symbol!(
                    "gbinder_servicemanager_new2",
                    unsafe extern "C" fn(*const c_char, *const c_char, *const c_char) -> Ptr
                ),
                sm_unref: symbol!("gbinder_servicemanager_unref", unsafe extern "C" fn(Ptr)),
                sm_wait: symbol!(
                    "gbinder_servicemanager_wait",
                    unsafe extern "C" fn(Ptr, i64) -> c_int
                ),
                sm_get: symbol!(
                    "gbinder_servicemanager_get_service_sync",
                    unsafe extern "C" fn(Ptr, *const c_char, *mut c_int) -> Ptr
                ),
                remote_ref: symbol!(
                    "gbinder_remote_object_ref",
                    unsafe extern "C" fn(Ptr) -> Ptr
                ),
                remote_unref: symbol!("gbinder_remote_object_unref", unsafe extern "C" fn(Ptr)),
                client_new: symbol!(
                    "gbinder_client_new",
                    unsafe extern "C" fn(Ptr, *const c_char) -> Ptr
                ),
                client_unref: symbol!("gbinder_client_unref", unsafe extern "C" fn(Ptr)),
                request_new: symbol!(
                    "gbinder_client_new_request",
                    unsafe extern "C" fn(Ptr) -> Ptr
                ),
                request_unref: symbol!("gbinder_local_request_unref", unsafe extern "C" fn(Ptr)),
                transact: symbol!(
                    "gbinder_client_transact_sync_reply",
                    unsafe extern "C" fn(Ptr, u32, Ptr, *mut c_int) -> Ptr
                ),
                reply_unref: symbol!("gbinder_remote_reply_unref", unsafe extern "C" fn(Ptr)),
                reply_reader: symbol!(
                    "gbinder_remote_reply_init_reader",
                    unsafe extern "C" fn(Ptr, *mut Reader)
                ),
                read_int: symbol!(
                    "gbinder_reader_read_int32",
                    unsafe extern "C" fn(*mut Reader, *mut i32) -> c_int
                ),
                read_string: symbol!(
                    "gbinder_reader_read_string16",
                    unsafe extern "C" fn(*mut Reader) -> *mut c_char
                ),
                g_free,
                _gbinder: gbinder,
                _glib: glib,
            })
        }
    }
}

pub struct Platform {
    api: Api,
    manager: Ptr,
    remote: Ptr,
    client: Ptr,
}

impl Platform {
    pub fn connect(config: &BinderConfig) -> Result<Self> {
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
        let ptr_of = |value: &Option<CString>| value.as_ref().map_or(ptr::null(), |v| v.as_ptr());
        // SAFETY: CStrings stay alive across each call. All non-null C handles
        // are retained and released by Platform::drop while Api is still live.
        unsafe {
            let manager = (api.sm_new)(
                device.as_ptr(),
                ptr_of(&manager_protocol),
                ptr_of(&binder_protocol),
            );
            if manager.is_null() {
                return Err(unavailable("cannot open Android Binder service manager"));
            }
            let mut platform = Self {
                api,
                manager,
                remote: ptr::null_mut(),
                client: ptr::null_mut(),
            };
            if (platform.api.sm_wait)(manager, 30_000) == 0 {
                return Err(unavailable("Android Binder service manager unavailable"));
            }
            let name = CString::new(SERVICE)?;
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                let mut status = 0;
                let remote = (platform.api.sm_get)(manager, name.as_ptr(), &mut status);
                if !remote.is_null() {
                    platform.remote = (platform.api.remote_ref)(remote);
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(unavailable(format!(
                        "{SERVICE} unavailable (Binder status {status})"
                    )));
                }
                thread::sleep(Duration::from_millis(250));
            }
            let interface = CString::new(INTERFACE)?;
            platform.client = (platform.api.client_new)(platform.remote, interface.as_ptr());
            if platform.client.is_null() {
                return Err(unavailable("cannot open IPlatform Binder client"));
            }
            Ok(platform)
        }
    }

    pub fn apps(&self) -> Result<Vec<AppInfo>> {
        // SAFETY: The request and reply are live for every gbinder call below.
        // The parcel layout matches IPlatform.getAppsInfo on the device.
        unsafe {
            let request = (self.api.request_new)(self.client);
            if request.is_null() {
                return Err(unavailable("cannot allocate Binder request"));
            }
            let mut status = 0;
            let reply = (self.api.transact)(self.client, GET_APPS_INFO, request, &mut status);
            (self.api.request_unref)(request);
            if status != 0 || reply.is_null() {
                return Err(unavailable(format!(
                    "getAppsInfo transport failure: {status}"
                )));
            }
            let parsed = (|| {
                let mut reader = Reader([ptr::null(); 6]);
                (self.api.reply_reader)(reply, &mut reader);
                let exception = self.read_int(&mut reader)?;
                if exception != 0 {
                    return Err(unavailable(format!("getAppsInfo exception: {exception}")));
                }
                let count = self.read_count(&mut reader, MAX_APPS)?;
                let mut apps = Vec::with_capacity(count);
                for _ in 0..count {
                    if self.read_int(&mut reader)? != 1 {
                        continue;
                    }
                    let name = self.read_string(&mut reader)?;
                    let package = self.read_string(&mut reader)?;
                    for _ in 0..4 {
                        self.read_string(&mut reader)?;
                    }
                    let categories_count = self.read_count(&mut reader, MAX_CATEGORIES)?;
                    let mut categories = Vec::with_capacity(categories_count);
                    for _ in 0..categories_count {
                        categories.push(self.read_string(&mut reader)?);
                    }
                    apps.push(AppInfo {
                        name,
                        package,
                        categories,
                    });
                }
                Ok(apps)
            })();
            (self.api.reply_unref)(reply);
            parsed
        }
    }

    fn read_int(&self, reader: &mut Reader) -> Result<i32> {
        let mut value = 0;
        // SAFETY: Reader was initialized from a live Binder reply; value is a
        // valid writable i32. False reports a truncated or invalid parcel.
        if unsafe { (self.api.read_int)(reader, &mut value) } == 0 {
            return Err(invalid("truncated IPlatform reply"));
        }
        Ok(value)
    }

    fn read_count(&self, reader: &mut Reader, limit: i32) -> Result<usize> {
        let value = self.read_int(reader)?;
        if !(0..=limit).contains(&value) {
            return Err(invalid("invalid IPlatform list count"));
        }
        Ok(value as usize)
    }

    fn read_string(&self, reader: &mut Reader) -> Result<String> {
        // SAFETY: Reader belongs to a live reply. A non-null returned string
        // is GLib-allocated and freed exactly once after copying its bytes.
        unsafe {
            let value = (self.api.read_string)(reader);
            if value.is_null() {
                return Err(invalid("truncated IPlatform string"));
            }
            let text = CStr::from_ptr(value).to_str().map(str::to_owned);
            (self.api.g_free)(value.cast());
            let text = text?;
            if text.len() > 4096 {
                return Err(invalid("IPlatform string is too long"));
            }
            Ok(text)
        }
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        // SAFETY: These are retained gbinder handles, each released once in
        // reverse order while the function pointers' libraries remain loaded.
        unsafe {
            if !self.client.is_null() {
                (self.api.client_unref)(self.client);
            }
            if !self.remote.is_null() {
                (self.api.remote_unref)(self.remote);
            }
            (self.api.sm_unref)(self.manager);
        }
    }
}
