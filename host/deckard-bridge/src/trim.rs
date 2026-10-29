//! Hands an idle Android's memory to the phone's compressed swap. Once no
//! Android app has been in front for a minute, the container's cgroup is
//! asked to reclaim, and again every ten minutes while none is. Android keeps
//! running; what it touches next is paged back in (about 250 ms more on an
//! app's cold start on the HackberryPi, for ~1.2 GB less resident).

use zbus::runtime::traits::TaskHandle as _;

use std::fs;
use std::io::ErrorKind;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use deckard_core::keel::APP_ID_PREFIX;
use deckard_core::sync;
use zbus::Connection;

use crate::intents;

const IDLE: Duration = Duration::from_secs(60);
const AGAIN: Duration = Duration::from_secs(600);
const MB: u64 = 1024 * 1024;

#[derive(Default)]
struct State {
    // Unknown until the foreground watcher has read the Shell's state.
    android: Option<bool>,
    /// Counts the times an Android app came to or left the front.
    changes: u64,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

/// Follows the Shell's foreground app and trims `cgroup` while it is not an
/// Android app.
pub fn start(bus: &Connection, cgroup: PathBuf) {
    let shared: Shared = Arc::default();
    let seen = Arc::clone(&shared);
    let follower = bus.clone();
    let follow = async move {
        let follow = intents::follow_foreground(&follower, |app| {
            let (state, wake) = &*seen;
            let mut state = sync::lock(state);
            let android = Some(app.starts_with(APP_ID_PREFIX));
            if state.android != android {
                state.android = android;
                state.changes += 1;
                wake.notify_all();
            }
            ControlFlow::Continue(())
        });
        let result = follow.await;
        // Losing the watcher must not turn an unknown foreground into idle.
        let (state, wake) = &*seen;
        let mut state = sync::lock(state);
        state.android = None;
        state.changes += 1;
        wake.notify_all();
        if let Err(error) = result {
            eprintln!("deckard-bridge: idle trim has no foreground changes: {error}");
        }
    };
    bus.spawn("deckard-trim", follow).detach();
    thread::spawn(move || run(&shared, &cgroup));
}

fn run(shared: &Shared, cgroup: &Path) {
    let (state, wake) = &**shared;
    let mut guard = sync::lock(state);
    loop {
        while guard.android != Some(false) {
            guard = wake.wait(guard).unwrap_or_else(PoisonError::into_inner);
        }
        let idle_since = guard.changes;
        let mut delay = IDLE;
        loop {
            guard = wait(wake, guard, delay, idle_since);
            if guard.changes != idle_since {
                break;
            }
            drop(guard);
            trim(cgroup, || {
                let guard = sync::lock(state);
                guard.android == Some(false) && guard.changes == idle_since
            });
            guard = sync::lock(state);
            delay = AGAIN;
        }
    }
}

fn wait<'a>(
    wake: &Condvar,
    guard: MutexGuard<'a, State>,
    delay: Duration,
    idle_since: u64,
) -> MutexGuard<'a, State> {
    wake.wait_timeout_while(guard, delay, |state| state.changes == idle_since)
        .unwrap_or_else(PoisonError::into_inner)
        .0
}

fn trim(cgroup: &Path, still_idle: impl Fn() -> bool) {
    // No cgroup: Android is not running.
    let Some(before) = charged(cgroup) else {
        return;
    };
    let started = Instant::now();
    // Recheck foreground state between bounded requests. A single full-cgroup
    // reclaim can run for seconds after the user has returned to Android.
    let result = reclaim_steps(before, still_idle, |bytes| {
        fs::write(cgroup.join("memory.reclaim"), bytes.to_string())
    });
    if let Err(error) = result {
        eprintln!("deckard-bridge: idle trim failed: {error}");
        return;
    }
    let after = charged(cgroup).unwrap_or(before);
    eprintln!(
        "deckard-bridge: idle Android trimmed from {} to {} MB in {:.1} s",
        before / MB,
        after / MB,
        started.elapsed().as_secs_f64()
    );
}

fn charged(cgroup: &Path) -> Option<u64> {
    fs::read_to_string(cgroup.join("memory.current"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The original charge bounds work even when Android allocates while idle.
fn reclaim_steps(
    mut remaining: u64,
    still_idle: impl Fn() -> bool,
    mut reclaim: impl FnMut(u64) -> std::io::Result<()>,
) -> std::io::Result<()> {
    while remaining > 0 && still_idle() {
        let bytes = remaining.min(32 * MB);
        match reclaim(bytes) {
            Ok(()) => remaining -= bytes,
            // Nothing more can be reclaimed without waiting for new activity.
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn returning_to_android_stops_reclaim_after_current_step() -> std::io::Result<()> {
        let idle = Cell::new(true);
        let calls = Cell::new(0);
        reclaim_steps(
            1024 * MB,
            || idle.get(),
            |bytes| {
                assert_eq!(bytes, 32 * MB);
                calls.set(calls.get() + 1);
                idle.set(false);
                Ok(())
            },
        )?;
        assert_eq!(calls.get(), 1);
        Ok(())
    }

    #[test]
    fn exhausted_reclaim_stops_without_retrying_the_entire_budget() -> std::io::Result<()> {
        let mut calls = 0;
        reclaim_steps(
            1024 * MB,
            || true,
            |_| {
                calls += 1;
                Err(std::io::Error::from(ErrorKind::WouldBlock))
            },
        )?;
        assert_eq!(calls, 1);
        Ok(())
    }

    #[test]
    fn reclaim_is_bounded_by_the_initial_charge() -> std::io::Result<()> {
        let mut total = 0;
        reclaim_steps(
            65 * MB,
            || true,
            |bytes| {
                assert!(bytes <= 32 * MB);
                total += bytes;
                Ok(())
            },
        )?;
        assert_eq!(total, 65 * MB);
        Ok(())
    }
}
