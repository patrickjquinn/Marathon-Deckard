//! Asks DeckardSystem to end Android's least recently used cached processes
//! when memory runs short, the job lmkd cannot do on this kernel: the
//! container going over `memory.high` or `memory.max`, Android stalling on
//! memory inside the container, or the whole phone stalling.

use std::fs;
use std::io;
use std::mem;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use deckard_core::pressure::{self, Reason, ReclaimPolicy};
use deckard_core::sync;
use deckard_core::validate::clean_text;
use rustix::event::{PollFd, PollFlags};
use rustix::fs::inotify::{CreateFlags, WatchFlags};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use serde_json::{Map, Value};

use crate::Outbound;

const MAX_LOGGED: usize = 8;

/// The watch thread, and the socket whose drop wakes it to exit.
type Watch = (UnixStream, JoinHandle<()>);

struct Inner {
    cgroup: PathBuf,
    system_pressure: PathBuf,
    send: Arc<Outbound>,
    policy: Mutex<ReclaimPolicy>,
    watch: Mutex<Option<Watch>>,
}

/// Watches memory pressure while DeckardSystem is connected.
#[derive(Clone)]
pub struct Reclaimer(Arc<Inner>);

#[derive(Clone, Copy)]
enum Source {
    Stop,
    Events,
    Stall(Reason),
}

impl Source {
    fn interest(self) -> PollFlags {
        match self {
            Self::Stall(_) => PollFlags::PRI,
            Self::Stop | Self::Events => PollFlags::IN,
        }
    }
}

impl Reclaimer {
    /// `cgroup` is the Android container's; `send` hands a message to
    /// DeckardSystem and says whether it was queued.
    pub fn new(cgroup: PathBuf, system_pressure: PathBuf, send: Arc<Outbound>) -> Self {
        Self(Arc::new(Inner {
            cgroup,
            system_pressure,
            send,
            policy: Mutex::default(),
            watch: Mutex::default(),
        }))
    }

    /// DeckardSystem connected: the container's cgroup is new on every
    /// Android start, so each connection arms afresh.
    pub fn connected(&self) {
        self.disarm();
        sync::lock(&self.0.policy).set_connected(true);
        let (stop, wake) = match UnixStream::pair() {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("deckard-bridge: reclaim cannot watch memory: {error}");
                return;
            }
        };
        let (sources, over) = self.arm(wake.into());
        let reclaimer = self.clone();
        let thread = thread::spawn(move || reclaimer.watch(sources, over));
        *sync::lock(&self.0.watch) = Some((stop, thread));
    }

    /// DeckardSystem went away: stops watching.
    pub fn disconnected(&self) {
        sync::lock(&self.0.policy).set_connected(false);
        self.disarm();
    }

    fn disarm(&self) {
        let watch = sync::lock(&self.0.watch).take();
        if let Some((stop, thread)) = watch {
            drop(stop);
            let _ = thread.join();
        }
    }

    /// Takes DeckardSystem's `reclaimed` answers; ignores other messages.
    pub fn handle(&self, message: &Map<String, Value>) {
        if message.get("t").and_then(Value::as_str) != Some("reclaimed") {
            return;
        }
        let ok = message.get("ok").and_then(Value::as_bool) == Some(true);
        let killed = message
            .get("killed")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        sync::lock(&self.0.policy).answered(ok, killed.len());
        let logged = killed.iter().take(MAX_LOGGED);
        for process in logged {
            let name = process.get("process").and_then(Value::as_str);
            eprintln!(
                "deckard-bridge: reclaim ended {}",
                clean_text(name.unwrap_or(""), 128)
            );
        }
        if !ok {
            let error = message.get("error").and_then(Value::as_str).unwrap_or("");
            eprintln!("deckard-bridge: reclaim failed: {}", clean_text(error, 256));
        }
    }

    fn request(&self, reason: Reason) {
        let message = sync::lock(&self.0.policy).request(reason, Instant::now());
        if let Some(message) = message {
            (self.0.send)(message);
        }
    }

    /// The sources to poll, and the over-budget count they start from.
    fn arm(&self, stop: OwnedFd) -> (Vec<(OwnedFd, Source)>, u64) {
        let events = self.0.cgroup.join("memory.events");
        let over = read_over(&events).unwrap_or(0);
        let mut sources = vec![(stop, Source::Stop)];
        match watch_events(&events) {
            Ok(fd) => sources.push((fd, Source::Events)),
            Err(error) => eprintln!("deckard-bridge: cannot watch {}: {error}", events.display()),
        }
        let container = self.0.cgroup.join("memory.pressure");
        let stalls = [
            (
                container.as_path(),
                pressure::CONTAINER_TRIGGER,
                Reason::ContainerStall,
            ),
            (
                &self.0.system_pressure,
                pressure::SYSTEM_TRIGGER,
                Reason::PhoneStall,
            ),
        ];
        for (path, spec, reason) in stalls {
            match trigger(path, spec) {
                Ok(fd) => sources.push((fd, Source::Stall(reason))),
                Err(error) => eprintln!(
                    "deckard-bridge: no PSI trigger on {}: {error}",
                    path.display()
                ),
            }
        }
        (sources, over)
    }

    fn watch(&self, mut sources: Vec<(OwnedFd, Source)>, mut over: u64) {
        let events = self.0.cgroup.join("memory.events");
        loop {
            let mut fds: Vec<PollFd<'_>> = sources
                .iter()
                .map(|(fd, source)| PollFd::new(fd, source.interest()))
                .collect();
            match rustix::event::poll(&mut fds, None) {
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(error) => {
                    eprintln!("deckard-bridge: reclaim poll failed: {error}");
                    return;
                }
            }
            let ready: Vec<PollFlags> = fds.iter().map(PollFd::revents).collect();
            let polled = mem::take(&mut sources).into_iter().zip(ready);
            for ((fd, source), revents) in polled {
                if revents.is_empty() {
                    sources.push((fd, source));
                    continue;
                }
                match source {
                    Source::Stop => return,
                    _ if revents.intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL) => {
                        continue;
                    }
                    Source::Events => {
                        drain(&fd);
                        if let Some(now) = read_over(&events) {
                            if now > over {
                                self.request(Reason::ContainerHigh);
                            }
                            over = now;
                        }
                    }
                    Source::Stall(reason) => self.request(reason),
                }
                sources.push((fd, source));
            }
        }
    }
}

fn read_over(events: &Path) -> Option<u64> {
    fs::read_to_string(events)
        .ok()
        .map(|events| pressure::over_budget(&events))
}

fn watch_events(events: &Path) -> io::Result<OwnedFd> {
    let fd = rustix::fs::inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
    rustix::fs::inotify::add_watch(&fd, events, WatchFlags::MODIFY)?;
    Ok(fd)
}

fn drain(fd: &OwnedFd) {
    let mut buffer = [0_u8; 4096];
    while rustix::io::read(fd, &mut buffer).is_ok_and(|count| count > 0) {}
}

fn trigger(path: &Path, spec: &str) -> io::Result<OwnedFd> {
    let flags = OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::open(path, flags, Mode::empty())?;
    rustix::io::write(&fd, format!("{spec}\0").as_bytes())?;
    Ok(fd)
}
