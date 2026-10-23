//! Authenticated, framed Android app socket with replaceable connections.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use deckard_core::ids::PackageName;
use deckard_core::sync;
use deckard_core::transport::{self, BridgeListener, BridgeSession};
use serde_json::{Map, Value};

const READ_POLL: Duration = Duration::from_millis(100);
const OUTBOUND_CAPACITY: usize = 256;
/// One rate-limiter burst. While the consumer is behind, further messages
/// are dropped as the limiter drops frames; past `transport::MAX_DROPPED` the
/// peer is disconnected as a flood.
pub const INBOUND_CAPACITY: usize = transport::BURST as usize;

/// What happens on the socket, by connection generation.
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

type Current = Arc<Mutex<Option<Active>>>;

/// Sends to the connected peer; cheap to clone.
#[derive(Clone)]
pub struct ChannelHandle {
    active: Current,
}

impl ChannelHandle {
    /// The connected peer's generation.
    #[must_use]
    pub fn generation(&self) -> Option<u64> {
        sync::lock(&self.active)
            .as_ref()
            .map(|active| active.generation)
    }

    /// Queues `message` for the connected peer.
    ///
    /// # Errors
    /// As [`Self::send_to`], or `NotConnected` when no peer is connected.
    pub fn send(&self, message: Map<String, Value>) -> io::Result<()> {
        let offline = || io::Error::new(io::ErrorKind::NotConnected, "Android bridge is offline");
        self.send_to(self.generation().ok_or_else(offline)?, message)
    }

    /// Queues `message` for peer `generation`, so a late reply never reaches
    /// a replaced Android process. A peer that lets its queue fill is
    /// disconnected.
    ///
    /// # Errors
    /// `NotConnected` when that peer is gone, `WouldBlock` when its queue
    /// is full.
    pub fn send_to(&self, generation: u64, message: Map<String, Value>) -> io::Result<()> {
        let guard = sync::lock(&self.active);
        let active = guard
            .as_ref()
            .filter(|active| active.generation == generation);
        let gone = |text| io::Error::new(io::ErrorKind::NotConnected, text);
        let active = active.ok_or_else(|| gone("Android peer replaced"))?;
        match active.outbound.try_send(message) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                active.stop.store(true, Ordering::Release);
                let text = "Android peer is not draining output";
                Err(io::Error::new(io::ErrorKind::WouldBlock, text))
            }
            Err(TrySendError::Disconnected(_)) => Err(gone("Android peer disconnected")),
        }
    }
}

/// The listening socket for one Android package.
pub struct AppChannel {
    listener: BridgeListener,
    package: PackageName,
    active: Current,
    events: SyncSender<ChannelEvent>,
}

impl AppChannel {
    /// Binds `socket_path` for the uid `packages_list` gives `package`.
    ///
    /// # Errors
    /// The socket could not be bound.
    pub fn bind(
        socket_path: impl AsRef<Path>,
        packages_list: impl AsRef<Path>,
        package: PackageName,
    ) -> io::Result<(Self, ChannelHandle, Receiver<ChannelEvent>)> {
        let listener = BridgeListener::bind(socket_path, packages_list, package.clone())?;
        let active = Current::default();
        let (events, receiver) = mpsc::sync_channel(INBOUND_CAPACITY);
        let handle = ChannelHandle {
            active: Arc::clone(&active),
        };
        let channel = Self {
            listener,
            package,
            active,
            events,
        };
        Ok((channel, handle, receiver))
    }

    /// Runs on a dedicated accept thread. Bad peers are rejected without
    /// displacing the current session, as in the C++ bridge.
    ///
    /// # Errors
    /// `BrokenPipe` once the event receiver is gone.
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
            eprintln!("deckard-bridge: {} connected", self.package.as_str());
            let (outbound, receiver) = mpsc::sync_channel(OUTBOUND_CAPACITY);
            let stop = Arc::new(AtomicBool::new(false));
            let next = Active {
                generation,
                outbound,
                stop: Arc::clone(&stop),
            };
            let previous = sync::lock(&self.active).replace(next);
            if let Some(previous) = previous {
                previous.stop.store(true, Ordering::Release);
            }
            let closed = |_| io::Error::new(io::ErrorKind::BrokenPipe, "channel events closed");
            self.events
                .send(ChannelEvent::Connected(generation))
                .map_err(closed)?;
            let events = self.events.clone();
            let active = Arc::clone(&self.active);
            thread::spawn(move || serve(session, receiver, &events, &active, &stop));
        }
    }
}

fn serve(
    mut session: BridgeSession,
    outbound: Receiver<Map<String, Value>>,
    events: &SyncSender<ChannelEvent>,
    active: &Mutex<Option<Active>>,
    stop: &AtomicBool,
) {
    let generation = session.generation();
    let mut dropped = 0_u32;
    let started = session.set_read_timeout(Some(READ_POLL));
    if let Ok(mut writer) = started.and_then(|()| session.writer()) {
        thread::spawn(move || {
            while let Ok(message) = outbound.recv() {
                if writer.send_message(&message).is_err() {
                    break;
                }
            }
            writer.close();
        });
        while !stop.load(Ordering::Acquire) {
            let message = match session.read_message() {
                Ok(Some(message)) => message,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Ok(None) | Err(_) => break,
            };
            match events.try_send(ChannelEvent::Message(generation, message)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    dropped += 1;
                    if dropped > transport::MAX_DROPPED {
                        eprintln!("deckard-bridge: Android peer floods a stalled consumer");
                        break;
                    }
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
    }
    let mut guard = sync::lock(active);
    if guard
        .as_ref()
        .is_some_and(|current| current.generation == generation)
    {
        guard.take();
    }
    drop(guard);
    let _ = events.send(ChannelEvent::Disconnected(generation));
}
