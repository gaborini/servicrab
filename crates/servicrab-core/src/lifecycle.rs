//! Service lifecycle types and pure restart logic.
//!
//! Everything in this module is deterministic and free of I/O: it takes
//! durations and process outcomes as inputs and returns decisions.  The actual
//! process handling lives in [`crate::runtime`].
//!
//! Per-process bookkeeping the daemon reports (pid, uptime, restart count)
//! lives in [`crate::runtime::status`], not in these types.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::RestartPolicy;

/// Observable state of a single managed service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// The service has been configured but never started.
    Pending,
    /// The process is being spawned.
    Starting,
    /// The process is alive.
    Running,
    /// The process exited and the supervisor is waiting out the restart delay.
    Backoff,
    /// A shutdown signal has been sent; waiting for the process to exit.
    Stopping,
    /// The service was stopped on explicit request and will not be restarted.
    Stopped,
    /// The process exited and the restart policy does not call for a restart.
    Exited,
    /// The service gave up: the restart limit was exhausted or a fatal error
    /// occurred.
    Failed,
}

impl ServiceState {
    /// Whether a transition from `self` to `next` is legal.
    pub fn can_transition_to(self, next: ServiceState) -> bool {
        use ServiceState::*;
        matches!(
            (self, next),
            (Pending, Starting)
                | (Pending, Stopped)
                | (Starting, Running)
                | (Starting, Backoff)
                | (Starting, Exited)
                | (Starting, Failed)
                | (Starting, Stopping)
                | (Running, Stopping)
                | (Running, Backoff)
                | (Running, Exited)
                | (Running, Failed)
                | (Backoff, Starting)
                | (Backoff, Stopped)
                | (Backoff, Failed)
                | (Stopping, Backoff)
                | (Stopping, Stopped)
                | (Stopping, Exited)
                | (Stopping, Failed)
        )
    }

    /// Whether this is a terminal state from which no further transition is
    /// possible.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ServiceState::Stopped | ServiceState::Exited | ServiceState::Failed
        )
    }
}

impl std::fmt::Display for ServiceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ServiceState::Pending => "pending",
            ServiceState::Starting => "starting",
            ServiceState::Running => "running",
            ServiceState::Backoff => "backoff",
            ServiceState::Stopping => "stopping",
            ServiceState::Stopped => "stopped",
            ServiceState::Exited => "exited",
            ServiceState::Failed => "failed",
        };
        f.write_str(s)
    }
}

/// A rejected [`ServiceState`] transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTransition {
    /// The state the machine was in.
    pub from: ServiceState,
    /// The state that was requested.
    pub to: ServiceState,
}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid transition {} -> {}", self.from, self.to)
    }
}

impl std::error::Error for InvalidTransition {}

/// An explicit state machine that rejects illegal transitions instead of
/// silently accepting them.
#[derive(Debug, Clone)]
pub struct StateMachine {
    state: ServiceState,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    /// Create a machine in the [`ServiceState::Pending`] state.
    pub fn new() -> Self {
        Self {
            state: ServiceState::Pending,
        }
    }

    /// The current state.
    pub fn state(&self) -> ServiceState {
        self.state
    }

    /// Attempt to move to `next`, returning an error if the edge is illegal.
    pub fn try_transition(&mut self, next: ServiceState) -> Result<(), InvalidTransition> {
        if self.state.can_transition_to(next) {
            self.state = next;
            Ok(())
        } else {
            Err(InvalidTransition {
                from: self.state,
                to: next,
            })
        }
    }
}

/// Why a supervised process stopped running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The process exited normally with this status code.
    Code(i32),
    /// The process was terminated by this signal number.
    Signal(i32),
    /// The process could not be spawned at all.
    SpawnFailure {
        /// Whether spawning is worth retrying (for example a transient
        /// resource shortage rather than a missing executable).
        retryable: bool,
    },
    /// The process was stopped because its health check failed.
    Unhealthy,
}

impl ExitReason {
    /// Whether this outcome counts as a successful run.
    pub fn is_success(self) -> bool {
        matches!(self, ExitReason::Code(0))
    }
}

impl std::fmt::Display for ExitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExitReason::Code(code) => write!(f, "exited with code {code}"),
            ExitReason::Signal(sig) => write!(f, "terminated by signal {sig}"),
            ExitReason::SpawnFailure { retryable } => {
                write!(f, "spawn failed (retryable: {retryable})")
            }
            ExitReason::Unhealthy => write!(f, "stopped after failing its health check"),
        }
    }
}

/// Why the supervisor is shutting a service down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// The user pressed Ctrl+C (SIGINT).
    UserInterrupt,
    /// The supervisor itself received SIGTERM.
    Terminated,
    /// The restart limit was exhausted.
    RestartLimit,
    /// Another service in the stack failed and the whole stack is being torn
    /// down (`up --abort-on-failure`).
    StackFailure,
    /// The service failed its health check.
    Unhealthy,
    /// An operator asked for this specific service to stop.
    Requested,
}

impl ShutdownReason {
    /// Whether this shutdown was requested by the user (and therefore must
    /// never trigger an automatic restart).
    pub fn is_user_requested(self) -> bool {
        matches!(
            self,
            ShutdownReason::UserInterrupt | ShutdownReason::Terminated | ShutdownReason::Requested
        )
    }
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownReason::UserInterrupt => write!(f, "interrupted by user"),
            ShutdownReason::Terminated => write!(f, "supervisor terminated"),
            ShutdownReason::RestartLimit => write!(f, "restart limit exhausted"),
            ShutdownReason::StackFailure => write!(f, "another service failed"),
            ShutdownReason::Requested => write!(f, "stopped on request"),
            ShutdownReason::Unhealthy => write!(f, "failed its health check"),
        }
    }
}

/// The result of one supervised process run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessOutcome {
    /// How the process stopped.
    pub reason: ExitReason,
    /// How long the process stayed alive before stopping.
    pub uptime: Duration,
}

impl ProcessOutcome {
    /// Build an outcome.
    pub fn new(reason: ExitReason, uptime: Duration) -> Self {
        Self { reason, uptime }
    }
}

/// What the supervisor should do after a [`ProcessOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    /// Wait `delay`, then start the process again.
    Restart {
        /// How long to wait before respawning.
        delay: Duration,
        /// The 1-based restart attempt number this decision represents.
        attempt: u32,
    },
    /// Stop supervising; this is not a failure.
    Stop,
    /// Stop supervising and report a failure.
    Fail {
        /// Why the supervisor gave up.
        reason: ShutdownReason,
    },
}

/// Restart bookkeeping for a single service.
///
/// The tracker is pure: callers feed it [`ProcessOutcome`]s and it returns
/// [`RestartDecision`]s.  It owns no clock and spawns nothing.
#[derive(Debug, Clone)]
pub struct RestartTracker {
    policy: RestartPolicy,
    restart_delay: Duration,
    restart_max_delay: Duration,
    max_restarts: u32,
    stable_after: Duration,
    attempts: u32,
}

impl RestartTracker {
    /// Build a tracker from validated service settings.
    pub fn new(
        policy: RestartPolicy,
        restart_delay: Duration,
        restart_max_delay: Duration,
        max_restarts: u32,
        stable_after: Duration,
    ) -> Self {
        Self {
            policy,
            restart_delay,
            restart_max_delay,
            max_restarts,
            stable_after,
            attempts: 0,
        }
    }

    /// Build a tracker from a validated [`crate::config::Service`].
    pub fn from_service(service: &crate::config::Service) -> Self {
        Self::new(
            service.restart,
            service.restart_delay,
            service.restart_max_delay,
            service.max_restarts,
            service.stable_after,
        )
    }

    /// Force the policy to [`RestartPolicy::Never`] (used by `--no-restart`).
    pub fn with_policy(mut self, policy: RestartPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The effective restart policy.
    pub fn policy(&self) -> RestartPolicy {
        self.policy
    }

    /// Number of restarts performed since the last stable period.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Backoff delay for the given zero-based attempt index:
    /// `min(restart_delay * 2^attempt, restart_max_delay)`.
    ///
    /// The multiplication saturates, so large attempt counts simply clamp to
    /// `restart_max_delay` instead of overflowing.
    pub fn backoff_for_attempt(&self, attempt: u32) -> Duration {
        let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
        let delay = self
            .restart_delay
            .checked_mul(u32::try_from(factor).unwrap_or(u32::MAX))
            .unwrap_or(self.restart_max_delay);
        delay.min(self.restart_max_delay)
    }

    /// Decide what to do after a process run.
    ///
    /// `shutdown` is `Some` when the user explicitly asked the supervisor to
    /// stop; in that case no restart ever happens, regardless of policy.
    pub fn decide(
        &mut self,
        outcome: ProcessOutcome,
        shutdown: Option<ShutdownReason>,
    ) -> RestartDecision {
        // An explicit user-requested shutdown short-circuits the policy.
        if let Some(reason) = shutdown {
            if reason.is_user_requested() {
                return RestartDecision::Stop;
            }
        }

        // A long-enough run resets the attempt counter before we evaluate the
        // policy, so a service that is stable for a while gets a fresh budget.
        if outcome.uptime >= self.stable_after {
            self.attempts = 0;
        }

        if !self.should_restart(outcome.reason) {
            return RestartDecision::Stop;
        }

        if self.attempts >= self.max_restarts {
            return RestartDecision::Fail {
                reason: ShutdownReason::RestartLimit,
            };
        }

        let delay = self.backoff_for_attempt(self.attempts);
        self.attempts += 1;
        RestartDecision::Restart {
            delay,
            attempt: self.attempts,
        }
    }

    fn should_restart(&self, reason: ExitReason) -> bool {
        match self.policy {
            RestartPolicy::Never => false,
            // `unless-stopped` differs from `always` only in what survives a
            // daemon restart, which is not something this tracker can observe:
            // a hand-stopped service never reaches the policy at all, because
            // the shutdown reason short-circuits above.
            RestartPolicy::Always | RestartPolicy::UnlessStopped => match reason {
                ExitReason::SpawnFailure { retryable } => retryable,
                _ => true,
            },
            RestartPolicy::OnFailure => match reason {
                ExitReason::Code(0) => false,
                ExitReason::Code(_) | ExitReason::Signal(_) | ExitReason::Unhealthy => true,
                ExitReason::SpawnFailure { retryable } => retryable,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: Duration = Duration::from_secs(1);

    fn tracker(policy: RestartPolicy) -> RestartTracker {
        RestartTracker::new(
            policy,
            SEC,
            Duration::from_secs(30),
            5,
            Duration::from_secs(60),
        )
    }

    fn outcome(reason: ExitReason) -> ProcessOutcome {
        ProcessOutcome::new(reason, Duration::from_millis(10))
    }

    // ── state machine ──────────────────────────────────────────────────────

    #[test]
    fn display_covers_every_state() {
        assert_eq!(ServiceState::Pending.to_string(), "pending");
        assert_eq!(ServiceState::Starting.to_string(), "starting");
        assert_eq!(ServiceState::Running.to_string(), "running");
        assert_eq!(ServiceState::Backoff.to_string(), "backoff");
        assert_eq!(ServiceState::Stopping.to_string(), "stopping");
        assert_eq!(ServiceState::Stopped.to_string(), "stopped");
        assert_eq!(ServiceState::Exited.to_string(), "exited");
        assert_eq!(ServiceState::Failed.to_string(), "failed");
    }

    #[test]
    fn happy_path_transitions_are_accepted() {
        let mut sm = StateMachine::new();
        assert_eq!(sm.state(), ServiceState::Pending);
        sm.try_transition(ServiceState::Starting).unwrap();
        sm.try_transition(ServiceState::Running).unwrap();
        sm.try_transition(ServiceState::Backoff).unwrap();
        sm.try_transition(ServiceState::Starting).unwrap();
        sm.try_transition(ServiceState::Running).unwrap();
        sm.try_transition(ServiceState::Stopping).unwrap();
        sm.try_transition(ServiceState::Stopped).unwrap();
        assert!(sm.state().is_terminal());
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        let mut sm = StateMachine::new();
        // Cannot go straight from pending to running.
        let err = sm.try_transition(ServiceState::Running).unwrap_err();
        assert_eq!(err.from, ServiceState::Pending);
        assert_eq!(err.to, ServiceState::Running);
        // State is unchanged after a rejected transition.
        assert_eq!(sm.state(), ServiceState::Pending);
    }

    #[test]
    fn terminal_states_reject_further_transitions() {
        let mut sm = StateMachine::new();
        sm.try_transition(ServiceState::Starting).unwrap();
        sm.try_transition(ServiceState::Exited).unwrap();
        assert!(sm.try_transition(ServiceState::Starting).is_err());
        assert!(sm.try_transition(ServiceState::Running).is_err());
    }

    #[test]
    fn invalid_transition_displays_both_states() {
        let err = InvalidTransition {
            from: ServiceState::Pending,
            to: ServiceState::Running,
        };
        assert_eq!(err.to_string(), "invalid transition pending -> running");
    }

    // ── restart policy decisions ───────────────────────────────────────────

    #[test]
    fn never_does_not_restart_anything() {
        for reason in [
            ExitReason::Code(0),
            ExitReason::Code(1),
            ExitReason::Signal(9),
            ExitReason::SpawnFailure { retryable: true },
        ] {
            let mut t = tracker(RestartPolicy::Never);
            assert_eq!(t.decide(outcome(reason), None), RestartDecision::Stop);
        }
    }

    #[test]
    fn on_failure_ignores_successful_exit() {
        let mut t = tracker(RestartPolicy::OnFailure);
        assert_eq!(
            t.decide(outcome(ExitReason::Code(0)), None),
            RestartDecision::Stop
        );
    }

    #[test]
    fn on_failure_restarts_non_zero_exit() {
        let mut t = tracker(RestartPolicy::OnFailure);
        assert_eq!(
            t.decide(outcome(ExitReason::Code(3)), None),
            RestartDecision::Restart {
                delay: SEC,
                attempt: 1
            }
        );
    }

    #[test]
    fn on_failure_restarts_signal_termination() {
        let mut t = tracker(RestartPolicy::OnFailure);
        assert_eq!(
            t.decide(outcome(ExitReason::Signal(11)), None),
            RestartDecision::Restart {
                delay: SEC,
                attempt: 1
            }
        );
    }

    #[test]
    fn on_failure_restarts_retryable_spawn_failure_only() {
        let mut t = tracker(RestartPolicy::OnFailure);
        assert_eq!(
            t.decide(outcome(ExitReason::SpawnFailure { retryable: true }), None),
            RestartDecision::Restart {
                delay: SEC,
                attempt: 1
            }
        );

        let mut t = tracker(RestartPolicy::OnFailure);
        assert_eq!(
            t.decide(outcome(ExitReason::SpawnFailure { retryable: false }), None),
            RestartDecision::Stop
        );
    }

    #[test]
    fn always_restarts_successful_exit() {
        let mut t = tracker(RestartPolicy::Always);
        assert_eq!(
            t.decide(outcome(ExitReason::Code(0)), None),
            RestartDecision::Restart {
                delay: SEC,
                attempt: 1
            }
        );
    }

    #[test]
    fn always_restarts_failed_exit() {
        let mut t = tracker(RestartPolicy::Always);
        assert_eq!(
            t.decide(outcome(ExitReason::Code(7)), None),
            RestartDecision::Restart {
                delay: SEC,
                attempt: 1
            }
        );
    }

    #[test]
    fn unless_stopped_restarts_whatever_the_exit_status_was() {
        for reason in [
            ExitReason::Code(0),
            ExitReason::Code(7),
            ExitReason::Signal(9),
        ] {
            let mut t = tracker(RestartPolicy::UnlessStopped);
            assert!(
                matches!(
                    t.decide(outcome(reason), None),
                    RestartDecision::Restart { .. }
                ),
                "{reason:?}"
            );
        }

        // What the policy is *named* for is the one case it shares with every
        // other policy; the difference only shows up across daemon restarts,
        // which this tracker cannot see.
        let mut t = tracker(RestartPolicy::UnlessStopped);
        assert_eq!(
            t.decide(
                outcome(ExitReason::Code(1)),
                Some(ShutdownReason::Requested)
            ),
            RestartDecision::Stop
        );
    }

    #[test]
    fn user_shutdown_never_restarts() {
        for reason in [ShutdownReason::UserInterrupt, ShutdownReason::Terminated] {
            let mut t = tracker(RestartPolicy::Always);
            assert_eq!(
                t.decide(outcome(ExitReason::Code(1)), Some(reason)),
                RestartDecision::Stop
            );
            assert_eq!(t.attempts(), 0, "shutdown must not consume attempts");
        }
    }

    // ── backoff ────────────────────────────────────────────────────────────

    #[test]
    fn backoff_doubles_each_attempt() {
        let mut t = tracker(RestartPolicy::Always);
        let delays: Vec<Duration> = (0..5)
            .map(|_| match t.decide(outcome(ExitReason::Code(0)), None) {
                RestartDecision::Restart { delay, .. } => delay,
                other => panic!("expected restart, got {other:?}"),
            })
            .collect();
        assert_eq!(
            delays,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
            ]
        );
    }

    #[test]
    fn backoff_is_capped_at_max_delay() {
        let t = RestartTracker::new(
            RestartPolicy::Always,
            SEC,
            Duration::from_secs(5),
            100,
            Duration::from_secs(60),
        );
        assert_eq!(t.backoff_for_attempt(3), Duration::from_secs(5));
        assert_eq!(t.backoff_for_attempt(40), Duration::from_secs(5));
        // Very large shift counts must saturate rather than overflow.
        assert_eq!(t.backoff_for_attempt(u32::MAX), Duration::from_secs(5));
    }

    #[test]
    fn max_restarts_is_enforced() {
        let mut t = RestartTracker::new(
            RestartPolicy::Always,
            Duration::from_millis(1),
            Duration::from_millis(1),
            2,
            Duration::from_secs(60),
        );
        assert!(matches!(
            t.decide(outcome(ExitReason::Code(1)), None),
            RestartDecision::Restart { attempt: 1, .. }
        ));
        assert!(matches!(
            t.decide(outcome(ExitReason::Code(1)), None),
            RestartDecision::Restart { attempt: 2, .. }
        ));
        assert_eq!(
            t.decide(outcome(ExitReason::Code(1)), None),
            RestartDecision::Fail {
                reason: ShutdownReason::RestartLimit
            }
        );
    }

    #[test]
    fn zero_max_restarts_fails_immediately() {
        let mut t =
            RestartTracker::new(RestartPolicy::Always, SEC, SEC, 0, Duration::from_secs(60));
        assert_eq!(
            t.decide(outcome(ExitReason::Code(1)), None),
            RestartDecision::Fail {
                reason: ShutdownReason::RestartLimit
            }
        );
    }

    #[test]
    fn stable_period_resets_attempt_counter() {
        let mut t = tracker(RestartPolicy::Always);
        // Two quick crashes advance the backoff.
        t.decide(outcome(ExitReason::Code(1)), None);
        t.decide(outcome(ExitReason::Code(1)), None);
        assert_eq!(t.attempts(), 2);

        // A run that lasted at least `stable_after` resets the counter, so the
        // next restart starts from the initial delay again.
        let stable = ProcessOutcome::new(ExitReason::Code(1), Duration::from_secs(60));
        assert_eq!(
            t.decide(stable, None),
            RestartDecision::Restart {
                delay: SEC,
                attempt: 1
            }
        );
    }

    #[test]
    fn stable_reset_revives_an_almost_exhausted_budget() {
        let mut t = RestartTracker::new(
            RestartPolicy::Always,
            SEC,
            Duration::from_secs(30),
            1,
            Duration::from_secs(10),
        );
        assert!(matches!(
            t.decide(outcome(ExitReason::Code(1)), None),
            RestartDecision::Restart { attempt: 1, .. }
        ));
        // Without a stable run this would fail; a long run resets the budget.
        let stable = ProcessOutcome::new(ExitReason::Code(1), Duration::from_secs(10));
        assert!(matches!(
            t.decide(stable, None),
            RestartDecision::Restart { attempt: 1, .. }
        ));
    }

    #[test]
    fn exit_reason_success_and_display() {
        assert!(ExitReason::Code(0).is_success());
        assert!(!ExitReason::Code(1).is_success());
        assert!(!ExitReason::Signal(15).is_success());
        assert_eq!(ExitReason::Code(2).to_string(), "exited with code 2");
        assert_eq!(
            ExitReason::Signal(15).to_string(),
            "terminated by signal 15"
        );
    }

    #[test]
    fn restart_limit_shutdown_is_not_user_requested() {
        assert!(ShutdownReason::UserInterrupt.is_user_requested());
        assert!(ShutdownReason::Terminated.is_user_requested());
        assert!(!ShutdownReason::RestartLimit.is_user_requested());
    }
}
