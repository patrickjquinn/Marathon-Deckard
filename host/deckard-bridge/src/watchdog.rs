//! Restarts an Android session that never finishes booting, or that lost
//! its display because the Shell went away and came back.

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use deckard_core::runtime_state::{
    self, BOOT_TIMEOUT_MS, BootTimer, EVIDENCE_TIMEOUT_MS, SHELL_BUS_NAME, SHELL_SETTLE_MS,
    SessionFollower, Stall,
};
use zbus::blocking::Connection;

const EVENT_CAPACITY: usize = 16;
const EVIDENCE_LOG_LIMIT: u64 = 256 * 1024;
const EVIDENCE_POLL: Duration = Duration::from_millis(100);

enum Event {
    ActiveSince(i64),
    EvidenceFinished,
}

/// Handle to the boot watchdog thread.
#[derive(Clone)]
pub struct BootWatchdog(SyncSender<Event>);

impl BootWatchdog {
    /// Starts the watchdog. Android writes `marker` at `sys.boot_completed`;
    /// a boot that has not touched it 60 s after the unit became active has
    /// `evidence`'s output logged and gets one `restart`.
    pub fn spawn(
        marker: PathBuf,
        evidence: Option<Vec<OsString>>,
        restart: impl Fn() + Send + 'static,
    ) -> Self {
        let (sender, events) = mpsc::sync_channel(EVENT_CAPACITY);
        let feedback = sender.clone();
        thread::spawn(move || run(&marker, evidence.as_deref(), &restart, &events, &feedback));
        Self(sender)
    }

    /// The session unit's activation time, ms since the epoch; 0 while it
    /// isn't active. Waits while the watchdog is busy restarting.
    pub fn set_active_since(&self, active_since_ms: i64) {
        let _ = self.0.send(Event::ActiveSince(active_since_ms));
    }
}

fn epoch_ms(time: SystemTime) -> Option<i64> {
    let since = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

fn modified_ms(path: &Path) -> Option<i64> {
    epoch_ms(fs::metadata(path).ok()?.modified().ok()?)
}

fn run(
    marker: &Path,
    evidence: Option<&[OsString]>,
    restart: &dyn Fn(),
    events: &Receiver<Event>,
    feedback: &SyncSender<Event>,
) {
    let mut watchdog = runtime_state::BootWatchdog::default();
    let mut timer: Option<Instant> = None;
    let restart = || {
        eprintln!("deckard-bridge: restarting the Android session");
        restart();
    };
    loop {
        let event = match timer {
            Some(at) => events.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => events.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match event {
            Ok(Event::ActiveSince(ms)) => {
                let now = epoch_ms(SystemTime::now()).unwrap_or(0);
                match watchdog.set_active_since(ms, now) {
                    BootTimer::Unchanged => {}
                    BootTimer::Cancelled => timer = None,
                    BootTimer::Armed(after) => timer = Some(Instant::now() + after),
                }
            }
            Ok(Event::EvidenceFinished) => {
                if watchdog.evidence_finished(modified_ms(marker)) {
                    restart();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                timer = None;
                let stall = watchdog.on_boot_timeout(modified_ms(marker), evidence.is_some());
                if stall != Stall::None {
                    eprintln!(
                        "deckard-bridge: Android has not finished booting {} s after its start",
                        BOOT_TIMEOUT_MS / 1000
                    );
                }
                match (stall, evidence) {
                    (Stall::Collect, Some(command)) => {
                        collect_evidence(command.to_vec(), feedback.clone());
                    }
                    (Stall::Restart, _) => restart(),
                    _ => {}
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Runs the evidence helper on its own thread, killing it after
/// `EVIDENCE_TIMEOUT_MS`, and reports back when it is gone.
fn collect_evidence(command: Vec<OsString>, done: SyncSender<Event>) {
    thread::spawn(move || {
        if let Err(error) = run_evidence(&command) {
            eprintln!("deckard-bridge: cannot run the boot evidence helper: {error}");
        }
        let _ = done.send(Event::EvidenceFinished);
    });
}

fn run_evidence(command: &[OsString]) -> io::Result<()> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let (output, writer) = io::pipe()?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer)
        .spawn()?;
    thread::spawn(move || log_evidence(output));
    let deadline = Instant::now() + Duration::from_millis(EVIDENCE_TIMEOUT_MS);
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            child.wait()?;
            break;
        }
        thread::sleep(EVIDENCE_POLL);
    }
    Ok(())
}

/// Logs the first `EVIDENCE_LOG_LIMIT` bytes line by line, then drains the
/// rest so the helper never blocks on a full pipe.
fn log_evidence(output: impl Read) {
    let mut output = BufReader::new(output);
    let lines = (&mut output).take(EVIDENCE_LOG_LIMIT).split(b'\n');
    for line in lines {
        let Ok(line) = line else { return };
        if !line.is_empty() {
            eprintln!("deckard-bridge: {}", String::from_utf8_lossy(&line));
        }
    }
    let _ = io::copy(&mut output, &mut io::sink());
}

/// Calls `restart` once the Shell's bus name returns after it was lost,
/// since the composer HAL aborts with its Wayland server. A Shell crash loop
/// restarts Android at most once every 30 s.
///
/// # Errors
/// Returns the D-Bus error that prevented the subscription.
pub fn follow_shell(
    bus: &Connection,
    restart: impl Fn() + Send + Sync + 'static,
) -> zbus::Result<()> {
    let rule = crate::owner_changes(SHELL_BUS_NAME)?;
    let mut follower = SessionFollower::default();
    let restart = Arc::new(restart);
    let start = Instant::now();
    crate::relay(bus, rule, 8, move |signal| {
        let Ok((name, _, owner)) = signal.body().deserialize::<(String, String, String)>() else {
            return true;
        };
        let now = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        if follower.on_name_owner_changed(&name, !owner.is_empty(), now) {
            let restart = Arc::clone(&restart);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(SHELL_SETTLE_MS));
                eprintln!("deckard-bridge: the Shell restarted; restarting the Android session");
                restart();
            });
        }
        true
    })
}
