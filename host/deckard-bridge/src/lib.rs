//! Marathon's user-session Deckard daemon (replaces C++ deckardd).
pub mod api;
pub mod app_channel;
pub mod audio;
pub mod auth;
pub mod contract;
pub mod ime;
pub mod intents;
pub mod keel;
pub mod location;
pub mod manage;
pub mod notifications;
pub mod packages;
pub mod prompts;
pub mod reclaim;
pub mod runtime;
pub mod sharing;
pub mod shell_sync;
pub mod storage;
pub mod watchdog;

use std::fmt::Display;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;

use serde_json::{Map, Value};
use zbus::blocking::{Connection, MessageIterator};
use zbus::match_rule::Builder;
use zbus::{MatchRule, Message, message};

/// Sends one message to an Android peer; false when it could not.
pub type Outbound = dyn Fn(serde_json::Map<String, serde_json::Value>) -> bool + Send + Sync;

/// The object in `value`; empty for anything else.
#[must_use]
pub fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(object) => object,
        _ => Map::new(),
    }
}

/// Logs `result`'s error after `what`.
pub fn report<T, E: Display>(what: &str, result: Result<T, E>) {
    if let Err(error) = result {
        eprintln!("deckard-bridge: {what}: {error}");
    }
}

/// Runs `work` on its own thread and logs its error after `what`.
pub fn spawn<E: Display>(
    what: &'static str,
    work: impl FnOnce() -> Result<(), E> + Send + 'static,
) {
    thread::spawn(move || report(what, work()));
}

/// Signals from `sender` on `path` and `interface`.
///
/// # Errors
/// A name is invalid.
pub fn signals<'m>(
    sender: &'m str,
    path: &'m str,
    interface: &'m str,
) -> zbus::Result<Builder<'m>> {
    MatchRule::builder()
        .msg_type(message::Type::Signal)
        .sender(sender)?
        .path(path)?
        .interface(interface)
}

/// The bus's `NameOwnerChanged` for `name`.
///
/// # Errors
/// `name` is invalid.
pub fn owner_changes(name: &'static str) -> zbus::Result<MatchRule<'static>> {
    let rule = MatchRule::builder().msg_type(message::Type::Signal);
    let rule = rule.sender("org.freedesktop.DBus")?;
    let rule = rule.interface("org.freedesktop.DBus")?;
    Ok(rule.member("NameOwnerChanged")?.add_arg(name)?.build())
}

/// Whether a `NameOwnerChanged` gives its name an owner.
#[must_use]
pub fn gained_owner(signal: &Message) -> bool {
    let body = signal.body().deserialize::<(String, String, String)>();
    body.is_ok_and(|(_, _, owner)| !owner.is_empty())
}

/// Whether a signal matched by a rule matters.
pub type Relevant = fn(&Message) -> bool;

/// Asks a [`follow`] worker for another refresh; cheap to clone.
#[derive(Clone)]
pub struct Wake(SyncSender<()>);

impl Wake {
    /// A refresh already pending covers this one. False once the worker
    /// has stopped.
    pub fn wake(&self) -> bool {
        !matches!(self.0.try_send(()), Err(TrySendError::Disconnected(())))
    }
}

/// Runs `refresh` on its own thread now and after each wake or relevant
/// signal. Each rule drains on its own thread, so a burst during a refresh
/// folds into one more and never fills a queue behind a pending reply.
///
/// # Errors
/// A subscription failed.
pub fn follow(
    bus: &Connection,
    rules: Vec<(MatchRule<'static>, Relevant)>,
    mut refresh: impl FnMut() + Send + 'static,
) -> zbus::Result<Wake> {
    let (sender, wakes) = mpsc::sync_channel(1);
    let wake = Wake(sender);
    for (rule, relevant) in rules {
        let signals = MessageIterator::for_match_rule(rule, bus, Some(16))?;
        let wake = wake.clone();
        thread::spawn(move || {
            for signal in signals {
                match signal {
                    Ok(signal) if relevant(&signal) && !wake.wake() => return,
                    Ok(_) => {}
                    Err(error) => eprintln!("deckard-bridge: signal failed: {error}"),
                }
            }
        });
    }
    wake.wake();
    thread::spawn(move || {
        for () in wakes {
            refresh();
        }
    });
    Ok(wake)
}
