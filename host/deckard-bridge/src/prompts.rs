//! Android's runtime permission requests as Marathon's prompt
//! (docs/permissions.md). DeckardSystem sends `perm_ask` for each question
//! PermissionController would draw; this asks the Shell's
//! `Permissions1.PromptAndroid` and sends `perm_answer` with the person's
//! choice. `unsupported` hands a request back to Android's own dialog.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_io::Timer;
use deckard_core::ids::PackageName;
use deckard_core::sync;
use deckard_core::validate;
use futures_lite::future;
use serde_json::{Map, Value, json};
use zbus::zvariant::{OwnedValue, Str};

use crate::Outbound;
use crate::contract::{self, VariantMap};

const SHELL: &str = "org.marathonos.Shell";
const PATH: &str = "/org/marathonos/Shell/Permissions";
const INTERFACE: &str = "org.marathonos.Shell.Permissions1";
const NO_REPLY: &str = "org.freedesktop.DBus.Error.NoReply";
/// Prompts the Shell may hold at once; more are left to Android.
pub const MAX_WAITING: usize = 8;
const TIMEOUT: Duration = Duration::from_secs(10 * 60);
const ANSWERS: [&str; 4] = ["allow", "once", "deny", "cancel"];

/// The Shell's `PromptAndroid` arguments for a `perm_ask`: app id,
/// Marathon permission and options. `None` when Marathon can't draw it.
#[must_use]
pub fn prompt(ask: &Map<String, Value>) -> Option<(String, &'static str, VariantMap)> {
    let text = |key| match ask.get(key) {
        None => Some(""),
        Some(value) => value.as_str(),
    };
    let flag = |key| match ask.get(key) {
        None => Some(false),
        Some(value) => value.as_bool(),
    };
    let (pkg, kind, accuracy) = (text("pkg")?, text("kind")?, text("accuracy")?);
    PackageName::try_from(pkg).ok()?;
    if !["foreground", "background", "precise"].contains(&kind)
        || !["", "choice", "approximate"].contains(&accuracy)
    {
        return None;
    }
    let (once, precise_default) = (flag("once")?, flag("preciseDefault")?);
    let perms = ask.get("perms").and_then(Value::as_array)?;
    if perms.is_empty() || perms.len() > 16 {
        return None;
    }
    let mut name = None;
    for permission in perms {
        let mapped = permission
            .as_str()
            .and_then(contract::marathon_permission)?;
        if name.is_some_and(|name| name != mapped) {
            return None;
        }
        name = Some(mapped);
    }
    let str = |value: &str| OwnedValue::from(Str::from(value.to_owned()));
    let options = VariantMap::from([
        ("request".to_owned(), str(text("id")?)),
        ("kind".to_owned(), str(kind)),
        ("once".to_owned(), once.into()),
        ("accuracy".to_owned(), str(accuracy)),
        ("preciseDefault".to_owned(), precise_default.into()),
    ]);
    Some((format!("waydroid.{pkg}"), name?, options))
}

fn is_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Default)]
struct Waiting {
    generation: u64,
    /// id -> generation, so a late reply finds no one.
    ids: HashMap<String, u64>,
}

/// Android's permission questions in flight; cheap to clone.
#[derive(Clone)]
pub struct PermissionPrompts {
    bus: zbus::Connection,
    send: Arc<Outbound>,
    waiting: Arc<Mutex<Waiting>>,
}

impl PermissionPrompts {
    /// Asks the Shell on `bus` and answers DeckardSystem with `send`.
    #[must_use]
    pub fn new(bus: zbus::Connection, send: Arc<Outbound>) -> Self {
        Self {
            bus,
            send,
            waiting: Arc::default(),
        }
    }

    fn waiting(&self) -> MutexGuard<'_, Waiting> {
        sync::lock(&self.waiting)
    }

    /// Takes `perm_ask` and `perm_cancel`; other messages are ignored.
    pub fn handle(&self, message: &Map<String, Value>) {
        let kind = message.get("t").and_then(Value::as_str).unwrap_or("");
        let Some(id) = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| is_id(id))
        else {
            return;
        };
        if kind == "perm_cancel" {
            if self.waiting().ids.remove(id).is_some() {
                self.cancel(id.to_owned());
            }
            return;
        }
        if kind != "perm_ask" {
            return;
        }
        let accepted = {
            let mut waiting = self.waiting();
            if waiting.ids.contains_key(id) {
                return;
            }
            match prompt(message) {
                Some(prompt) if waiting.ids.len() < MAX_WAITING => {
                    waiting.generation += 1;
                    let generation = waiting.generation;
                    waiting.ids.insert(id.to_owned(), generation);
                    Some((generation, prompt))
                }
                _ => None,
            }
        };
        let Some((generation, (app_id, permission, options))) = accepted else {
            let pkg = message.get("pkg").and_then(Value::as_str).unwrap_or("");
            let count = message
                .get("perms")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let pkg = validate::clean_text(pkg, 255);
            eprintln!("deckard-bridge: left to Android: {pkg} {count} permissions");
            self.answer(id, "unsupported", false);
            return;
        };
        let this = self.clone();
        let id = id.to_owned();
        let ask = async move {
            let call = async {
                let body = (app_id, permission, options);
                let reply = this
                    .bus
                    .call_method(Some(SHELL), PATH, Some(INTERFACE), "PromptAndroid", &body)
                    .await;
                Some(reply.and_then(|reply| reply.body().deserialize::<VariantMap>()))
            };
            let timeout = async {
                Timer::after(TIMEOUT).await;
                None
            };
            let reply = future::or(call, timeout).await;
            {
                let mut waiting = this.waiting();
                if waiting.ids.get(&id) != Some(&generation) {
                    return;
                }
                waiting.ids.remove(&id);
            }
            let (answer, precise) = match reply {
                Some(Ok(reply)) => {
                    let get = |key: &str| reply.get(key).map(|value| &**value);
                    let answer = get("answer").and_then(|v| v.downcast_ref::<&str>().ok());
                    let precise = get("precise").and_then(|v| v.downcast_ref::<bool>().ok());
                    let answer = ANSWERS.into_iter().find(|a| Some(*a) == answer);
                    let answer = answer.unwrap_or("cancel");
                    (answer, precise.unwrap_or(false))
                }
                Some(Err(error)) => {
                    eprintln!("deckard-bridge: PromptAndroid: {error}");
                    let no_reply = match &error {
                        zbus::Error::MethodError(name, ..) => name.as_str() == NO_REPLY,
                        _ => false,
                    };
                    (if no_reply { "cancel" } else { "unsupported" }, false)
                }
                None => ("cancel", false),
            };
            this.answer(&id, answer, precise);
        };
        self.bus.executor().spawn(ask, "permission-prompt").detach();
    }

    /// DeckardSystem went away: withdraw every prompt, answering none.
    pub fn system_lost(&self) {
        let ids = std::mem::take(&mut self.waiting().ids);
        for id in ids.into_keys() {
            self.cancel(id);
        }
    }

    fn answer(&self, id: &str, answer: &str, precise: bool) {
        let message = json!({"t": "perm_answer", "id": id, "answer": answer, "precise": precise});
        (self.send)(crate::object(message));
    }

    fn cancel(&self, id: String) {
        let bus = self.bus.clone();
        let cancel = async move {
            let (method, body) = ("CancelAndroidPrompt", (id,));
            let reply = bus.call_method(Some(SHELL), PATH, Some(INTERFACE), method, &body);
            if let Err(error) = reply.await {
                eprintln!("deckard-bridge: {method}: {error}");
            }
        };
        self.bus
            .executor()
            .spawn(cancel, "permission-cancel")
            .detach();
    }
}
