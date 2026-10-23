//! DeckardSystem intent routing. Feed only messages from the authenticated
//! system socket; `Effect`s are executed by the daemon's session-bus loop.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use deckard_core::ids::PackageName;
use deckard_core::validate::clean_text;
use futures_lite::future;
use serde_json::{Map, Value, json};
use zbus::zvariant::{OwnedValue, Str, Value as Variant};

const MAX_PENDING: usize = 4;
const MAX_FILES: usize = 32;
const MAX_BYTES: u64 = 1 << 30;
const O_NOFOLLOW: i32 = 0o400000;

#[derive(Debug, Clone, PartialEq)]
pub enum BusCall {
    Navigation {
        method: &'static str,
        app: &'static str,
        route: String,
        args: Value,
    },
    Keel {
        app: &'static str,
        action: &'static str,
        args: Value,
        timeout_ms: u64,
    },
    Portal {
        id: String,
        method: &'static str,
        options: Value,
    },
    ClosePortal {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Call(BusCall),
    Send(Map<String, Value>),
}

/// Executes a validated intent effect on the session bus. Portal calls
/// return their Request object path; camera calls return the Keel JSON reply.
pub async fn execute_bus_call(
    bus: &zbus::Connection,
    call: &BusCall,
) -> zbus::Result<Option<String>> {
    match call {
        BusCall::Navigation {
            method,
            app,
            route,
            args,
        } => {
            let proxy = zbus::Proxy::new(
                bus,
                "org.marathonos.Shell",
                "/org/marathonos/Shell/Navigation",
                "org.marathonos.Shell.Navigation1",
            )
            .await?;
            if *method == "LaunchAppWithRoute" {
                let _: () = proxy
                    .call(*method, &(*app, route.as_str(), args.to_string()))
                    .await?;
            } else {
                let _: () = proxy.call(*method, &(*app,)).await?;
            }
            Ok(None)
        }
        BusCall::Keel {
            app,
            action,
            args,
            timeout_ms,
        } => {
            let destination = format!("org.marathonos.{app}");
            let proxy = zbus::Proxy::new(
                bus,
                destination,
                "/org/shipwright/Keel/Actions",
                "org.shipwright.Keel.Actions",
            )
            .await?;
            let future = async {
                proxy
                    .call::<_, _, String>(
                        "Invoke",
                        &(
                            *action,
                            args.to_string(),
                            HashMap::<String, OwnedValue>::new(),
                        ),
                    )
                    .await
            };
            let answer = future::or(future, async {
                async_io::Timer::after(std::time::Duration::from_millis(*timeout_ms)).await;
                Err(zbus::Error::Failure("intent call timed out".into()))
            })
            .await?;
            Ok(Some(answer))
        }
        BusCall::Portal {
            id,
            method,
            options,
        } => {
            let proxy = zbus::Proxy::new(
                bus,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.FileChooser",
            )
            .await?;
            let mut fields = HashMap::<String, OwnedValue>::new();
            fields.insert(
                "handle_token".into(),
                Str::from(format!("deckard_{id}").as_str()).into(),
            );
            if *method == "OpenFile" {
                fields.insert(
                    "multiple".into(),
                    options
                        .get("multiple")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        .into(),
                );
                if let Some(filters) = options.get("filters").and_then(Value::as_array) {
                    let filters: Vec<(String, Vec<(u32, String)>)> = filters
                        .iter()
                        .filter_map(|filter| {
                            let parts = filter.as_array()?;
                            let name = parts.first()?.as_str()?.to_owned();
                            let rules = parts
                                .get(1)?
                                .as_array()?
                                .iter()
                                .filter_map(|rule| {
                                    let rule = rule.as_array()?;
                                    Some((
                                        rule.first()?.as_u64()? as u32,
                                        rule.get(1)?.as_str()?.to_owned(),
                                    ))
                                })
                                .collect();
                            Some((name, rules))
                        })
                        .collect();
                    if let Ok(value) = OwnedValue::try_from(Variant::from(filters)) {
                        fields.insert("filters".into(), value);
                    }
                }
            } else {
                let name = options
                    .get("current_name")
                    .and_then(Value::as_str)
                    .unwrap_or("file");
                fields.insert("current_name".into(), Str::from(name).into());
            }
            let title = if *method == "OpenFile" {
                "Open"
            } else {
                "Save"
            };
            let path: zbus::zvariant::OwnedObjectPath =
                proxy.call(*method, &(String::new(), title, fields)).await?;
            Ok(Some(path.to_string()))
        }
        BusCall::ClosePortal { id } => {
            let unique = bus
                .unique_name()
                .ok_or_else(|| zbus::Error::Failure("session bus has no unique name".into()))?;
            let sender = unique.as_str().trim_start_matches(':').replace('.', "_");
            let path = format!("/org/freedesktop/portal/desktop/request/{sender}/deckard_{id}");
            let proxy = zbus::Proxy::new(
                bus,
                "org.freedesktop.portal.Desktop",
                path,
                "org.freedesktop.portal.Request",
            )
            .await?;
            let _: () = proxy.call("Close", &()).await?;
            Ok(None)
        }
    }
}

#[derive(Debug, Clone)]
struct Pending {
    mode: String,
    kind: String,
}

pub struct SystemIntents {
    xfer: PathBuf,
    camera: PathBuf,
    generation: Option<u64>,
    focus: String,
    pending: HashMap<String, Pending>,
    saved: HashMap<String, PathBuf>,
}

impl SystemIntents {
    pub fn new(xfer: PathBuf, camera: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&xfer)?;
        Ok(Self {
            xfer,
            camera,
            generation: None,
            focus: String::new(),
            pending: HashMap::new(),
            saved: HashMap::new(),
        })
    }

    pub fn connected(&mut self, generation: u64) -> Vec<Effect> {
        self.generation = Some(generation);
        self.pending.clear();
        vec![self.focus_effect()]
    }

    pub fn disconnected(&mut self, generation: u64) -> Vec<Effect> {
        if self.generation != Some(generation) {
            return Vec::new();
        }
        self.generation = None;
        let ids: Vec<String> = self.pending.keys().cloned().collect();
        self.pending.clear();
        ids.into_iter()
            .map(|id| Effect::Call(BusCall::ClosePortal { id }))
            .collect()
    }

    pub fn foreground_changed(&mut self, app: &str) -> Option<Effect> {
        let pkg = app
            .strip_prefix("waydroid.")
            .filter(|p| PackageName::try_from(*p).is_ok())
            .unwrap_or("");
        if self.focus == pkg {
            return None;
        }
        self.focus = pkg.into();
        self.generation.map(|_| self.focus_effect())
    }

    fn focus_effect(&self) -> Effect {
        Effect::Send(object(json!({"t":"focus", "pkg":self.focus})))
    }

    pub fn handle_authenticated(
        &mut self,
        generation: u64,
        message: &Map<String, Value>,
    ) -> Vec<Effect> {
        if self.generation != Some(generation) {
            return Vec::new();
        }
        let kind = field(message, "t");
        match kind {
            "open" => route(message)
                .map(|call| vec![Effect::Call(call)])
                .unwrap_or_default(),
            "pick" => self.pick(message),
            "pick_done" => {
                self.pick_done(field(message, "id"));
                Vec::new()
            }
            "pick_cancel" => {
                let id = field(message, "id");
                if self.pending.remove(id).is_some() {
                    vec![Effect::Call(BusCall::ClosePortal { id: id.into() })]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn pick(&mut self, message: &Map<String, Value>) -> Vec<Effect> {
        let id = field(message, "id");
        if !request_id(id) || self.pending.contains_key(id) {
            return Vec::new();
        }
        if self.pending.len() >= MAX_PENDING {
            return vec![picked(id, Vec::new())];
        }
        let mode = field(message, "mode");
        let kind = if field(message, "kind") == "video" {
            "video"
        } else {
            "image"
        };
        match mode {
            "capture" => {
                self.pending.insert(
                    id.into(),
                    Pending {
                        mode: mode.into(),
                        kind: kind.into(),
                    },
                );
                vec![Effect::Call(BusCall::Keel {
                    app: "camera",
                    action: "camera.capture",
                    args: json!({"mode":if kind == "video" {"video"} else {"photo"}}),
                    timeout_ms: 630_000,
                })]
            }
            "open" | "save" => {
                let options = if mode == "save" {
                    json!({"handle_token":format!("deckard_{id}"), "current_name":safe_name(field(message, "name"))})
                } else {
                    let types = message
                        .get("mime")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let any = types.iter().any(|v| v.as_str() == Some("*/*"));
                    let filters: Vec<_> = types
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|t| mime_type(t))
                        .take(16)
                        .map(|t| json!([1, t]))
                        .collect();
                    if any || filters.is_empty() {
                        json!({"handle_token":format!("deckard_{id}"), "multiple":message.get("multiple").and_then(Value::as_bool).unwrap_or(false)})
                    } else {
                        json!({"handle_token":format!("deckard_{id}"), "multiple":message.get("multiple").and_then(Value::as_bool).unwrap_or(false), "filters":[["Files",filters]]})
                    }
                };
                self.pending.insert(
                    id.into(),
                    Pending {
                        mode: mode.into(),
                        kind: String::new(),
                    },
                );
                vec![Effect::Call(BusCall::Portal {
                    id: id.into(),
                    method: if mode == "open" {
                        "OpenFile"
                    } else {
                        "SaveFile"
                    },
                    options,
                })]
            }
            _ => vec![picked(id, Vec::new())],
        }
    }

    /// The integrator must correlate the portal Request path with this ID and
    /// accept its Response only from the portal's current D-Bus owner.
    pub fn portal_response(&mut self, id: &str, response: u32, uris: &[String]) -> Vec<Effect> {
        let Some(pending) = self.pending.remove(id) else {
            return Vec::new();
        };
        if pending.mode == "capture" {
            return Vec::new();
        }
        let paths: Vec<PathBuf> = uris
            .iter()
            .filter_map(|uri| file_uri(uri))
            .take(MAX_FILES)
            .collect();
        if response != 0 || paths.is_empty() {
            return vec![picked(id, Vec::new())];
        }
        if pending.mode == "save" {
            let path = paths[0].clone();
            self.saved.insert(id.into(), path.clone());
            return vec![Effect::Send(object(
                json!({"t":"picked","id":id,"ok":true,"files":[{"name":safe_name(path.to_string_lossy().as_ref())}]}),
            ))];
        }
        let copied = self.copy_in(id, &paths);
        if copied.is_err() {
            let _ = fs::remove_dir_all(self.xfer.join(id));
        }
        vec![picked(id, copied.unwrap_or_default())]
    }

    pub fn camera_response(&mut self, id: &str, result: Option<&str>) -> Vec<Effect> {
        let Some(pending) = self.pending.remove(id) else {
            return Vec::new();
        };
        if pending.mode != "capture" {
            return Vec::new();
        }
        let Some(path) = result
            .and_then(|r| serde_json::from_str::<Value>(r).ok())
            .and_then(|r| r.get("path").and_then(Value::as_str).map(PathBuf::from))
        else {
            return vec![picked(id, Vec::new())];
        };
        let folder = self.camera.canonicalize().ok();
        let file = path.canonicalize().ok();
        let allowed = folder
            .zip(file.as_ref())
            .is_some_and(|(dir, file)| file.parent() == Some(dir.as_path()));
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let right = if pending.kind == "video" {
            ["mp4", "mkv", "webm", "mov"].contains(&extension.as_str())
        } else {
            ["jpg", "jpeg", "png"].contains(&extension.as_str())
        };
        if !allowed || !right {
            return vec![picked(id, Vec::new())];
        }
        let copied = self.copy_in(id, &[path]);
        if copied.is_err() {
            let _ = fs::remove_dir_all(self.xfer.join(id));
        }
        vec![picked(id, copied.unwrap_or_default())]
    }

    fn copy_in(&self, id: &str, paths: &[PathBuf]) -> io::Result<Vec<Value>> {
        if !request_id(id) || paths.len() > MAX_FILES {
            return Err(io::Error::other("invalid transfer"));
        }
        let dir = self.xfer.join(id);
        fs::create_dir_all(&dir)?;
        let mut files = Vec::new();
        let mut total = 0_u64;
        for path in paths {
            if fs::symlink_metadata(path)?.file_type().is_symlink() {
                return Err(io::Error::other("symlink source refused"));
            }
            let input = OpenOptions::new()
                .read(true)
                .custom_flags(O_NOFOLLOW)
                .open(path)?;
            let metadata = input.metadata()?;
            if !metadata.is_file() || metadata.len() > MAX_BYTES.saturating_sub(total) {
                return Err(io::Error::other("transfer too large or not regular"));
            }
            let base = safe_name(path.to_string_lossy().as_ref());
            let mut name = base.clone();
            for n in 2..=MAX_FILES + 1 {
                if !dir.join(&name).exists() {
                    break;
                }
                name = format!("{n}-{base}");
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.join(&name))?;
            let copied = io::copy(&mut input.take(MAX_BYTES - total + 1), &mut output)?;
            if copied > metadata.len() || copied > MAX_BYTES - total {
                return Err(io::Error::other("file grew during transfer"));
            }
            total += copied;
            files.push(json!({"name":name,"size":copied}));
        }
        Ok(files)
    }

    fn pick_done(&mut self, id: &str) {
        if !request_id(id) {
            return;
        }
        let Some(target) = self.saved.remove(id) else {
            return;
        };
        let from = self
            .xfer
            .join(id)
            .join(safe_name(target.to_string_lossy().as_ref()));
        let Some(parent) = target.parent() else {
            return;
        };
        let temp = parent.join(format!(".deckard-{id}.part"));
        let result = (|| -> io::Result<()> {
            let mut input = OpenOptions::new()
                .read(true)
                .custom_flags(O_NOFOLLOW)
                .open(from)?;
            if !input.metadata()?.is_file() {
                return Err(io::Error::other("not a regular file"));
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            let len = io::copy(
                &mut Read::by_ref(&mut input).take(MAX_BYTES + 1),
                &mut output,
            )?;
            if len > MAX_BYTES {
                return Err(io::Error::other("save too large"));
            }
            output.sync_all()?;
            fs::rename(&temp, &target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
    }
}

fn picked(id: &str, files: Vec<Value>) -> Effect {
    Effect::Send(object(
        json!({"t":"picked","id":id,"ok":!files.is_empty(),"files":files}),
    ))
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
fn field<'a>(map: &'a Map<String, Value>, key: &str) -> &'a str {
    map.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn request_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn safe_name(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = clean_text(name, 200)
        .chars()
        .filter(|c| *c != '/' && *c != '\\')
        .collect();
    let clean = clean.trim();
    if clean.is_empty() || clean == "." || clean == ".." {
        "file".into()
    } else {
        clean.into()
    }
}
fn mime_type(value: &str) -> bool {
    let Some((a, b)) = value.split_once('/') else {
        return false;
    };
    !a.is_empty()
        && !b.is_empty()
        && a.len() <= 64
        && b.len() <= 64
        && a.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".+-".contains(&c))
        && b.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".+*-".contains(&c))
}
fn file_uri(value: &str) -> Option<PathBuf> {
    let rest = value.strip_prefix("file://")?;
    if !rest.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::new();
    let mut iter = rest.bytes();
    while let Some(c) = iter.next() {
        if c == b'%' {
            let hi = (iter.next()? as char).to_digit(16)?;
            let lo = (iter.next()? as char).to_digit(16)?;
            bytes.push(((hi << 4) | lo) as u8);
        } else {
            bytes.push(c);
        }
    }
    if bytes.contains(&0) {
        return None;
    }
    let path = String::from_utf8(bytes).ok()?;
    Some(PathBuf::from(path))
}

pub fn route(message: &Map<String, Value>) -> Option<BusCall> {
    let kind = field(message, "kind");
    let uri = field(message, "uri");
    if uri.len() > 4096 {
        return None;
    }
    let scheme = uri.split(':').next()?.to_ascii_lowercase();
    match kind {
        "url" => {
            if uri.len() > 2048 || !["http", "https"].contains(&scheme.as_str()) {
                return None;
            }
            let host = uri.split_once("://")?.1.split(['/', '?', '#']).next()?;
            if host.is_empty() || host.contains('@') || host.contains(' ') || host.contains('\\') {
                return None;
            }
            Some(BusCall::Keel {
                app: "browser",
                action: "browser.open",
                args: json!({"url":uri,"newTab":true}),
                timeout_ms: 30_000,
            })
        }
        "tel" | "sms" => {
            if (kind == "tel" && scheme != "tel")
                || (kind == "sms" && !["sms", "smsto", "mms", "mmsto"].contains(&scheme.as_str()))
            {
                return None;
            }
            let raw = uri
                .split_once(':')?
                .1
                .split(['?', ','])
                .next()
                .unwrap_or("");
            let raw = percent_decode(raw)
                .unwrap_or_default()
                .replace([' ', '(', ')', '.', '-'], "");
            let number = if raw.len() <= 64
                && raw
                    .bytes()
                    .all(|c| c.is_ascii_digit() || b"+*#,;pw".contains(&c))
            {
                raw
            } else {
                String::new()
            };
            let app = if kind == "tel" { "phone" } else { "messages" };
            let body = if kind == "sms" {
                let raw = message
                    .get("body")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        percent_decode(query(uri, "body").unwrap_or_default()).unwrap_or_default()
                    });
                clean_text(&raw, 10_000)
            } else {
                String::new()
            };
            if number.is_empty() && body.is_empty() {
                return Some(BusCall::Navigation {
                    method: "LaunchApp",
                    app,
                    route: String::new(),
                    args: json!({}),
                });
            }
            let route = if kind == "tel" {
                "/call"
            } else if number.is_empty() {
                "compose"
            } else {
                "conversation"
            };
            let mut args = serde_json::Map::new();
            if !number.is_empty() {
                args.insert("number".into(), json!(number));
            }
            if !body.is_empty() {
                args.insert("body".into(), json!(body));
            }
            Some(BusCall::Navigation {
                method: "LaunchAppWithRoute",
                app,
                route: route.into(),
                args: Value::Object(args),
            })
        }
        "mailto" => {
            let addresses = |key| {
                message
                    .get(key)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::trim)
                            .filter(|s| {
                                !s.is_empty()
                                    && s.len() <= 254
                                    && !s
                                        .bytes()
                                        .any(|b| b.is_ascii_whitespace() || b",<>\"".contains(&b))
                            })
                            .take(20)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default()
            };
            let args = json!({"to":addresses("to"),"cc":addresses("cc"),"subject":clean_text(field(message,"subject"),1000),"body":clean_text(field(message,"body"),100_000)});
            Some(BusCall::Keel {
                app: "email",
                action: "mail.compose",
                args,
                timeout_ms: 30_000,
            })
        }
        "geo" => {
            if scheme != "geo" {
                return None;
            }
            let body = uri.strip_prefix("geo:")?;
            let q = percent_decode(query(uri, "q").unwrap_or_default()).unwrap_or_default();
            let point = |s: &str| {
                let (where_, name) = s
                    .split_once('(')
                    .map_or((s, ""), |(a, b)| (a, b.trim_end_matches(')')));
                let (lat, lon) = where_.split_once(',')?;
                let lat: f64 = lat.trim().parse().ok()?;
                let lon: f64 = lon.trim().parse().ok()?;
                if !(-90.0..=90.0).contains(&lat)
                    || !(-180.0..=180.0).contains(&lon)
                    || (lat == 0.0 && lon == 0.0)
                {
                    return None;
                }
                Some(json!({"lat":lat,"lon":lon,"name":clean_text(name,256)}))
            };
            if let Some(args) =
                point(&q).or_else(|| point(body.split(['?', ';']).next().unwrap_or("")))
            {
                return Some(BusCall::Keel {
                    app: "maps",
                    action: "map.show",
                    args,
                    timeout_ms: 30_000,
                });
            }
            if !q.trim().is_empty() {
                return Some(BusCall::Keel {
                    app: "maps",
                    action: "search.show",
                    args: json!({"query":clean_text(q.trim(),256)}),
                    timeout_ms: 30_000,
                });
            }
            Some(BusCall::Navigation {
                method: "LaunchApp",
                app: "maps",
                route: String::new(),
                args: json!({}),
            })
        }
        "settings" => {
            let action = field(message, "action");
            let pkg = field(message, "pkg");
            let route = settings_route(action, pkg);
            Some(BusCall::Navigation {
                method: if route.is_empty() {
                    "LaunchApp"
                } else {
                    "LaunchAppWithRoute"
                },
                app: "settings",
                route,
                args: json!({}),
            })
        }
        _ => None,
    }
}

fn percent_decode(value: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut it = value.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hi = (it.next()? as char).to_digit(16)?;
            let lo = (it.next()? as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}
fn query<'a>(uri: &'a str, key: &str) -> Option<&'a str> {
    uri.split_once('?')?.1.split('&').find_map(|part| {
        part.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
    })
}
pub fn settings_route(action: &str, pkg: &str) -> String {
    let page = match action {
        "android.settings.WIFI_SETTINGS"
        | "android.settings.WIRELESS_SETTINGS"
        | "android.settings.panel.action.INTERNET_CONNECTIVITY"
        | "android.settings.panel.action.WIFI" => "wifi",
        "android.settings.BLUETOOTH_SETTINGS" => "bluetooth",
        "android.settings.SOUND_SETTINGS" | "android.settings.panel.action.VOLUME" => "sound",
        "android.settings.DISPLAY_SETTINGS" => "display",
        "android.settings.DATE_SETTINGS" => "datetime",
        "android.settings.INTERNAL_STORAGE_SETTINGS" | "android.settings.MEMORY_CARD_SETTINGS" => {
            "storage"
        }
        "android.settings.BATTERY_SAVER_SETTINGS" | "android.intent.action.POWER_USAGE_SUMMARY" => {
            "battery"
        }
        "android.settings.SECURITY_SETTINGS" => "security",
        "android.settings.INPUT_METHOD_SETTINGS" => "keyboard",
        "android.settings.SYNC_SETTINGS" | "android.settings.ADD_ACCOUNT_SETTINGS" => "accounts",
        "android.settings.DEVICE_INFO_SETTINGS" => "about",
        "android.settings.NOTIFICATION_SETTINGS"
        | "android.settings.ZEN_MODE_SETTINGS"
        | "android.settings.APP_NOTIFICATION_SETTINGS"
        | "android.settings.CHANNEL_NOTIFICATION_SETTINGS" => "notifications",
        "android.settings.APPLICATION_DETAILS_SETTINGS"
        | "android.settings.MANAGE_APPLICATIONS_SETTINGS"
        | "android.settings.MANAGE_ALL_APPLICATIONS_SETTINGS" => "appmanager",
        _ => "",
    };
    let access = match action {
        "android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION"
        | "android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION" => "all_files",
        "android.settings.MANAGE_UNKNOWN_APP_SOURCES" => "install_apps",
        "android.settings.action.MANAGE_OVERLAY_PERMISSION" => "overlay",
        "android.settings.action.MANAGE_WRITE_SETTINGS" => "write_settings",
        "android.settings.USAGE_ACCESS_SETTINGS" => "usage",
        "android.settings.ACTION_NOTIFICATION_LISTENER_SETTINGS"
        | "android.settings.NOTIFICATION_LISTENER_DETAIL_SETTINGS" => "notification_listener",
        "android.settings.NOTIFICATION_POLICY_ACCESS_SETTINGS" => "dnd",
        "android.settings.IGNORE_BATTERY_OPTIMIZATION_SETTINGS"
        | "android.settings.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS" => "battery",
        "android.settings.REQUEST_SCHEDULE_EXACT_ALARM" => "alarms",
        "android.settings.PICTURE_IN_PICTURE_SETTINGS" => "pip",
        "android.settings.ACCESSIBILITY_SETTINGS" => "accessibility",
        _ => "",
    };
    let valid = PackageName::try_from(pkg).is_ok();
    if !access.is_empty() {
        return if valid {
            format!("androidaccess/{access}/{pkg}")
        } else {
            format!("androidaccess/{access}")
        };
    }
    if valid && (page == "notifications" || page == "appmanager") {
        return format!("{page}/waydroid.{pkg}");
    }
    page.into()
}
