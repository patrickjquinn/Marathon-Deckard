//! Policy for deckard-bridge's runtime, boot watchdog and Shell follower.
//! The adapters own clocks, timers, I/O and commands.

use std::time::Duration;

pub const BOOT_TIMEOUT_MS: u64 = 60_000;
pub const EVIDENCE_TIMEOUT_MS: u64 = 90_000;
pub const SHELL_SETTLE_MS: u64 = 2_000;
const SHELL_MIN_GAP_MS: u64 = 30_000;
pub const SHELL_BUS_NAME: &str = "org.marathonos.Shell";

/// Android as Settings shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    Running,
    Stopping,
}

/// A systemd call on the session unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitCall {
    Start,
    Stop,
    Restart,
}

/// Why a reset cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetError {
    AlreadyResetting,
    Unavailable,
}

/// What Settings shows about the runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub state: State,
    pub memory_bytes: i64,
    pub android_version: String,
    pub uptime_seconds: i64,
    pub start_at_login: bool,
    pub has_android_apps: bool,
}

/// The session unit and Android as Settings shows them. Setters return
/// whether anything changed.
#[derive(Debug)]
pub struct RuntimeState {
    unit_state: String,
    entered_ms: i64,
    android_connected: bool,
    android_version: String,
    start_at_login: Option<bool>,
    has_android_apps: bool,
    resetting: bool,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            unit_state: "inactive".into(),
            entered_ms: 0,
            android_connected: false,
            android_version: String::new(),
            start_at_login: None,
            has_android_apps: false,
            resetting: false,
        }
    }
}

impl RuntimeState {
    #[must_use]
    pub fn state(&self) -> State {
        match self.unit_state.as_str() {
            "active" | "reloading" if self.android_connected => State::Running,
            "active" | "reloading" | "activating" => State::Starting,
            "deactivating" => State::Stopping,
            _ => State::Stopped,
        }
    }

    /// When the unit became active, ms since the epoch; 0 while it isn't.
    #[must_use]
    pub fn active_since_ms(&self) -> i64 {
        match self.unit_state.as_str() {
            "active" | "reloading" => self.entered_ms,
            _ => 0,
        }
    }

    #[must_use]
    pub fn snapshot(&self, now_ms: i64, observed_memory_bytes: i64) -> RuntimeSnapshot {
        let state = self.state();
        let running = state == State::Running && self.entered_ms > 0;
        RuntimeSnapshot {
            state,
            memory_bytes: if state == State::Stopped {
                0
            } else {
                observed_memory_bytes.max(0)
            },
            android_version: self.android_version.clone(),
            uptime_seconds: if running {
                now_ms.saturating_sub(self.entered_ms).max(0) / 1_000
            } else {
                0
            },
            start_at_login: self.start_at_login(),
            has_android_apps: self.has_android_apps,
        }
    }

    /// The saved choice, or whether Android apps are installed.
    #[must_use]
    pub fn start_at_login(&self) -> bool {
        self.start_at_login.unwrap_or(self.has_android_apps)
    }

    pub fn set_has_android_apps(&mut self, has_apps: bool) {
        self.has_android_apps = has_apps;
    }

    pub fn set_start_at_login(&mut self, enabled: bool) {
        self.start_at_login = Some(enabled);
    }

    /// Whether to start the unit at login.
    #[must_use]
    pub fn at_login(&self) -> bool {
        self.start_at_login() && self.state() == State::Stopped
    }

    pub fn set_unit(&mut self, active_state: &str, entered_ms: i64) -> bool {
        let changed = self.unit_state != active_state || self.entered_ms != entered_ms;
        self.unit_state = active_state.into();
        self.entered_ms = entered_ms;
        changed
    }

    pub fn set_android_connected(&mut self, connected: bool) -> bool {
        let changed = self.android_connected != connected;
        self.android_connected = connected;
        changed
    }

    pub fn set_android_version(&mut self, version: &str) -> bool {
        let changed = self.android_version != version;
        self.android_version = version.into();
        changed
    }

    /// Starts a reset, which its caller ends with [`Self::finish_reset`].
    ///
    /// # Errors
    /// A reset is under way, or there is no helper.
    pub fn request_reset(&mut self, helper_available: bool) -> Result<(), ResetError> {
        if self.resetting {
            return Err(ResetError::AlreadyResetting);
        }
        if !helper_available {
            return Err(ResetError::Unavailable);
        }
        self.resetting = true;
        Ok(())
    }

    pub fn finish_reset(&mut self) {
        self.resetting = false;
    }
}

/// The boot timer after a change of the unit's activation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootTimer {
    Unchanged,
    Cancelled,
    Armed(Duration),
}

/// What a boot timeout calls for. Every variant but `None` is a stall to
/// report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stall {
    /// Android booted, or evidence of an earlier stall is being collected.
    None,
    /// The restarted attempt stalled too.
    Report,
    /// Collect evidence, then call [`BootWatchdog::evidence_finished`].
    Collect,
    Restart,
}

/// Restarts a stalled boot once until a boot completes. The adapter
/// supplies the boot marker's modification time, ms since the epoch;
/// `None` when it is missing or unreadable.
#[derive(Debug, Default)]
pub struct BootWatchdog {
    active: bool,
    attempt_ms: i64,
    stalled_attempt_ms: i64,
    restarted: bool,
    collecting: bool,
}

impl BootWatchdog {
    #[must_use]
    pub fn set_active_since(&mut self, active_since_ms: i64, now_ms: i64) -> BootTimer {
        self.active = active_since_ms > 0;
        if !self.active {
            return BootTimer::Cancelled;
        }
        if active_since_ms == self.attempt_ms {
            return BootTimer::Unchanged;
        }
        self.attempt_ms = active_since_ms;
        let remaining = active_since_ms
            .saturating_add(BOOT_TIMEOUT_MS as i64)
            .saturating_sub(now_ms);
        BootTimer::Armed(Duration::from_millis(
            remaining.clamp(0, BOOT_TIMEOUT_MS as i64) as u64,
        ))
    }

    #[must_use]
    pub fn on_boot_timeout(&mut self, marker_ms: Option<i64>, evidence_available: bool) -> Stall {
        if self.booted(marker_ms) {
            self.restarted = false;
            return Stall::None;
        }
        if self.collecting {
            return Stall::None;
        }
        self.stalled_attempt_ms = self.attempt_ms;
        if evidence_available {
            self.collecting = true;
            Stall::Collect
        } else if self.restart(marker_ms) {
            Stall::Restart
        } else {
            Stall::Report
        }
    }

    /// Whether to restart now that the evidence is in.
    #[must_use]
    pub fn evidence_finished(&mut self, marker_ms: Option<i64>) -> bool {
        self.collecting = false;
        self.restart(marker_ms)
    }

    fn booted(&self, marker_ms: Option<i64>) -> bool {
        marker_ms.is_some_and(|modified| modified >= self.attempt_ms)
    }

    fn restart(&mut self, marker_ms: Option<i64>) -> bool {
        let stalled = self.active && self.attempt_ms == self.stalled_attempt_ms;
        let restart = stalled && !self.booted(marker_ms) && !self.restarted;
        self.restarted |= restart;
        restart
    }
}

/// Follows the Shell's bus name. `now_ms` is a monotonic clock.
#[derive(Debug, Default)]
pub struct SessionFollower {
    lost: bool,
    last_restart_ms: Option<u64>,
}

impl SessionFollower {
    /// Whether the Shell came back after it was lost, and no restart was
    /// asked for in the last [`SHELL_MIN_GAP_MS`]: the adapter restarts the
    /// session [`SHELL_SETTLE_MS`] later. A suppressed return still clears
    /// the loss.
    #[must_use]
    pub fn on_name_owner_changed(&mut self, name: &str, has_new_owner: bool, now_ms: u64) -> bool {
        if name != SHELL_BUS_NAME {
            return false;
        }
        if !has_new_owner {
            self.lost = true;
            return false;
        }
        let recent = self
            .last_restart_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < SHELL_MIN_GAP_MS);
        if !std::mem::take(&mut self.lost) || recent {
            return false;
        }
        self.last_restart_ms = Some(now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_maps_unit_and_android_readiness_without_optimism() {
        let mut runtime = RuntimeState::default();
        assert_eq!(runtime.state(), State::Stopped);
        assert_eq!(runtime.snapshot(100_000, 500).memory_bytes, 0);
        assert!(runtime.set_unit("activating", 0));
        assert_eq!(runtime.state(), State::Starting);
        runtime.set_android_connected(true);
        assert_eq!(runtime.state(), State::Starting);
        runtime.set_unit("active", 10_000);
        assert_eq!(runtime.state(), State::Running);
        assert_eq!(runtime.snapshot(100_999, 700).uptime_seconds, 90);
        assert!(!runtime.set_unit("active", 10_000));
        runtime.set_android_connected(false);
        assert_eq!(runtime.state(), State::Starting);
        runtime.set_unit("deactivating", 0);
        assert_eq!(runtime.state(), State::Stopping);
        assert_eq!(runtime.active_since_ms(), 0);
        runtime.set_unit("failed", 0);
        assert_eq!(runtime.state(), State::Stopped);
    }

    #[test]
    fn login_defaults_to_installed_apps_but_explicit_policy_wins() {
        let mut runtime = RuntimeState::default();
        assert!(!runtime.at_login());
        runtime.set_has_android_apps(true);
        assert!(runtime.at_login());
        runtime.set_start_at_login(false);
        assert!(!runtime.at_login());
    }

    #[test]
    fn watchdog_restarts_once_then_rearms_after_a_completed_boot() {
        let mut watchdog = BootWatchdog::default();
        let armed = watchdog.set_active_since(1_000, 2_000);
        assert_eq!(armed, BootTimer::Armed(Duration::from_millis(59_000)));
        assert_eq!(
            watchdog.set_active_since(1_000, 2_001),
            BootTimer::Unchanged
        );
        assert_eq!(watchdog.on_boot_timeout(Some(999), false), Stall::Restart);
        assert_eq!(watchdog.set_active_since(0, 61_000), BootTimer::Cancelled);
        let _ = watchdog.set_active_since(2_000, 162_000);
        assert_eq!(watchdog.on_boot_timeout(None, false), Stall::Report);
        let _ = watchdog.set_active_since(3_000, 163_000);
        assert_eq!(watchdog.on_boot_timeout(Some(3_000), false), Stall::None);
        let _ = watchdog.set_active_since(4_000, 164_000);
        assert_eq!(watchdog.on_boot_timeout(None, true), Stall::Collect);
        assert_eq!(watchdog.on_boot_timeout(None, true), Stall::None);
        assert!(watchdog.evidence_finished(None));
    }

    #[test]
    fn follower_suppresses_crash_loop_but_allows_exact_gap() {
        let mut follower = SessionFollower::default();
        assert!(!follower.on_name_owner_changed(SHELL_BUS_NAME, true, 0));
        assert!(!follower.on_name_owner_changed("org.other", false, 0));
        assert!(!follower.on_name_owner_changed(SHELL_BUS_NAME, false, 100));
        assert!(follower.on_name_owner_changed(SHELL_BUS_NAME, true, 100));
        assert!(!follower.on_name_owner_changed(SHELL_BUS_NAME, false, 101));
        assert!(!follower.on_name_owner_changed(SHELL_BUS_NAME, true, 30_099));
        assert!(!follower.on_name_owner_changed(SHELL_BUS_NAME, true, 30_100));
        assert!(!follower.on_name_owner_changed(SHELL_BUS_NAME, false, 30_100));
        assert!(follower.on_name_owner_changed(SHELL_BUS_NAME, true, 30_100));
    }
}
