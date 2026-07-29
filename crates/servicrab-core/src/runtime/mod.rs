//! Process runtime: spawning, signalling, and supervising a single service in
//! the foreground.
//!
//! The pure restart logic lives in [`crate::lifecycle`]; this module only deals
//! with the operating-system side of running a process.  User-facing formatting
//! and exit-code mapping belong to the CLI, not here.
//!
//! Only Linux and macOS are supported.  On other platforms every entry point
//! returns [`crate::error::RuntimeError::UnsupportedPlatform`].
//!
//! ## Future phases (TODOs)
//!
//! - TODO(phase-2): Generalise [`ForegroundRunner`] into a multi-service
//!   supervisor driven by the dependency start order.
//! - TODO(phase-2): Capture stdout/stderr instead of inheriting them, so the
//!   daemon can persist logs.

use crate::config::RestartPolicy;
use crate::lifecycle::{ExitReason, ShutdownReason};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{ForegroundRunner, ProcessHandle};

#[cfg(not(unix))]
mod stub;
#[cfg(not(unix))]
pub use stub::ForegroundRunner;

/// Options controlling a single foreground run.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    /// Disable automatic restarts regardless of the configured policy
    /// (`--no-restart`).
    pub no_restart: bool,
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
}

/// How a foreground run finished.
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
        let opts = RunOptions { no_restart: true };
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
    }
}
