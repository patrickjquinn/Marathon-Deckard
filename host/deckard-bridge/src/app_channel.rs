//! Authenticated, framed Android app socket with replaceable connections.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use deckard_core::framing;
use deckard_core::ids::PackageName;
use deckard_core::transport::{BridgeListener, BridgeSession};
use serde_json::{Map, Value};

const READ_POLL: Duration = Duration::from_millis(100);
const OUTBOUND_CAPACITY: usize = 256;
/// One rate-limiter burst. While the consumer is behind, further messages
/// are dropped as the limiter drops frames; past `framing::MAX_DROPPED` the
/// peer is disconnected as a flood.
pub const INBOUND_CAPACITY: usize = framing::BURST as usize;

#[derive(Debug)]
pub enum ChannelEvent {
    Connected(u64),
    Message(u64, Map<String, Value>),
    Disconnected(u64),
}

struct Active {
    generation: u64,
    outbound: SyncSender<Map<String, Value>>,
    stop: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct ChannelHandle {
    active: Arc<Mutex<Option<Active>>>,
}

impl ChannelHandle {
    pub fn generation(&self) -> Option<u64> {
        self.active
            .lock()
            .ok()?
            .as_ref()
            .map(|active| active.generation)
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.generation() == Some(generation)
    }

    pub fn send(&self, message: Map<String, Value>) -> io::Result<()> {
        let generation = self.generation().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Android bridge is offline")
        })?;
        self.send_to(generation, message)
    }

    /// Rejects late replies intended for a replaced Android process.
    pub fn send_to(&self, generation: u64, message: Map<String, Value>) -> io::Result<()> {
        let guard = self
            .active
            .lock()
            .map_err(|_| io::Error::other("Android bridge lock poisoned"))?;
        let active = guard
            .as_ref()
            .filter(|active| active.generation == generation)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "Android peer replaced"))?;
        match active.outbound.try_send(message) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                active.stop.store(true, Ordering::Release);
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Android peer is not draining output",
                ))
            }
            Err(TrySendError::Disconnected(_)) => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Android peer disconnected",
            )),
        }
    }
}

pub struct AppChannel {
    listener: BridgeListener,
    active: Arc<Mutex<Option<Active>>>,
    events: SyncSender<ChannelEvent>,
}

impl AppChannel {
    pub fn bind(
        socket_path: impl AsRef<Path>,
        packages_list: impl AsRef<Path>,
        package: PackageName,
    ) -> io::Result<(Self, ChannelHandle, Receiver<ChannelEvent>)> {
        let listener = BridgeListener::bind(socket_path, packages_list, package)?;
        let active = Arc::new(Mutex::new(None));
        let (events, receiver) = mpsc::sync_channel(INBOUND_CAPACITY);
        let handle = ChannelHandle {
            active: Arc::clone(&active),
        };
        Ok((
            Self {
                listener,
                active,
                events,
            },
            handle,
            receiver,
        ))
    }

    /// Runs on a dedicated accept thread. Bad peers are rejected without
    /// displacing the current session, as in the C++ bridge.
    pub fn run(mut self) -> io::Result<()> {
        loop {
            let session = match self.listener.accept() {
                Ok(session) => session,
                Err(error) => {
                    eprintln!("deckard-bridge: rejected Android peer: {error}");
                    continue;
                }
            };
            let generation = session.generation();
            let (outbound, receiver) = mpsc::sync_channel(OUTBOUND_CAPACITY);
            let stop = Arc::new(AtomicBool::new(false));
            {
                let mut guard = self
                    .active
                    .lock()
                    .map_err(|_| io::Error::other("Android bridge lock poisoned"))?;
                if let Some(previous) = guard.replace(Active {
                    generation,
                    outbound,
                    stop: Arc::clone(&stop),
                }) {
                    previous.stop.store(true, Ordering::Release);
                }
            }
            self.events
                .send(ChannelEvent::Connected(generation))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "channel events closed"))?;
            let events = self.events.clone();
            let active = Arc::clone(&self.active);
            thread::spawn(move || serve(session, receiver, events, active, stop));
        }
    }
}

fn serve(
    mut session: BridgeSession,
    outbound: Receiver<Map<String, Value>>,
    events: SyncSender<ChannelEvent>,
    active: Arc<Mutex<Option<Active>>>,
    stop: Arc<AtomicBool>,
) {
    let generation = session.generation();
    let mut dropped = 0_u32;
    let started = session
        .set_read_timeout(Some(READ_POLL))
        .and_then(|()| session.writer());
    if let Ok(mut writer) = started {
        thread::spawn(move || {
            while let Ok(message) = outbound.recv() {
                if writer.send_message(&message).is_err() {
                    break;
                }
            }
            writer.close();
        });
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            match session.read_message() {
                Ok(Some(message)) => {
                    match events.try_send(ChannelEvent::Message(generation, message)) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            dropped += 1;
                            if dropped > framing::MAX_DROPPED {
                                eprintln!("deckard-bridge: Android peer floods a stalled consumer");
                                break;
                            }
                        }
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                }
                Ok(None) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
        }
    }
    if let Ok(mut guard) = active.lock()
        && guard
            .as_ref()
            .is_some_and(|current| current.generation == generation)
    {
        guard.take();
    }
    let _ = events.send(ChannelEvent::Disconnected(generation));
}
