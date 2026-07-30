//! Process runtime: spawning, signalling, and supervising services.
//!
//! The pure restart logic lives in [`crate::lifecycle`]; this module only deals
//! with the operating-system side of running processes.  User-facing formatting
//! and exit-code mapping belong to the CLI, not here.
//!
//! * `ServiceRunner` supervises one service.
//! * `ForegroundRunner` wraps it with signal handling for `run`.
//! * `stack::StackSupervisor` runs many services concurrently for `up`.
//!
//! What a dependent waits for is set per `depends_on` entry by its
//! [`crate::config::DependencyCondition`], defaulting to *healthy* when the
//! dependency declares a health check and to *running* otherwise.
//!
//! Only Linux and macOS are supported.  On other platforms every entry point
//! returns [`crate::error::RuntimeError::UnsupportedPlatform`].

use tokio::sync::watch;

use crate::config::RestartPolicy;
use crate::lifecycle::{ExitReason, ShutdownReason};

pub mod event;
pub mod filewatch;
#[cfg(unix)]
pub mod health;
pub mod logs;
pub mod plan;
pub mod status;

#[cfg(unix)]
pub mod stack;
#[cfg(not(unix))]
#[path = "stack_stub.rs"]
pub mod stack;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{ForegroundRunner, ProcessHandle, ServiceRunner, SignalWatcher};

#[cfg(not(unix))]
mod stub;
#[cfg(not(unix))]
pub use stub::{ForegroundRunner, ServiceRunner, SignalWatcher};

pub use event::{
    event_channel, EventKind, EventReceiver, EventSender, EventSink, ServiceEvent, Stream,
};
pub use filewatch::{scan, spawn_watchers, watch_service, watched_services, FileStamp, Scan};
#[cfg(unix)]
pub use health::{HealthMonitor, HealthSignal};
pub use logs::{LogRouter, LogWriter};
pub use plan::{known_services, lookup_service, plan_stack};
pub use stack::{
    control_channel, Ack, Control, ControlRx, ControlTx, Readiness, ServiceReport, ServiceResult,
    StackOptions, StackOutcome, StackSupervisor,
};
pub use status::{Health, ServiceStatus, StatusRegistry};

/// Sending half of a shutdown channel.
///
/// Sending `Some(reason)` asks every subscribed runner to stop.  Sending again
/// while a shutdown is already in progress means "do not wait any longer" and
/// escalates to `SIGKILL`.
pub type ShutdownTx = watch::Sender<Option<ShutdownReason>>;
/// Receiving half of a shutdown channel.
pub type ShutdownRx = watch::Receiver<Option<ShutdownReason>>;

/// Create a shutdown channel that starts out with no shutdown requested.
pub fn shutdown_channel() -> (ShutdownTx, ShutdownRx) {
    watch::channel(None)
}

/// Resolve once a shutdown has been requested.
///
/// If the sender is dropped without a shutdown ever being requested, this
/// future simply never resolves — there is nobody left who could ask for one.
pub async fn wait_for_shutdown(rx: &mut ShutdownRx) -> ShutdownReason {
    loop {
        let current = *rx.borrow_and_update();
        if let Some(reason) = current {
            return reason;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// What to do with a service's standard output and error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// Hand the terminal straight to the child (used by `run`).
    #[default]
    Inherit,
    /// Read the child's output line by line and publish it as events (used by
    /// `up`, which has to interleave the output of several services).
    Capture,
}

/// Options controlling a single service run.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    /// Disable automatic restarts regardless of the configured policy
    /// (`--no-restart`).
    pub no_restart: bool,
    /// How to handle the child's output.
    pub output: OutputMode,
}

impl RunOptions {
    /// The effective restart policy for a service under these options.
    pub fn effective_policy(&self, configured: RestartPolicy) -> RestartPolicy {
        if self.no_restart {
            RestartPolicy::Never
        } else {
            configured
        }
    }

    /// Return a copy with the given output mode.
    pub fn with_output(mut self, output: OutputMode) -> Self {
        self.output = output;
        self
    }
}

/// How a single service run finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// The supervised process stopped and the restart policy did not call for
    /// another attempt.
    Exited {
        /// Why the last process run ended.
        reason: ExitReason,
        /// How many restarts were performed during this run.
        restarts: u32,
    },
    /// The run ended because the supervisor was asked to shut down.
    Stopped {
        /// Why the supervisor shut the service down.
        reason: ShutdownReason,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_restart_overrides_configured_policy() {
        let opts = RunOptions {
            no_restart: true,
            ..RunOptions::default()
        };
        assert_eq!(
            opts.effective_policy(RestartPolicy::Always),
            RestartPolicy::Never
        );
        assert_eq!(
            opts.effective_policy(RestartPolicy::OnFailure),
            RestartPolicy::Never
        );
    }

    #[test]
    fn default_options_keep_configured_policy() {
        let opts = RunOptions::default();
        assert_eq!(
            opts.effective_policy(RestartPolicy::Always),
            RestartPolicy::Always
        );
        assert_eq!(
            opts.effective_policy(RestartPolicy::Never),
            RestartPolicy::Never
        );
        assert_eq!(opts.output, OutputMode::Inherit);
    }

    #[test]
    fn output_mode_can_be_overridden() {
        let opts = RunOptions::default().with_output(OutputMode::Capture);
        assert_eq!(opts.output, OutputMode::Capture);
    }

    #[tokio::test]
    async fn wait_for_shutdown_resolves_on_request() {
        let (tx, mut rx) = shutdown_channel();
        tx.send(Some(ShutdownReason::UserInterrupt))
            .expect("receiver alive");
        assert_eq!(
            wait_for_shutdown(&mut rx).await,
            ShutdownReason::UserInterrupt
        );
    }

    #[tokio::test]
    async fn wait_for_shutdown_waits_for_a_later_request() {
        let (tx, mut rx) = shutdown_channel();
        let waiter = tokio::spawn(async move { wait_for_shutdown(&mut rx).await });
        tokio::task::yield_now().await;
        tx.send(Some(ShutdownReason::Terminated))
            .expect("receiver alive");
        assert_eq!(waiter.await.expect("task"), ShutdownReason::Terminated);
    }
}
