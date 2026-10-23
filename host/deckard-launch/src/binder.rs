use crate::Result;
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

const INTERFACE: &str = "lineageos.waydroid.IPlatform";
const SERVICE: &str = "waydroidplatform";
const SERVICE_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinderConfig {
    pub device: PathBuf,
    pub service_manager_protocol: Option<String>,
    pub binder_protocol: Option<String>,
}

impl BinderConfig {
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
        if binder.is_empty()
            || !binder
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err("invalid Binder device name".into());
        }
        for value in [manager.as_deref(), protocol.as_deref()]
            .into_iter()
            .flatten()
        {
            if value.is_empty()
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            {
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

#[repr(C)]
struct Writer([*const c_void; 4]);
#[repr(C)]
struct Reader([*const c_void; 6]);

type Ptr = *mut c_void;

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
    request_writer: unsafe extern "C" fn(Ptr, *mut Writer),
    append_string: unsafe extern "C" fn(*mut Writer, *const c_char),
    append_int: unsafe extern "C" fn(*mut Writer, u32),
    transact: unsafe extern "C" fn(Ptr, u32, Ptr, *mut c_int) -> Ptr,
    reply_unref: unsafe extern "C" fn(Ptr),
    reply_reader: unsafe extern "C" fn(Ptr, *mut Reader),
    read_int: unsafe extern "C" fn(*mut Reader, *mut i32) -> c_int,
    read_string: unsafe extern "C" fn(*mut Reader) -> *mut c_char,
    g_free: unsafe extern "C" fn(Ptr),
}

impl Api {
    fn load() -> Result<Self> {
        // Symbols are copied while each Library stays alive in this object.
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
                request_writer: symbol!(
                    "gbinder_local_request_init_writer",
                    unsafe extern "C" fn(Ptr, *mut Writer)
                ),
                append_string: symbol!(
                    "gbinder_writer_append_string16",
                    unsafe extern "C" fn(*mut Writer, *const c_char)
                ),
                append_int: symbol!(
                    "gbinder_writer_append_int32",
                    unsafe extern "C" fn(*mut Writer, u32)
                ),
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
            .as_ref()
            .map(|v| CString::new(v.as_str()))
            .transpose()?;
        let binder_protocol = config
            .binder_protocol
            .as_ref()
            .map(|v| CString::new(v.as_str()))
            .transpose()?;
        let protocol_ptr =
            |value: &Option<CString>| value.as_ref().map_or(ptr::null(), |v| v.as_ptr());
        // gbinder returns an autoreleased service object; take a reference before using it.
        unsafe {
            let manager = (api.sm_new)(
                device.as_ptr(),
                protocol_ptr(&manager_protocol),
                protocol_ptr(&binder_protocol),
            );
            if manager.is_null() {
                return Err("cannot open Binder service manager".into());
            }
            let mut platform = Self {
                api,
                manager,
                remote: ptr::null_mut(),
                client: ptr::null_mut(),
            };
            if (platform.api.sm_wait)(manager, 30_000) == 0 {
                return Err("Binder service manager did not appear".into());
            }
            let deadline = Instant::now() + SERVICE_TIMEOUT;
            let name = CString::new(SERVICE)?;
            loop {
                let mut status = 0;
                let remote = (platform.api.sm_get)(manager, name.as_ptr(), &mut status);
                if !remote.is_null() {
                    platform.remote = (platform.api.remote_ref)(remote);
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!("{SERVICE} unavailable (Binder status {status})").into());
                }
                thread::sleep(Duration::from_millis(150));
            }
            let interface = CString::new(INTERFACE)?;
            platform.client = (platform.api.client_new)(platform.remote, interface.as_ptr());
            if platform.client.is_null() {
                return Err("cannot open IPlatform Binder client".into());
            }
            Ok(platform)
        }
    }

    fn call(
        &self,
        code: u32,
        strings: &[&str],
        int: Option<i32>,
        reply_string: bool,
    ) -> Result<Option<String>> {
        unsafe {
            let request = (self.api.request_new)(self.client);
            if request.is_null() {
                return Err("cannot allocate Binder request".into());
            }
            struct RequestGuard<'a>(&'a Api, Ptr);
            impl Drop for RequestGuard<'_> {
                fn drop(&mut self) {
                    unsafe { (self.0.request_unref)(self.1) }
                }
            }
            let _request = RequestGuard(&self.api, request);
            let mut writer = Writer([ptr::null(); 4]);
            (self.api.request_writer)(request, &mut writer);
            if let Some(value) = int {
                (self.api.append_int)(&mut writer, value as u32);
            }
            for value in strings {
                let text = CString::new(*value)?;
                (self.api.append_string)(&mut writer, text.as_ptr());
            }
            let mut status = 0;
            let reply = (self.api.transact)(self.client, code, request, &mut status);
            if status != 0 || reply.is_null() {
                return Err(format!(
                    "IPlatform transaction {code} failed with transport status {status}"
                )
                .into());
            }
            struct ReplyGuard<'a>(&'a Api, Ptr);
            impl Drop for ReplyGuard<'_> {
                fn drop(&mut self) {
                    unsafe { (self.0.reply_unref)(self.1) }
                }
            }
            let _reply = ReplyGuard(&self.api, reply);
            let mut reader = Reader([ptr::null(); 6]);
            (self.api.reply_reader)(reply, &mut reader);
            let mut exception = 0;
            if (self.api.read_int)(&mut reader, &mut exception) == 0 {
                return Err(format!("IPlatform transaction {code} has no exception header").into());
            }
            if exception != 0 {
                return Err(
                    format!("IPlatform transaction {code} raised exception {exception}").into(),
                );
            }
            if reply_string {
                let value = (self.api.read_string)(&mut reader);
                if value.is_null() {
                    return Err(format!("IPlatform transaction {code} has no string reply").into());
                }
                let result = CStr::from_ptr(value).to_string_lossy().into_owned();
                (self.api.g_free)(value.cast());
                Ok(Some(result))
            } else {
                Ok(None)
            }
        }
    }

    pub fn getprop(&self, key: &str, fallback: &str) -> Result<String> {
        Ok(self
            .call(1, &[key, fallback], None, true)?
            .unwrap_or_default())
    }
    pub fn setprop(&self, key: &str, value: &str) -> Result<()> {
        self.call(2, &[key, value], None, false)?;
        Ok(())
    }
    pub fn launch_app(&self, package: &str) -> Result<()> {
        self.call(7, &[package], None, false)?;
        Ok(())
    }
    pub fn settings_put_string(&self, table: i32, key: &str, value: &str) -> Result<()> {
        self.call(9, &[key, value], Some(table), false)?;
        Ok(())
    }
    pub fn launch_intent(&self, action: &str, uri: &str) -> Result<String> {
        Ok(self
            .call(13, &[action, uri], None, true)?
            .unwrap_or_default())
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        unsafe {
            if !self.client.is_null() {
                (self.api.client_unref)(self.client)
            }
            if !self.remote.is_null() {
                (self.api.remote_unref)(self.remote)
            }
            if !self.manager.is_null() {
                (self.api.sm_unref)(self.manager)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_protocol_and_ignores_properties() {
        let cfg = BinderConfig::parse("[waydroid]\nbinder = anbox-binder\nservice_manager_protocol = aidl3\nbinder_protocol = aidl3\n[properties]\nbinder = malicious\n").unwrap();
        assert_eq!(cfg.device, Path::new("/dev/anbox-binder"));
        assert_eq!(cfg.binder_protocol.as_deref(), Some("aidl3"));
    }

    #[test]
    fn refuses_binder_path_traversal() {
        assert!(BinderConfig::parse("[waydroid]\nbinder = ../mem\n").is_err());
        assert!(BinderConfig::parse("[waydroid]\nbinder_protocol = aidl3\n").is_err());
    }
}
