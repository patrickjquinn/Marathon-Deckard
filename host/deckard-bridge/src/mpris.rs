//! MediaSession wiring for the authenticated Android app channel.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use deckard_mpris::{Error, MprisAdapter};
use serde_json::{Map, Value};

const MAX_COMMANDS_PER_DRAIN: usize = 64;

struct Active {
    generation: u64,
    adapter: MprisAdapter,
    commands: Receiver<Value>,
}

/// Only feed events from `ChannelEvent::Message` after its generation has
/// matched the authenticated app socket. Never trust a JSON-supplied route.
pub struct MediaBridge {
    bus_address: Option<String>,
    art_dir: PathBuf,
    active: Option<Active>,
}

impl MediaBridge {
    pub fn new_session(art_dir: impl Into<PathBuf>) -> Self {
        Self::new_at(None, art_dir)
    }

    /// `bus_address` is for private-bus tests; production uses the session bus.
    pub fn new_at(bus_address: Option<&str>, art_dir: impl Into<PathBuf>) -> Self {
        Self {
            bus_address: bus_address.map(str::to_owned),
            art_dir: art_dir.into(),
            active: None,
        }
    }

    pub fn connected(&mut self, generation: u64) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation)
        {
            return;
        }
        self.active = None;
        let (sender, commands) = mpsc::channel();
        self.active = Some(Active {
            generation,
            adapter: MprisAdapter::new_at(self.bus_address.as_deref(), &self.art_dir, sender),
            commands,
        });
    }

    pub fn handle_authenticated(
        &mut self,
        generation: u64,
        message: &Map<String, Value>,
    ) -> Result<(), Error> {
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.generation == generation)
        else {
            return Ok(());
        };
        let kind = message.get("t").and_then(Value::as_str).unwrap_or("");
        if matches!(kind, "media" | "media_gone") {
            active.adapter.handle(kind, message)?;
        }
        Ok(())
    }

    pub fn disconnected(&mut self, generation: u64) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation)
        {
            self.active = None;
        }
    }

    /// The caller must pass each tuple to `ChannelHandle::send_to`. The
    /// generation check there also protects commands racing a disconnect.
    pub fn drain_commands(&self) -> Vec<(u64, Map<String, Value>)> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        let mut commands = Vec::new();
        for _ in 0..MAX_COMMANDS_PER_DRAIN {
            match active.commands.try_recv() {
                Ok(Value::Object(command)) => commands.push((active.generation, command)),
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        commands
    }

    pub fn active_players(&self) -> usize {
        self.active
            .as_ref()
            .map_or(0, |active| active.adapter.active_players())
    }
}
