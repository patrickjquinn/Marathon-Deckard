//! Pure state and timeout policy for deckard-bridge's runtime, boot watchdog, and
//! shell session follower. The adapter owns clocks, timers, I/O, and commands.

pub const STOP_TIMEOUT_MS: u64 = 90_000;
pub const BOOT_TIMEOUT_MS: u64 = 60_000;
pub const EVIDENCE_TIMEOUT_MS: u64 = 90_000;
pub const SHELL_SETTLE_MS: u64 = 2_000;
pub const SHELL_MIN_GAP_MS: u64 = 30_000;
pub const SHELL_BUS_NAME: &str = "org.marathonos.Shell";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    Running,
    Stopping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitCall {
    Start,
    Stop,
    Restart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetError {
    AlreadyResetting,
    Unavailable,
    DidNotStop,
    HelperFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeCommand {
    Changed,
    CallUnit(UnitCall),
    SaveStartAtLogin(bool),
    ScheduleStopTimeout {
        request: u64,
        after_ms: u64,
    },
    CancelStopTimeout {
        request: u64,
    },
    RunResetHelper {
        request: u64,
    },
    ResetFinished {
        request: u64,
        result: Result<(), ResetError>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub state: State,
    pub memory_bytes: i64,
    pub android_version: String,
    pub uptime_seconds: i64,
    pub start_at_login: bool,
    pub has_android_apps: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResetPhase {
    WaitingForStop(u64),
    RunningHelper(u64),
}

/// Inputs are observations from systemd, Android, and the settings adapter.
/// Timer and helper callbacks carry the request ID from their command so late
/// callbacks cannot complete a newer reset.
#[derive(Debug)]
pub struct RuntimeState {
    unit_state: String,
    entered_ms: i64,
    android_connected: bool,
    android_version: String,
    start_at_login: Option<bool>,
    has_android_apps: bool,
    reset: Option<ResetPhase>,
    next_request: u64,
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
            reset: None,
            next_request: 0,
        }
    }
}

impl RuntimeState {
    pub fn state(&self) -> State {
        match self.unit_state.as_str() {
            "active" | "reloading" if self.android_connected => State::Running,
            "active" | "reloading" | "activating" => State::Starting,
            "deactivating" => State::Stopping,
            _ => State::Stopped,
        }
    }

    pub fn active_since_ms(&self) -> i64 {
        match self.unit_state.as_str() {
            "active" | "reloading" => self.entered_ms,
            _ => 0,
        }
    }

    pub fn snapshot(&self, now_ms: i64, observed_memory_bytes: i64) -> RuntimeSnapshot {
        let state = self.state();
        RuntimeSnapshot {
            state,
            memory_bytes: if state == State::Stopped {
                0
            } else {
                observed_memory_bytes.max(0)
            },
            android_version: self.android_version.clone(),
            uptime_seconds: if state == State::Running && self.entered_ms > 0 {
                now_ms.saturating_sub(self.entered_ms).max(0) / 1_000
            } else {
                0
            },
            start_at_login: self.start_at_login(),
            has_android_apps: self.has_android_apps,
        }
    }

    pub fn start_at_login(&self) -> bool {
        self.start_at_login.unwrap_or(self.has_android_apps)
    }

    pub fn start_at_login_is_set(&self) -> bool {
        self.start_at_login.is_some()
    }

    pub fn set_has_android_apps(&mut self, has_apps: bool) {
        self.has_android_apps = has_apps;
    }

    pub fn set_start_at_login(&mut self, enabled: bool) -> Vec<RuntimeCommand> {
        self.start_at_login = Some(enabled);
        vec![
            RuntimeCommand::SaveStartAtLogin(enabled),
            RuntimeCommand::Changed,
        ]
    }

    pub fn at_login(&self) -> Vec<RuntimeCommand> {
        if self.start_at_login() && self.state() == State::Stopped {
            vec![RuntimeCommand::CallUnit(UnitCall::Start)]
        } else {
            vec![]
        }
    }

    pub fn start(&self) -> RuntimeCommand {
        RuntimeCommand::CallUnit(UnitCall::Start)
    }

    pub fn stop(&self) -> RuntimeCommand {
        RuntimeCommand::CallUnit(UnitCall::Stop)
    }

    pub fn restart(&self) -> RuntimeCommand {
        RuntimeCommand::CallUnit(UnitCall::Restart)
    }

    pub fn set_unit(&mut self, active_state: &str, entered_ms: i64) -> Vec<RuntimeCommand> {
        if self.unit_state == active_state && self.entered_ms == entered_ms {
            return vec![];
        }
        self.unit_state = active_state.into();
        self.entered_ms = entered_ms;
        let mut commands = vec![RuntimeCommand::Changed];
        if self.state() == State::Stopped
            && let Some(ResetPhase::WaitingForStop(request)) = self.reset
        {
            self.reset = Some(ResetPhase::RunningHelper(request));
            commands.push(RuntimeCommand::CancelStopTimeout { request });
            commands.push(RuntimeCommand::RunResetHelper { request });
        }
        commands
    }

    pub fn set_android_connected(&mut self, connected: bool) -> Vec<RuntimeCommand> {
        if self.android_connected == connected {
            return vec![];
        }
        self.android_connected = connected;
        vec![RuntimeCommand::Changed]
    }

    pub fn set_android_version(&mut self, version: &str) -> Vec<RuntimeCommand> {
        if self.android_version == version {
            return vec![];
        }
        self.android_version = version.into();
        vec![RuntimeCommand::Changed]
    }

    pub fn request_reset(
        &mut self,
        helper_available: bool,
    ) -> Result<(u64, Vec<RuntimeCommand>), ResetError> {
        if self.reset.is_some() {
            return Err(ResetError::AlreadyResetting);
        }
        if !helper_available {
            return Err(ResetError::Unavailable);
        }
        self.next_request = self.next_request.wrapping_add(1);
        let request = self.next_request;
        if self.state() == State::Stopped {
            self.reset = Some(ResetPhase::RunningHelper(request));
            Ok((request, vec![RuntimeCommand::RunResetHelper { request }]))
        } else {
            self.reset = Some(ResetPhase::WaitingForStop(request));
            Ok((
                request,
                vec![
                    RuntimeCommand::ScheduleStopTimeout {
                        request,
                        after_ms: STOP_TIMEOUT_MS,
                    },
                    RuntimeCommand::CallUnit(UnitCall::Stop),
                ],
            ))
        }
    }

    pub fn on_stop_timeout(&mut self, request: u64) -> Vec<RuntimeCommand> {
        if self.reset != Some(ResetPhase::WaitingForStop(request)) {
            return vec![];
        }
        self.reset = None;
        vec![RuntimeCommand::ResetFinished {
            request,
            result: Err(ResetError::DidNotStop),
        }]
    }

    pub fn on_reset_helper_finished(&mut self, request: u64, success: bool) -> Vec<RuntimeCommand> {
        if self.reset != Some(ResetPhase::RunningHelper(request)) {
            return vec![];
        }
        self.reset = None;
        vec![RuntimeCommand::ResetFinished {
            request,
            result: if success {
                Ok(())
            } else {
                Err(ResetError::HelperFailed)
            },
        }]
    }

    pub fn on_reset_helper_failed_to_start(&mut self, request: u64) -> Vec<RuntimeCommand> {
        if self.reset != Some(ResetPhase::RunningHelper(request)) {
            return vec![];
        }
        self.reset = None;
        vec![RuntimeCommand::ResetFinished {
            request,
            result: Err(ResetError::Unavailable),
        }]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootCommand {
    ScheduleBootTimeout { timer: u64, after_ms: u64 },
    CancelBootTimeout { timer: u64 },
    Stalled,
    StartEvidence { run: u64 },
    ScheduleEvidenceTimeout { run: u64, after_ms: u64 },
    CancelEvidenceTimeout { run: u64 },
    KillEvidence { run: u64 },
    RestartUnit,
}

/// The marker's modification time is supplied by the adapter; `None` means
/// missing or unreadable. Evidence completion is separate from its kill timer.
#[derive(Debug, Default)]
pub struct BootWatchdog {
    active: bool,
    attempt_ms: i64,
    stalled_attempt_ms: i64,
    restarted: bool,
    timer: Option<u64>,
    evidence_run: Option<u64>,
    next_id: u64,
}

impl BootWatchdog {
    fn next_id(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    pub fn set_active_since(&mut self, active_since_ms: i64, now_ms: i64) -> Vec<BootCommand> {
        self.active = active_since_ms > 0;
        if !self.active {
            return self.timer.take().map_or_else(Vec::new, |timer| {
                vec![BootCommand::CancelBootTimeout { timer }]
            });
        }
        if active_since_ms == self.attempt_ms {
            return vec![];
        }
        let mut commands = Vec::new();
        if let Some(timer) = self.timer.take() {
            commands.push(BootCommand::CancelBootTimeout { timer });
        }
        self.attempt_ms = active_since_ms;
        let timer = self.next_id();
        self.timer = Some(timer);
        let remaining = active_since_ms
            .saturating_add(BOOT_TIMEOUT_MS as i64)
            .saturating_sub(now_ms);
        commands.push(BootCommand::ScheduleBootTimeout {
            timer,
            after_ms: remaining.clamp(0, BOOT_TIMEOUT_MS as i64) as u64,
        });
        commands
    }

    pub fn on_boot_timeout(
        &mut self,
        timer: u64,
        marker_modified_ms: Option<i64>,
        evidence_available: bool,
    ) -> Vec<BootCommand> {
        if self.timer != Some(timer) || !self.active {
            return vec![];
        }
        self.timer = None;
        if self.booted(marker_modified_ms) {
            self.restarted = false;
            return vec![];
        }
        if self.evidence_run.is_some() {
            return vec![];
        }
        let mut commands = vec![BootCommand::Stalled];
        self.stalled_attempt_ms = self.attempt_ms;
        if evidence_available {
            let run = self.next_id();
            self.evidence_run = Some(run);
            commands.push(BootCommand::StartEvidence { run });
            commands.push(BootCommand::ScheduleEvidenceTimeout {
                run,
                after_ms: EVIDENCE_TIMEOUT_MS,
            });
        } else {
            commands.extend(self.finish(marker_modified_ms));
        }
        commands
    }

    pub fn on_evidence_timeout(&self, run: u64) -> Vec<BootCommand> {
        if self.evidence_run == Some(run) {
            vec![BootCommand::KillEvidence { run }]
        } else {
            vec![]
        }
    }

    pub fn on_evidence_finished(
        &mut self,
        run: u64,
        marker_modified_ms: Option<i64>,
    ) -> Vec<BootCommand> {
        if self.evidence_run != Some(run) {
            return vec![];
        }
        self.evidence_run = None;
        let mut commands = vec![BootCommand::CancelEvidenceTimeout { run }];
        commands.extend(self.finish(marker_modified_ms));
        commands
    }

    fn booted(&self, marker_modified_ms: Option<i64>) -> bool {
        marker_modified_ms.is_some_and(|modified| modified >= self.attempt_ms)
    }

    fn finish(&mut self, marker_modified_ms: Option<i64>) -> Vec<BootCommand> {
        if !self.active
            || self.attempt_ms != self.stalled_attempt_ms
            || self.booted(marker_modified_ms)
            || self.restarted
        {
            return vec![];
        }
        self.restarted = true;
        vec![BootCommand::RestartUnit]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowCommand {
    ScheduleSettle { timer: u64, after_ms: u64 },
    RestartUnit,
}

/// `now_ms` is a monotonic millisecond clock, not the epoch clock used by the
/// unit and boot marker. A suppressed reacquisition still clears `lost`.
#[derive(Debug, Default)]
pub struct SessionFollower {
    lost: bool,
    last_restart_ms: Option<u64>,
    pending: Vec<u64>,
    next_timer: u64,
}

impl SessionFollower {
    pub fn on_name_owner_changed(
        &mut self,
        name: &str,
        has_new_owner: bool,
        now_ms: u64,
    ) -> Vec<FollowCommand> {
        if name != SHELL_BUS_NAME {
            return vec![];
        }
        if !has_new_owner {
            self.lost = true;
            return vec![];
        }
        if !self.lost {
            return vec![];
        }
        self.lost = false;
        if self
            .last_restart_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < SHELL_MIN_GAP_MS)
        {
            return vec![];
        }
        self.last_restart_ms = Some(now_ms);
        self.next_timer = self.next_timer.wrapping_add(1);
        let timer = self.next_timer;
        self.pending.push(timer);
        vec![FollowCommand::ScheduleSettle {
            timer,
            after_ms: SHELL_SETTLE_MS,
        }]
    }

    pub fn on_settle_timeout(&mut self, timer: u64) -> Vec<FollowCommand> {
        if let Some(index) = self.pending.iter().position(|pending| *pending == timer) {
            self.pending.swap_remove(index);
            vec![FollowCommand::RestartUnit]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests assert successful reset requests")]
mod tests {
    use super::*;

    #[test]
    fn runtime_maps_unit_and_android_readiness_without_optimism() {
        let mut runtime = RuntimeState::default();
        assert_eq!(runtime.state(), State::Stopped);
        assert_eq!(runtime.snapshot(100_000, 500).memory_bytes, 0);
        assert_eq!(
            runtime.set_unit("activating", 0),
            vec![RuntimeCommand::Changed]
        );
        assert_eq!(runtime.state(), State::Starting);
        runtime.set_android_connected(true);
        assert_eq!(runtime.state(), State::Starting);
        runtime.set_unit("active", 10_000);
        assert_eq!(runtime.state(), State::Running);
        assert_eq!(runtime.active_since_ms(), 10_000);
        assert_eq!(runtime.snapshot(100_999, 700).uptime_seconds, 90);
        assert_eq!(runtime.set_unit("active", 10_000), vec![]);
        runtime.set_android_connected(false);
        assert_eq!(runtime.state(), State::Starting);
        runtime.set_unit("reloading", 20_000);
        assert_eq!(runtime.active_since_ms(), 20_000);
        runtime.set_android_connected(true);
        assert_eq!(runtime.state(), State::Running);
        runtime.set_unit("deactivating", 0);
        assert_eq!(runtime.state(), State::Stopping);
        assert_eq!(runtime.active_since_ms(), 0);
        runtime.set_unit("failed", 0);
        assert_eq!(runtime.state(), State::Stopped);
    }

    #[test]
    fn runtime_snapshot_clamps_memory_and_uptime_and_tracks_version() {
        let mut runtime = RuntimeState::default();
        assert_eq!(
            runtime.set_android_version("13"),
            vec![RuntimeCommand::Changed]
        );
        assert_eq!(runtime.set_android_version("13"), vec![]);
        runtime.set_unit("active", 2_000);
        runtime.set_android_connected(true);
        let snapshot = runtime.snapshot(1_000, -123);
        assert_eq!(snapshot.android_version, "13");
        assert_eq!(snapshot.memory_bytes, 0);
        assert_eq!(snapshot.uptime_seconds, 0);
        assert_eq!(runtime.snapshot(4_999, 123).uptime_seconds, 2);
        runtime.set_unit("active", 0);
        assert_eq!(runtime.snapshot(4_999, 123).uptime_seconds, 0);
    }

    #[test]
    fn login_defaults_to_installed_apps_but_explicit_policy_wins() {
        let mut runtime = RuntimeState::default();
        assert!(!runtime.start_at_login_is_set());
        assert!(runtime.at_login().is_empty());
        runtime.set_has_android_apps(true);
        assert_eq!(
            runtime.at_login(),
            vec![RuntimeCommand::CallUnit(UnitCall::Start)]
        );
        assert_eq!(
            runtime.set_start_at_login(false),
            vec![
                RuntimeCommand::SaveStartAtLogin(false),
                RuntimeCommand::Changed,
            ]
        );
        assert!(runtime.start_at_login_is_set());
        assert!(runtime.at_login().is_empty());
        runtime.set_start_at_login(true);
        runtime.set_unit("activating", 0);
        assert!(runtime.at_login().is_empty());
        assert_eq!(runtime.start(), RuntimeCommand::CallUnit(UnitCall::Start));
        assert_eq!(runtime.stop(), RuntimeCommand::CallUnit(UnitCall::Stop));
        assert_eq!(
            runtime.restart(),
            RuntimeCommand::CallUnit(UnitCall::Restart)
        );
    }

    #[test]
    fn reset_waits_for_stopped_state_and_ignores_stale_callbacks() {
        let mut runtime = RuntimeState::default();
        runtime.set_unit("active", 1);
        let (request, commands) = runtime.request_reset(true).unwrap();
        assert_eq!(
            commands,
            vec![
                RuntimeCommand::ScheduleStopTimeout {
                    request,
                    after_ms: STOP_TIMEOUT_MS
                },
                RuntimeCommand::CallUnit(UnitCall::Stop),
            ]
        );
        assert_eq!(
            runtime.request_reset(true),
            Err(ResetError::AlreadyResetting)
        );
        assert_eq!(
            runtime.set_unit("deactivating", 0),
            vec![RuntimeCommand::Changed]
        );
        assert_eq!(
            runtime.set_unit("inactive", 0),
            vec![
                RuntimeCommand::Changed,
                RuntimeCommand::CancelStopTimeout { request },
                RuntimeCommand::RunResetHelper { request },
            ]
        );
        assert!(runtime.on_stop_timeout(request).is_empty());
        assert!(
            runtime
                .on_reset_helper_finished(request + 1, true)
                .is_empty()
        );
        assert_eq!(
            runtime.on_reset_helper_finished(request, true),
            vec![RuntimeCommand::ResetFinished {
                request,
                result: Ok(())
            },]
        );
        assert!(runtime.on_reset_helper_finished(request, false).is_empty());
    }

    #[test]
    fn reset_timeout_and_helper_errors_release_the_guard() {
        let mut runtime = RuntimeState::default();
        assert_eq!(runtime.request_reset(false), Err(ResetError::Unavailable));
        runtime.set_unit("activating", 0);
        let (first, _) = runtime.request_reset(true).unwrap();
        assert_eq!(
            runtime.on_stop_timeout(first),
            vec![RuntimeCommand::ResetFinished {
                request: first,
                result: Err(ResetError::DidNotStop)
            },]
        );
        runtime.set_unit("inactive", 0);
        let (second, commands) = runtime.request_reset(true).unwrap();
        assert_eq!(
            commands,
            vec![RuntimeCommand::RunResetHelper { request: second }]
        );
        assert!(runtime.on_stop_timeout(first).is_empty());
        assert_eq!(
            runtime.on_reset_helper_finished(second, false),
            vec![RuntimeCommand::ResetFinished {
                request: second,
                result: Err(ResetError::HelperFailed)
            },]
        );
        let (third, _) = runtime.request_reset(true).unwrap();
        assert_eq!(
            runtime.on_reset_helper_failed_to_start(third),
            vec![RuntimeCommand::ResetFinished {
                request: third,
                result: Err(ResetError::Unavailable)
            },]
        );
    }

    #[test]
    fn watchdog_clamps_delay_and_accepts_marker_at_activation_time() {
        let mut watchdog = BootWatchdog::default();
        let first = watchdog.set_active_since(1_000, 2_000);
        assert_eq!(
            first,
            vec![BootCommand::ScheduleBootTimeout {
                timer: 1,
                after_ms: 59_000
            }]
        );
        assert!(watchdog.set_active_since(1_000, 2_001).is_empty());
        assert!(watchdog.on_boot_timeout(1, Some(1_000), false).is_empty());
        let next = watchdog.set_active_since(2_000, 100_000);
        assert_eq!(
            next,
            vec![BootCommand::ScheduleBootTimeout {
                timer: 2,
                after_ms: 0
            }]
        );
        assert!(watchdog.on_boot_timeout(1, None, false).is_empty());
        assert_eq!(
            watchdog.set_active_since(0, 100_000),
            vec![BootCommand::CancelBootTimeout { timer: 2 }]
        );
        assert!(watchdog.on_boot_timeout(2, None, false).is_empty());
    }

    #[test]
    fn watchdog_restarts_once_then_rearms_after_a_completed_boot() {
        let mut watchdog = BootWatchdog::default();
        watchdog.set_active_since(1_000, 61_000);
        assert_eq!(
            watchdog.on_boot_timeout(1, Some(999), false),
            vec![BootCommand::Stalled, BootCommand::RestartUnit,]
        );
        watchdog.set_active_since(0, 61_000);
        watchdog.set_active_since(2_000, 62_000);
        assert_eq!(
            watchdog.on_boot_timeout(2, None, false),
            vec![BootCommand::Stalled]
        );
        watchdog.set_active_since(3_000, 63_000);
        assert!(watchdog.on_boot_timeout(3, Some(3_000), false).is_empty());
        watchdog.set_active_since(4_000, 64_000);
        assert_eq!(
            watchdog.on_boot_timeout(4, None, false),
            vec![BootCommand::Stalled, BootCommand::RestartUnit,]
        );
    }

    #[test]
    fn watchdog_waits_for_evidence_and_drops_stopped_or_superseded_attempts() {
        let mut watchdog = BootWatchdog::default();
        watchdog.set_active_since(1_000, 61_000);
        assert_eq!(
            watchdog.on_boot_timeout(1, None, true),
            vec![
                BootCommand::Stalled,
                BootCommand::StartEvidence { run: 2 },
                BootCommand::ScheduleEvidenceTimeout {
                    run: 2,
                    after_ms: EVIDENCE_TIMEOUT_MS
                },
            ]
        );
        assert_eq!(
            watchdog.on_evidence_timeout(2),
            vec![BootCommand::KillEvidence { run: 2 }]
        );
        watchdog.set_active_since(2_000, 62_000);
        assert!(watchdog.on_boot_timeout(3, None, true).is_empty());
        assert_eq!(
            watchdog.on_evidence_finished(2, None),
            vec![BootCommand::CancelEvidenceTimeout { run: 2 },]
        );
        assert!(watchdog.on_evidence_timeout(2).is_empty());
        watchdog.set_active_since(3_000, 63_000);
        assert_eq!(
            watchdog.on_boot_timeout(4, None, true),
            vec![
                BootCommand::Stalled,
                BootCommand::StartEvidence { run: 5 },
                BootCommand::ScheduleEvidenceTimeout {
                    run: 5,
                    after_ms: EVIDENCE_TIMEOUT_MS
                },
            ]
        );
        watchdog.set_active_since(0, 63_000);
        assert_eq!(
            watchdog.on_evidence_finished(5, None),
            vec![BootCommand::CancelEvidenceTimeout { run: 5 },]
        );
    }

    #[test]
    fn watchdog_skips_restart_if_boot_completes_during_evidence() {
        let mut watchdog = BootWatchdog::default();
        watchdog.set_active_since(1_000, 61_000);
        watchdog.on_boot_timeout(1, None, true);
        assert_eq!(
            watchdog.on_evidence_finished(2, Some(1_000)),
            vec![BootCommand::CancelEvidenceTimeout { run: 2 },]
        );
        assert!(watchdog.on_evidence_finished(2, None).is_empty());
    }

    #[test]
    fn follower_requires_loss_and_reacquisition_of_only_the_shell_name() {
        let mut follower = SessionFollower::default();
        assert!(
            follower
                .on_name_owner_changed(SHELL_BUS_NAME, true, 0)
                .is_empty()
        );
        assert!(
            follower
                .on_name_owner_changed("org.other", false, 0)
                .is_empty()
        );
        assert!(
            follower
                .on_name_owner_changed("org.other", true, 1)
                .is_empty()
        );
        assert!(
            follower
                .on_name_owner_changed(SHELL_BUS_NAME, false, 2)
                .is_empty()
        );
        assert_eq!(
            follower.on_name_owner_changed(SHELL_BUS_NAME, true, 3),
            vec![FollowCommand::ScheduleSettle {
                timer: 1,
                after_ms: SHELL_SETTLE_MS
            },]
        );
        assert!(follower.on_settle_timeout(2).is_empty());
        assert_eq!(
            follower.on_settle_timeout(1),
            vec![FollowCommand::RestartUnit]
        );
        assert!(follower.on_settle_timeout(1).is_empty());
    }

    #[test]
    fn follower_suppresses_crash_loop_but_allows_exact_gap() {
        let mut follower = SessionFollower::default();
        follower.on_name_owner_changed(SHELL_BUS_NAME, false, 100);
        follower.on_name_owner_changed(SHELL_BUS_NAME, true, 100);
        follower.on_name_owner_changed(SHELL_BUS_NAME, false, 101);
        assert!(
            follower
                .on_name_owner_changed(SHELL_BUS_NAME, true, 30_099)
                .is_empty()
        );
        assert!(
            follower
                .on_name_owner_changed(SHELL_BUS_NAME, true, 30_100)
                .is_empty()
        );
        follower.on_name_owner_changed(SHELL_BUS_NAME, false, 30_100);
        assert_eq!(
            follower.on_name_owner_changed(SHELL_BUS_NAME, true, 30_100),
            vec![FollowCommand::ScheduleSettle {
                timer: 2,
                after_ms: SHELL_SETTLE_MS
            },]
        );
        assert_eq!(
            follower.on_settle_timeout(1),
            vec![FollowCommand::RestartUnit]
        );
        assert_eq!(
            follower.on_settle_timeout(2),
            vec![FollowCommand::RestartUnit]
        );
    }
}
