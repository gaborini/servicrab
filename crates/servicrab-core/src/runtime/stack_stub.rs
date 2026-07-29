//! Stub stack supervisor for platforms other than Linux and macOS.
//!
//! It mirrors the public shape of [`crate::runtime::stack`] so that the CLI can
//! be written without platform conditionals; every service is reported as
//! failed with [`RuntimeError::UnsupportedPlatform`].

use crate::config::{Config, ServiceName};
use crate::error::RuntimeError;
use crate::lifecycle::ShutdownReason;
use crate::runtime::event::EventSender;
use crate::runtime::{RunOutcome, ShutdownRx};

/// Options for a stack run.
#[derive(Debug, Clone, Copy, Default)]
pub struct StackOptions {
    /// Disable automatic restarts for every service.
    pub no_restart: bool,
    /// Tear the whole stack down as soon as one service fails.
    pub abort_on_failure: bool,
}

/// How one service ended during a stack run.
#[derive(Debug)]
pub enum ServiceResult {
    /// The service ran and stopped without a fatal error.
    Finished(RunOutcome),
    /// The service failed fatally.
    Failed(RuntimeError),
    /// The service was never started because a dependency did not come up.
    Skipped {
        /// The dependency that never became available.
        dependency: ServiceName,
    },
}

impl ServiceResult {
    /// Whether this counts as a failure for the exit status.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            ServiceResult::Failed(_) | ServiceResult::Skipped { .. }
        )
    }
}

/// The final report for a single service.
#[derive(Debug)]
pub struct ServiceReport {
    /// Which service this report is about.
    pub service: ServiceName,
    /// How it ended.
    pub result: ServiceResult,
}

/// The result of supervising a whole stack.
#[derive(Debug)]
pub struct StackOutcome {
    /// One report per planned service.
    pub reports: Vec<ServiceReport>,
    /// Why the stack was shut down, if it was.
    pub shutdown: Option<ShutdownReason>,
}

impl StackOutcome {
    /// Reports that count as failures.
    pub fn failures(&self) -> impl Iterator<Item = &ServiceReport> {
        self.reports.iter().filter(|r| r.result.is_failure())
    }

    /// Whether every service ended without a fatal error.
    pub fn is_success(&self) -> bool {
        self.failures().next().is_none()
    }
}

/// Placeholder stack supervisor.
pub struct StackSupervisor<'a> {
    plan: Vec<ServiceName>,
    _config: &'a Config,
    _options: StackOptions,
    _events: EventSender,
}

impl<'a> StackSupervisor<'a> {
    /// Build a supervisor for the given start plan.
    pub fn new(
        config: &'a Config,
        plan: Vec<ServiceName>,
        options: StackOptions,
        events: EventSender,
    ) -> Self {
        Self {
            plan,
            _config: config,
            _options: options,
            _events: events,
        }
    }

    /// The services this supervisor would start.
    pub fn plan(&self) -> &[ServiceName] {
        &self.plan
    }

    /// Always reports every service as unsupported on this platform.
    pub async fn run(self, _shutdown: &mut ShutdownRx) -> StackOutcome {
        let reports = self
            .plan
            .iter()
            .map(|service| ServiceReport {
                service: service.clone(),
                result: ServiceResult::Failed(RuntimeError::UnsupportedPlatform {
                    service: service.to_string(),
                }),
            })
            .collect();
        StackOutcome {
            reports,
            shutdown: None,
        }
    }
}
