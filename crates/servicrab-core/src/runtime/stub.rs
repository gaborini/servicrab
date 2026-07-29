//! Stub foreground runner for platforms other than Linux and macOS.
//!
//! Windows support is out of scope for now; the stub exists so that the
//! workspace still compiles everywhere.

use crate::config::{RestartPolicy, Service};
use crate::error::RuntimeError;
use crate::runtime::{RunOptions, RunOutcome};

/// Placeholder runner that always reports an unsupported platform.
pub struct ForegroundRunner<'a> {
    service: &'a Service,
    policy: RestartPolicy,
}

impl<'a> ForegroundRunner<'a> {
    /// Build a runner for a validated service.
    pub fn new(service: &'a Service, options: RunOptions) -> Self {
        Self {
            policy: options.effective_policy(service.restart),
            service,
        }
    }

    /// The effective restart policy after `--no-restart` is taken into account.
    pub fn policy(&self) -> RestartPolicy {
        self.policy
    }

    /// Always fails with [`RuntimeError::UnsupportedPlatform`].
    pub async fn run(&mut self) -> Result<RunOutcome, RuntimeError> {
        Err(RuntimeError::UnsupportedPlatform {
            service: self.service.name.to_string(),
        })
    }
}
