//! Shell-only share D-Bus API and authenticated DeckardSystem replies.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_io::Timer;
use base64::{Engine, engine::general_purpose::STANDARD};
use deckard_core::ids::PackageName;
use deckard_core::validate::{base64_png, clean_text};
use futures_lite::future;
use rustix::fs::{Mode, OFlags};
use serde_json::{Map, Value, json};
use zbus::message::{Header, Message};
use zbus::names::ErrorName;
use zbus::zvariant::{Fd, OwnedValue, Str, Value as Variant};
use zbus::{DBusError, fdo, interface};

pub const INTERFACE: &str = "org.marathonos.Deckard1.Share";
pub const OBJECT_PATH: &str = "/org/marathonos/Deckard";
const MAX_RESOURCES: usize = 32;
const MAX_TARGETS: usize = 64;
const MAX_TEXT: usize = 65_536;
const MAX_BYTES: u64 = 1 << 30;
const MAX_PENDING: usize = 64;
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum ShareError {
    Standard(fdo::Error),
    Failed(String),
}
impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Standard(e) => e.fmt(f),
            Self::Failed(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for ShareError {}
impl DBusError for ShareError {
    fn name(&self) -> ErrorName<'_> {
        match self {
            Self::Standard(e) => e.name(),
            Self::Failed(_) => ErrorName::try_from("org.marathonos.Deckard1.Error.Failed")
                .unwrap_or_else(|_| unreachable!()),
        }
    }
    fn description(&self) -> Option<&str> {
        match self {
            Self::Standard(e) => e.description(),
            Self::Failed(s) => Some(s),
        }
    }
    fn create_reply(&self, call: &Header<'_>) -> zbus::Result<Message> {
        match self {
            Self::Standard(e) => e.create_reply(call),
            Self::Failed(s) => Message::error(call, self.name())?.build(&(s,)),
        }
    }
}
fn denied() -> ShareError {
    ShareError::Standard(fdo::Error::AccessDenied("only the shell shares".into()))
}
fn invalid(message: &str) -> ShareError {
    ShareError::Standard(fdo::Error::InvalidArgs(message.into()))
}

pub type VariantMap = HashMap<String, OwnedValue>;
pub type SendSystem = dyn Fn(Map<String, Value>) -> bool + Send + Sync;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Targets,
    Share,
}
struct Pending {
    generation: u64,
    kind: Kind,
    answer: async_channel::Sender<Result<Value, String>>,
}
struct State {
    generation: Option<u64>,
    pending: HashMap<String, Pending>,
    next: u64,
}

#[derive(Clone)]
pub struct Sharing {
    bus: zbus::Connection,
    send: Arc<SendSystem>,
    xfer: PathBuf,
    state: Arc<Mutex<State>>,
}

impl Sharing {
    pub fn new(bus: zbus::Connection, send: Arc<SendSystem>, xfer: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&xfer)?;
        Ok(Self {
            bus,
            send,
            xfer,
            state: Arc::new(Mutex::new(State {
                generation: None,
                pending: HashMap::new(),
                next: 0,
            })),
        })
    }
    pub async fn register(&self) -> zbus::Result<()> {
        self.bus
            .object_server()
            .at(OBJECT_PATH, self.clone())
            .await
            .map(|_| ())
    }
    pub fn connected(&self, generation: u64) {
        self.system_lost();
        if let Ok(mut state) = self.state.lock() {
            state.generation = Some(generation);
        }
    }
    pub fn system_lost(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.generation = None;
            for (_, pending) in state.pending.drain() {
                let _ = pending.answer.try_send(Err("Android stopped".into()));
            }
        }
    }
    /// Invoke only after the system socket has checked SO_PEERCRED.
    pub fn handle_authenticated(&self, generation: u64, message: &Map<String, Value>) {
        let kind = field(message, "t");
        if kind == "share_out" {
            if self.state.lock().ok().and_then(|s| s.generation) == Some(generation) {
                self.share_out(message);
            }
            return;
        }
        let id = field(message, "id");
        if !request_id(id) {
            return;
        }
        let pending = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let expected = match kind {
                "share_targets_result" => Kind::Targets,
                "shared" => Kind::Share,
                _ => return,
            };
            if state.generation != Some(generation)
                || state
                    .pending
                    .get(id)
                    .is_none_or(|p| p.kind != expected || p.generation != generation)
            {
                return;
            }
            state.pending.remove(id)
        };
        if let Some(pending) = pending {
            let result = match kind {
                "share_targets_result" => Ok(Value::Array(targets(message))),
                "shared" => {
                    if message.get("ok").and_then(Value::as_bool) == Some(true) {
                        Ok(json!(null))
                    } else {
                        Err(clean_text(field(message, "error"), 512))
                    }
                }
                _ => return,
            };
            let _ = pending.answer.try_send(result);
        }
    }

    async fn shell(&self, header: Header<'_>) -> Result<(), ShareError> {
        let sender = header.sender().ok_or_else(denied)?.to_string();
        let check = async {
            let proxy = zbus::Proxy::new(
                &self.bus,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )
            .await?;
            let caller: u32 = proxy
                .call("GetConnectionUnixProcessID", &(sender.as_str(),))
                .await?;
            let shell: u32 = proxy
                .call("GetConnectionUnixProcessID", &("org.marathonos.Shell",))
                .await?;
            Ok::<bool, zbus::Error>(caller != 0 && caller == shell)
        };
        let allowed = future::or(async { check.await.unwrap_or(false) }, async {
            Timer::after(Duration::from_secs(5)).await;
            false
        })
        .await;
        if allowed { Ok(()) } else { Err(denied()) }
    }

    async fn request(
        &self,
        kind: Kind,
        mut message: Map<String, Value>,
        requested_id: Option<String>,
    ) -> Result<Value, ShareError> {
        let (tx, rx) = async_channel::bounded(1);
        let id = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| ShareError::Failed("share state failed".into()))?;
            let generation = state.generation;
            if generation.is_none() {
                if kind == Kind::Targets {
                    return Ok(json!([]));
                }
                return Err(ShareError::Failed("Android isn't running".into()));
            }
            if state.pending.len() >= MAX_PENDING {
                return Err(ShareError::Failed("too many pending shares".into()));
            }
            let id = if let Some(id) = requested_id {
                id
            } else {
                let id = format!("{:016x}", state.next);
                state.next = state.next.wrapping_add(1);
                id
            };
            if state.pending.contains_key(&id) {
                return Err(ShareError::Failed("duplicate share ID".into()));
            }
            state.pending.insert(
                id.clone(),
                Pending {
                    generation: generation.unwrap_or(0),
                    kind,
                    answer: tx,
                },
            );
            id
        };
        message.insert("id".into(), json!(id));
        if !(self.send)(message) {
            self.state
                .lock()
                .ok()
                .and_then(|mut s| s.pending.remove(&id));
            return if kind == Kind::Targets {
                Ok(json!([]))
            } else {
                Err(ShareError::Failed("Android isn't running".into()))
            };
        }
        let answer = future::or(async { rx.recv().await.ok() }, async {
            Timer::after(TIMEOUT).await;
            None
        })
        .await;
        self.state
            .lock()
            .ok()
            .and_then(|mut s| s.pending.remove(&id));
        match answer {
            Some(Ok(value)) => Ok(value),
            Some(Err(error)) if kind == Kind::Targets => {
                let _ = error;
                Ok(json!([]))
            }
            Some(Err(error)) => Err(ShareError::Failed(error)),
            None if kind == Kind::Targets => Ok(json!([])),
            None => Err(ShareError::Failed("Android didn't answer".into())),
        }
    }

    fn share_out(&self, message: &Map<String, Value>) {
        let id = field(message, "id");
        let pkg = field(message, "pkg");
        if !request_id(id) || PackageName::try_from(pkg).is_err() {
            return;
        }
        let dir = match open_dir(&self.xfer)
            .and_then(|root| open_child(&root, id, OFlags::RDONLY | OFlags::DIRECTORY))
        {
            Ok(dir) => dir,
            Err(_) => return,
        };
        let mut resources = Vec::<VariantMap>::new();
        let mut total = 0_u64;
        if let Some(files) = message.get("files").and_then(Value::as_array) {
            for name in files.iter().filter_map(Value::as_str).take(MAX_RESOURCES) {
                if !simple_name(name) {
                    continue;
                }
                let Ok(fd) = open_child(&dir, name, OFlags::RDONLY) else {
                    continue;
                };
                let file = File::from(fd);
                let Ok(meta) = file.metadata() else {
                    continue;
                };
                if !meta.is_file() || meta.len() > MAX_BYTES.saturating_sub(total) {
                    continue;
                }
                total += meta.len();
                let mut item = VariantMap::new();
                item.insert("name".into(), Str::from(name).into());
                let fd: OwnedFd = file.into();
                if let Ok(value) = OwnedValue::try_from(Variant::from(Fd::from(fd))) {
                    item.insert("fileDescriptor".into(), value);
                } else {
                    continue;
                }
                resources.push(item);
            }
        }
        let title = clean_text(field(message, "title"), 256);
        let text = clean_text(field(message, "text"), MAX_TEXT);
        if !text.is_empty() && resources.len() < MAX_RESOURCES {
            let mut item = VariantMap::new();
            item.insert("name".into(), Str::from(title.as_str()).into());
            item.insert("data".into(), Str::from(text.as_str()).into());
            resources.push(item);
        }
        if resources.is_empty() {
            return;
        }
        let mime = field(message, "mime");
        let mime = if mime_type(mime) { mime } else { "" };
        let mut config = VariantMap::new();
        if let Ok(value) = OwnedValue::try_from(Variant::from(resources)) {
            config.insert("resources".into(), value);
        } else {
            return;
        }
        config.insert("mimeType".into(), Str::from(mime).into());
        config.insert("title".into(), Str::from(title.as_str()).into());
        config.insert(
            "source".into(),
            Str::from(format!("waydroid.{pkg}").as_str()).into(),
        );
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            async_io::block_on(async move {
                if let Ok(proxy) =
                    zbus::Proxy::new(&bus, "org.sailfishos.share", "/", "org.sailfishos.share")
                        .await
                {
                    let _: zbus::Result<()> = proxy.call("share", &(config,)).await;
                }
            })
        });
    }
}

#[interface(name = "org.marathonos.Deckard1.Share")]
impl Sharing {
    #[zbus(name = "ShareTargets")]
    async fn share_targets(
        &self,
        mime_type: String,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<Vec<VariantMap>, ShareError> {
        self.shell(header).await?;
        if !self::mime_type(&mime_type) {
            return Err(invalid("not a MIME type"));
        }
        let value = self
            .request(
                Kind::Targets,
                object(json!({"t":"share_targets","mime":mime_type})),
                None,
            )
            .await?;
        Ok(value
            .as_array()
            .map(|items| items.iter().filter_map(target_map).collect())
            .unwrap_or_default())
    }
    #[zbus(name = "Share")]
    async fn share(
        &self,
        id: String,
        configuration: VariantMap,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ShareError> {
        self.shell(header).await?;
        if !target_id(&id) {
            return Err(invalid("invalid share target"));
        }
        let resources = configuration
            .get("resources")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| Vec::<VariantMap>::try_from(v).ok())
            .ok_or_else(|| invalid("resources required"))?;
        if resources.is_empty() || resources.len() > MAX_RESOURCES {
            return Err(invalid("a target and 1 to 32 resources"));
        }
        let mut mime = configuration
            .get("mimeType")
            .and_then(value_string)
            .unwrap_or_default();
        if !mime.is_empty() && !mime_type(&mime) {
            return Err(invalid("not a MIME type"));
        }
        let title = configuration
            .get("title")
            .and_then(value_string)
            .unwrap_or_default();
        let tid = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| ShareError::Failed("share state failed".into()))?;
            let id = format!("{:016x}", state.next);
            state.next = state.next.wrapping_add(1);
            id
        };
        let dir =
            create_transfer(&self.xfer, &tid).map_err(|e| ShareError::Failed(e.to_string()))?;
        let (copied_tx, copied_rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = copy_resources(&dir, &resources);
            let _ = copied_tx.send_blocking(result);
        });
        let result = copied_rx
            .recv()
            .await
            .map_err(|_| ShareError::Failed("share copy stopped".into()))?;
        let (files, text) = match result {
            Ok(v) => v,
            Err(error) => {
                let _ = fs::remove_dir_all(self.xfer.join(&tid));
                return Err(invalid(&error.to_string()));
            }
        };
        if mime.is_empty() {
            mime = if files.is_empty() {
                "text/plain".into()
            } else {
                "application/octet-stream".into()
            };
        }
        let pkg = id.split('/').next().unwrap_or("");
        if let Ok(proxy) = zbus::Proxy::new(
            &self.bus,
            "org.marathonos.Shell",
            "/org/marathonos/Shell/Navigation",
            "org.marathonos.Shell.Navigation1",
        )
        .await
        {
            let _: zbus::Result<()> = proxy
                .call("ExpectAppWindow", &(format!("waydroid.{pkg}"),))
                .await;
        }
        let message = object(
            json!({"t":"share","target":id,"mime":mime,"title":clean_text(&title,256),"text":text,"files":files}),
        );
        let result = self.request(Kind::Share, message, Some(tid.clone())).await;
        if result.is_err() {
            let _ = fs::remove_dir_all(self.xfer.join(&tid));
        }
        result.map(|_| ())
    }
}

fn field<'a>(message: &'a Map<String, Value>, key: &str) -> &'a str {
    message.get(key).and_then(Value::as_str).unwrap_or("")
}
fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
pub fn request_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn simple_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && clean_text(name, 200) == name
}
fn mime_type(mime: &str) -> bool {
    let Some((a, b)) = mime.split_once('/') else {
        return false;
    };
    !a.is_empty()
        && !b.is_empty()
        && mime.len() <= 128
        && mime
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._+*-/".contains(&c))
}
fn value_string(value: &OwnedValue) -> Option<String> {
    value
        .try_clone()
        .ok()
        .and_then(|v| String::try_from(v).ok())
}
pub fn target_id(id: &str) -> bool {
    let Some((pkg, class)) = id.split_once('/') else {
        return false;
    };
    id.len() <= 256
        && PackageName::try_from(pkg).is_ok()
        && !class.is_empty()
        && class
            .strip_prefix('.')
            .unwrap_or(class)
            .split('.')
            .all(|part| {
                !part.is_empty()
                    && part.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '$')
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
            })
}
fn targets(message: &Map<String, Value>) -> Vec<Value> {
    let mut out = Vec::new();
    for target in message
        .get("targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let pkg = target.get("pkg").and_then(Value::as_str).unwrap_or("");
        let activity = target.get("activity").and_then(Value::as_str).unwrap_or("");
        let id = format!("{pkg}/{activity}");
        if !target_id(&id) || pkg.starts_with("org.marathonos.deckard.") {
            continue;
        }
        let label = clean_text(
            target.get("label").and_then(Value::as_str).unwrap_or(""),
            128,
        );
        let icon = target
            .get("icon")
            .and_then(Value::as_str)
            .and_then(|s| base64_png(s, 256 * 1024).ok())
            .and_then(|png| image::load_from_memory(&png).ok())
            .filter(|image| image.width() <= 4096 && image.height() <= 4096)
            .and_then(|image| {
                let mut output = std::io::Cursor::new(Vec::new());
                image
                    .thumbnail(96, 96)
                    .write_to(&mut output, image::ImageFormat::Png)
                    .ok()?;
                Some(format!(
                    "data:image/png;base64,{}",
                    STANDARD.encode(output.into_inner())
                ))
            })
            .unwrap_or_default();
        out.push(json!({"id":id,"pkg":pkg,"label":label,"icon":icon,"multiple":target.get("multiple").and_then(Value::as_bool).unwrap_or(false)}));
        if out.len() >= MAX_TARGETS {
            break;
        }
    }
    out
}
fn target_map(value: &Value) -> Option<VariantMap> {
    let o = value.as_object()?;
    let mut map = VariantMap::new();
    for key in ["id", "pkg", "label", "icon"] {
        map.insert(key.into(), Str::from(o.get(key)?.as_str()?).into());
    }
    map.insert("multiple".into(), o.get("multiple")?.as_bool()?.into());
    Some(map)
}

/// Never follows a final symlink, and closes on exec.
fn open_at(dir: &OwnedFd, name: &str, flags: OFlags, mode: Mode) -> io::Result<OwnedFd> {
    Ok(rustix::fs::openat(
        dir,
        name,
        flags | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        mode,
    )?)
}
fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    Ok(rustix::fs::openat(
        rustix::fs::CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?)
}
fn open_child(dir: &OwnedFd, name: &str, flags: OFlags) -> io::Result<OwnedFd> {
    if !simple_name(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid name"));
    }
    open_at(dir, name, flags, Mode::empty())
}
fn create_transfer(root: &Path, id: &str) -> io::Result<OwnedFd> {
    if !request_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid transfer ID",
        ));
    }
    fs::create_dir(root.join(id))?;
    open_child(&open_dir(root)?, id, OFlags::RDONLY | OFlags::DIRECTORY)
}
fn copy_resources(dir: &OwnedFd, resources: &[VariantMap]) -> io::Result<(Vec<String>, String)> {
    let mut files = Vec::new();
    let mut texts = Vec::new();
    let mut total = 0_u64;
    for resource in resources {
        if let Some(raw) = resource.get("fileDescriptor") {
            let descriptor = Fd::try_from(raw.try_clone().map_err(io::Error::other)?)
                .map_err(io::Error::other)?;
            let owned = descriptor.as_fd().try_clone_to_owned()?;
            let mut input = File::from(owned);
            let meta = input.metadata()?;
            if !meta.is_file() || meta.len() > MAX_BYTES.saturating_sub(total) {
                return Err(io::Error::other("too large or not a file"));
            }
            let base = resource
                .get("name")
                .and_then(value_string)
                .unwrap_or_else(|| "file".into());
            let base = safe_name(&base);
            let mut name = base.clone();
            for n in 2..=MAX_RESOURCES + 1 {
                if !files.contains(&name) {
                    break;
                }
                name = format!("{n}-{base}");
            }
            let output = open_at(
                dir,
                &name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
                Mode::RUSR | Mode::WUSR,
            )?;
            let mut output = File::from(output);
            let copied = io::copy(
                &mut Read::by_ref(&mut input).take(MAX_BYTES - total + 1),
                &mut output,
            )?;
            if copied > meta.len() || copied > MAX_BYTES - total {
                return Err(io::Error::other("file grew during copy"));
            }
            total += copied;
            output.sync_all()?;
            files.push(name);
        } else {
            let text = resource
                .get("data")
                .and_then(value_string)
                .unwrap_or_default();
            if text.is_empty() || text.encode_utf16().count() > MAX_TEXT {
                return Err(io::Error::other("text of 1 to 65536 characters"));
            }
            texts.push(text);
        }
    }
    Ok((files, texts.join("\n")))
}
fn safe_name(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or("");
    let clean = clean_text(base, 200);
    let clean = clean.trim();
    if clean.is_empty() || clean == "." || clean == ".." {
        "file".into()
    } else {
        clean.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_descriptor_and_text_with_unique_basename() -> io::Result<()> {
        let scratch = tempfile::tempdir()?;
        let source = scratch.path().join("source.txt");
        fs::write(&source, b"hello")?;
        let root = scratch.path().join("xfer");
        fs::create_dir(&root)?;
        let dir = create_transfer(&root, "0123456789abcdef")?;
        let resource = || -> io::Result<VariantMap> {
            let file = File::open(&source)?;
            let fd: OwnedFd = file.into();
            let mut map = VariantMap::new();
            map.insert("name".into(), Str::from("../../sample.txt").into());
            map.insert(
                "fileDescriptor".into(),
                OwnedValue::try_from(Variant::from(Fd::from(fd))).map_err(io::Error::other)?,
            );
            Ok(map)
        };
        let mut text = VariantMap::new();
        text.insert("data".into(), Str::from("note").into());
        let (files, combined) = copy_resources(&dir, &[resource()?, resource()?, text])?;
        assert_eq!(files, ["sample.txt", "2-sample.txt"]);
        assert_eq!(combined, "note");
        assert_eq!(
            fs::read(root.join("0123456789abcdef/sample.txt"))?,
            b"hello"
        );
        assert_eq!(
            fs::read(root.join("0123456789abcdef/2-sample.txt"))?,
            b"hello"
        );
        Ok(())
    }

    #[test]
    fn inbound_transfer_refuses_symlink() -> io::Result<()> {
        let scratch = tempfile::tempdir()?;
        let root = scratch.path().join("xfer");
        fs::create_dir(&root)?;
        let dir = create_transfer(&root, "0123456789abcdef")?;
        fs::write(scratch.path().join("secret"), b"private")?;
        std::os::unix::fs::symlink(
            scratch.path().join("secret"),
            root.join("0123456789abcdef/link"),
        )?;
        assert!(open_child(&dir, "link", OFlags::RDONLY).is_err());
        assert!(open_child(&dir, "../secret", OFlags::RDONLY).is_err());
        Ok(())
    }

    #[test]
    fn sanitizes_targets_and_caps_list() {
        let entries: Vec<Value> = (0..80).map(|n| json!({"pkg":format!("com.example.app{n}"),"activity":".Share","label":"Share","multiple":true})).collect();
        let result = targets(&object(json!({"targets":entries})));
        assert_eq!(result.len(), MAX_TARGETS);
        assert_eq!(result[0]["id"], "com.example.app0/.Share");
        assert!(targets(&object(json!({"targets":[{"pkg":"org.marathonos.deckard.bridge","activity":".Share"},{"pkg":"../bad","activity":".Share"}]}))).is_empty());
    }
}
