//! DeckardSystem's `manage` requests, answered by id within one connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_channel::Sender;
use async_io::Timer;
use deckard_core::sync;
use deckard_core::validate;
use futures_lite::future;
use serde_json::{Map, Value};

pub const MAX_PENDING: usize = 32;
const TIMEOUT: Duration = Duration::from_secs(30);

struct Pending {
    generation: u64,
    sender: Sender<Result<Map<String, Value>, String>>,
}

#[derive(Default)]
struct Inner {
    next: u64,
    generation: u64,
    outbound: Option<Sender<Map<String, Value>>>,
    pending: HashMap<String, Pending>,
}

/// The DeckardSystem connection and its requests in flight; cheap to clone.
#[derive(Clone, Default)]
pub struct Manage(Arc<Mutex<Inner>>);

impl Manage {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        sync::lock(&self.0)
    }

    /// A new DeckardSystem sends through `sender`; its generation.
    /// Requests to the old one fail.
    pub fn connect(&self, sender: Sender<Map<String, Value>>) -> u64 {
        let mut inner = self.inner();
        inner.generation = inner.generation.wrapping_add(1);
        for (_, pending) in inner.pending.drain() {
            let _ = pending.sender.try_send(Err("Android stopped".into()));
        }
        inner.outbound = Some(sender);
        inner.generation
    }

    /// DeckardSystem `generation` went away; a newer one stays.
    pub fn disconnect(&self, generation: u64) {
        let mut inner = self.inner();
        if inner.generation != generation {
            return;
        }
        inner.outbound = None;
        for (_, pending) in inner.pending.drain() {
            let _ = pending.sender.try_send(Err("Android stopped".into()));
        }
    }

    #[must_use]
    pub fn connected(&self) -> bool {
        self.inner().outbound.is_some()
    }

    /// Whether `generation` is the one connected.
    #[must_use]
    pub fn is_generation(&self, generation: u64) -> bool {
        let inner = self.inner();
        inner.generation == generation && inner.outbound.is_some()
    }

    /// Queues `message`; false when offline or the queue is full.
    pub fn send(&self, message: Map<String, Value>) -> bool {
        self.inner()
            .outbound
            .as_ref()
            .is_some_and(|outbound| outbound.try_send(message).is_ok())
    }

    /// Sends `op` with `args` and waits up to 30 s for its `managed` reply.
    ///
    /// # Errors
    /// Android is offline, busy, gone, silent, or answered with an error.
    pub async fn request(
        &self,
        op: &str,
        mut args: Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let (tx, rx) = async_channel::bounded(1);
        let id = {
            let mut inner = self.inner();
            let outbound = inner.outbound.clone().ok_or("Android isn't running")?;
            if inner.pending.len() >= MAX_PENDING {
                return Err("busy".into());
            }
            inner.next = inner.next.wrapping_add(1);
            let id = format!("{:016x}", inner.next);
            args.insert("t".into(), Value::String("manage".into()));
            args.insert("id".into(), Value::String(id.clone()));
            args.insert("op".into(), Value::String(op.into()));
            let generation = inner.generation;
            inner.pending.insert(
                id.clone(),
                Pending {
                    generation,
                    sender: tx,
                },
            );
            if outbound.try_send(args).is_err() {
                inner.pending.remove(&id);
                return Err("Android stopped".into());
            }
            id
        };
        let reply = future::or(async { rx.recv().await.ok() }, async {
            Timer::after(TIMEOUT).await;
            None
        })
        .await;
        self.inner().pending.remove(&id);
        reply.unwrap_or_else(|| Err("Android didn't answer".into()))
    }

    /// Takes `managed` replies from DeckardSystem `generation`.
    pub fn handle(&self, generation: u64, message: &Map<String, Value>) {
        if message.get("t").and_then(Value::as_str) != Some("managed") {
            return;
        }
        let Some(id) = message.get("id").and_then(Value::as_str) else {
            return;
        };
        let mut inner = self.inner();
        if inner.generation != generation {
            return;
        }
        let Some(pending) = inner.pending.remove(id) else {
            return;
        };
        if pending.generation != generation {
            return;
        }
        let result = if message.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(message.clone())
        } else {
            let error = message.get("error").and_then(Value::as_str).unwrap_or("");
            Err(validate::clean_text(error, 256))
        };
        let _ = pending.sender.try_send(result);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixed local protocol fixtures")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn correlates_replies_and_rejects_stale_generations() {
        let manage = Manage::default();
        let (tx, rx) = async_channel::bounded(2);
        let old = manage.connect(tx);
        let caller = manage.clone();
        let request =
            std::thread::spawn(move || future::block_on(caller.request("app", Map::new())));
        let sent = rx.recv_blocking().unwrap();
        assert_eq!(sent["t"], "manage");
        assert_eq!(sent["op"], "app");
        assert_eq!(sent["id"].as_str().unwrap().len(), 16);
        manage.handle(
            old,
            json!({"t":"managed", "id":"unknown", "ok":true})
                .as_object()
                .unwrap(),
        );
        let mut reply = json!({"t":"managed", "id":sent["id"], "ok":true, "pkg":"com.example"});
        manage.handle(old, reply.as_object().unwrap());
        assert_eq!(request.join().unwrap().unwrap()["pkg"], "com.example");
        let (next_tx, _) = async_channel::bounded(1);
        let next = manage.connect(next_tx);
        reply["ok"] = json!(false);
        manage.handle(old, reply.as_object().unwrap());
        assert!(manage.connected());
        manage.disconnect(old);
        assert!(manage.connected());
        manage.disconnect(next);
        assert!(!manage.connected());
    }

    #[test]
    fn disconnect_fails_pending_and_android_error_is_bounded() {
        let manage = Manage::default();
        let (tx, rx) = async_channel::bounded(1);
        let generation = manage.connect(tx);
        let caller = manage.clone();
        let request =
            std::thread::spawn(move || future::block_on(caller.request("clear", Map::new())));
        let sent = rx.recv_blocking().unwrap();
        let reply = json!({"t":"managed", "id":sent["id"], "ok":false, "error":"x".repeat(300)});
        manage.handle(generation, reply.as_object().unwrap());
        assert_eq!(request.join().unwrap(), Err("x".repeat(256)));
        let caller = manage.clone();
        let request =
            std::thread::spawn(move || future::block_on(caller.request("clear", Map::new())));
        let _ = rx.recv_blocking().unwrap();
        manage.disconnect(generation);
        assert_eq!(request.join().unwrap(), Err("Android stopped".into()));
    }
}
