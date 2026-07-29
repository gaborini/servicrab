//! Concurrent supervision of a whole stack of services (`servicrab up`).
//!
//! The supervisor starts every planned service in dependency order, keeps them
//! running according to their individual restart policies, and shuts the stack
//! down in reverse dependency order so that dependents stop before the services
//! they rely on.
//!
//! A service is considered "available" for its dependents as soon as its
//! process is running.  Real readiness probes are a later milestone; until then
//! a dependent may still need to retry its own connection attempts.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::config::{Config, Service, ServiceName};
use crate::error::RuntimeError;
use crate::lifecycle::{ServiceState, ShutdownReason};
use crate::runtime::event::{event_channel, EventKind, EventSender, EventSink, ServiceEvent};
use crate::runtime::unix::ServiceRunner;
use crate::runtime::{
    shutdown_channel, wait_for_shutdown, OutputMode, RunOptions, RunOutcome, ShutdownRx,
};

/// Extra time granted on top of a service's own shutdown timeout before the
/// supervisor stops waiting for its task and detaches it.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Options for a stack run.
#[derive(Debug, Clone, Copy, Default)]
pub struct StackOptions {
    /// Disable automatic restarts for every service (`--no-restart`).
    pub no_restart: bool,
    /// Tear the whole stack down as soon as one service fails.
    pub abort_on_failure: bool,
}

/// How one service ended during a stack run.
#[derive(Debug)]
pub enum ServiceResult {
    /// The service ran and stopped without a fatal error.
    Finished(RunOutcome),
    /// The service failed fatally (spawn failure or restart limit).
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
    /// One report per planned service, in completion order.
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

/// Supervises several services concurrently.
pub struct StackSupervisor<'a> {
    config: &'a Config,
    plan: Vec<ServiceName>,
    options: StackOptions,
    events: EventSender,
}

impl<'a> StackSupervisor<'a> {
    /// Build a supervisor for the given start plan.
    ///
    /// The plan must already be topologically ordered and contain every
    /// transitive dependency; use [`crate::runtime::plan_stack`] to build one.
    pub fn new(
        config: &'a Config,
        plan: Vec<ServiceName>,
        options: StackOptions,
        events: EventSender,
    ) -> Self {
        Self {
            config,
            plan,
            options,
            events,
        }
    }

    /// The services this supervisor will start, in start order.
    pub fn plan(&self) -> &[ServiceName] {
        &self.plan
    }

    /// Run the stack until every service has stopped or a shutdown is
    /// requested.
    pub async fn run(self, shutdown: &mut ShutdownRx) -> StackOutcome {
        let run_options = RunOptions {
            no_restart: self.options.no_restart,
            output: OutputMode::Capture,
        };

        let (report_tx, mut report_rx) = mpsc::unbounded_channel::<ServiceReport>();
        let mut states: BTreeMap<ServiceName, watch::Receiver<ServiceState>> = BTreeMap::new();
        let mut stops: BTreeMap<ServiceName, crate::runtime::ShutdownTx> = BTreeMap::new();
        let mut handles: BTreeMap<ServiceName, JoinHandle<()>> = BTreeMap::new();

        for name in &self.plan {
            let Some(service) = self.config.services.get(name) else {
                continue;
            };
            let service = Arc::new(service.clone());

            let (state_tx, state_rx) = watch::channel(ServiceState::Pending);
            let (stop_tx, stop_rx) = shutdown_channel();

            // Dependencies always appear earlier in the plan, so their state
            // channels already exist.
            let deps: Vec<(ServiceName, watch::Receiver<ServiceState>)> = service
                .depends_on
                .iter()
                .filter_map(|dep| states.get(dep).map(|rx| (dep.clone(), rx.clone())))
                .collect();

            states.insert(name.clone(), state_rx);
            stops.insert(name.clone(), stop_tx);

            handles.insert(
                name.clone(),
                tokio::spawn(supervise_service(
                    service,
                    deps,
                    state_tx,
                    stop_rx,
                    self.events.clone(),
                    run_options,
                    report_tx.clone(),
                )),
            );
        }

        drop(report_tx);

        let total = handles.len();
        let mut reports: Vec<ServiceReport> = Vec::with_capacity(total);
        let mut shutdown_reason: Option<ShutdownReason> = None;

        while reports.len() < total {
            tokio::select! {
                reason = wait_for_shutdown(shutdown) => {
                    shutdown_reason = Some(reason);
                    break;
                }
                report = report_rx.recv() => {
                    let Some(report) = report else { break };
                    let failed = report.result.is_failure();
                    tracing::debug!(service = %report.service, "service finished");
                    reports.push(report);
                    if failed && self.options.abort_on_failure {
                        shutdown_reason = Some(ShutdownReason::StackFailure);
                        break;
                    }
                }
            }
        }

        if let Some(reason) = shutdown_reason {
            self.stop_all(reason, &stops, &mut handles).await;
        }

        for (name, handle) in handles {
            if let Err(err) = handle.await {
                tracing::warn!(service = %name, error = %err, "supervision task did not finish cleanly");
            }
        }

        // Every task has finished, so the remaining reports are already
        // queued.
        while let Some(report) = report_rx.recv().await {
            reports.push(report);
        }

        StackOutcome {
            reports,
            shutdown: shutdown_reason,
        }
    }

    /// Stop the stack in reverse dependency order, waiting for each service
    /// before moving on to the ones it depends on.
    async fn stop_all(
        &self,
        reason: ShutdownReason,
        stops: &BTreeMap<ServiceName, crate::runtime::ShutdownTx>,
        handles: &mut BTreeMap<ServiceName, JoinHandle<()>>,
    ) {
        for name in self.plan.iter().rev() {
            let Some(stop) = stops.get(name) else {
                continue;
            };
            let _ = stop.send(Some(reason));

            let Some(mut handle) = handles.remove(name) else {
                continue;
            };
            let grace = self
                .config
                .services
                .get(name)
                .map(|service| service.shutdown_timeout)
                .unwrap_or_default()
                + STOP_GRACE;

            if tokio::time::timeout(grace, &mut handle).await.is_err() {
                tracing::warn!(service = %name, ?grace, "service did not stop in time; detaching");
                handle.abort();
            }
        }
    }
}

/// Whether a service may start yet.
enum DependencyWait {
    /// Every dependency is available.
    Ready,
    /// A dependency will never become available.
    Blocked(ServiceName),
    /// A shutdown was requested while waiting.
    Shutdown(ShutdownReason),
}

/// Supervise one service: wait for its dependencies, then run it.
#[allow(clippy::too_many_arguments)]
async fn supervise_service(
    service: Arc<Service>,
    deps: Vec<(ServiceName, watch::Receiver<ServiceState>)>,
    state: watch::Sender<ServiceState>,
    mut stop: ShutdownRx,
    events: EventSender,
    options: RunOptions,
    reports: mpsc::UnboundedSender<ServiceReport>,
) {
    let name = service.name.clone();

    match wait_for_dependencies(&deps, &mut stop).await {
        DependencyWait::Ready => {}
        DependencyWait::Blocked(dependency) => {
            let _ = events.send(ServiceEvent::new(
                name.clone(),
                EventKind::Skipped {
                    dependency: dependency.clone(),
                },
            ));
            let _ = state.send(ServiceState::Failed);
            let _ = reports.send(ServiceReport {
                service: name,
                result: ServiceResult::Skipped { dependency },
            });
            return;
        }
        DependencyWait::Shutdown(reason) => {
            let _ = state.send(ServiceState::Stopped);
            let _ = reports.send(ServiceReport {
                service: name,
                result: ServiceResult::Finished(RunOutcome::Stopped { reason }),
            });
            return;
        }
    }

    // Relay the runner's events to the stack-wide stream while mirroring state
    // changes into the watch channel that dependents are waiting on.
    let (tx, mut rx) = event_channel();
    let relay = {
        let global = events.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let EventKind::State(next) = &event.kind {
                    let _ = state.send(*next);
                }
                let _ = global.send(event);
            }
        })
    };

    let result = {
        let mut runner = ServiceRunner::new(&service, options).with_events(EventSink::new(tx));
        runner.run(&mut stop).await
        // `runner` is dropped here, which closes the event channel and ends
        // the relay task.
    };

    let _ = relay.await;

    let result = match result {
        Ok(outcome) => ServiceResult::Finished(outcome),
        Err(err) => {
            let _ = events.send(ServiceEvent::new(
                name.clone(),
                EventKind::Failed {
                    message: err.to_string(),
                },
            ));
            ServiceResult::Failed(err)
        }
    };

    let _ = reports.send(ServiceReport {
        service: name,
        result,
    });
}

/// Wait until every dependency is available (or will never be).
async fn wait_for_dependencies(
    deps: &[(ServiceName, watch::Receiver<ServiceState>)],
    stop: &mut ShutdownRx,
) -> DependencyWait {
    for (name, receiver) in deps {
        let mut receiver = receiver.clone();
        loop {
            match *receiver.borrow_and_update() {
                // Running: the process is up.  Exited: a one-shot dependency
                // (a migration, a build step) already did its job.
                ServiceState::Running | ServiceState::Exited => break,
                ServiceState::Failed | ServiceState::Stopped => {
                    return DependencyWait::Blocked(name.clone())
                }
                _ => {}
            }

            tokio::select! {
                changed = receiver.changed() => {
                    if changed.is_err() {
                        return DependencyWait::Blocked(name.clone());
                    }
                }
                reason = wait_for_shutdown(stop) => return DependencyWait::Shutdown(reason),
            }
        }
    }
    DependencyWait::Ready
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(raw: &str) -> ServiceName {
        crate::validation::validate_service_name(raw).expect("valid test name")
    }

    #[test]
    fn skipped_and_failed_results_count_as_failures() {
        assert!(ServiceResult::Skipped {
            dependency: name("db")
        }
        .is_failure());
        assert!(!ServiceResult::Finished(RunOutcome::Exited {
            reason: crate::lifecycle::ExitReason::Code(0),
            restarts: 0
        })
        .is_failure());
    }

    #[test]
    fn outcome_reports_success_only_without_failures() {
        let ok = StackOutcome {
            reports: vec![ServiceReport {
                service: name("api"),
                result: ServiceResult::Finished(RunOutcome::Stopped {
                    reason: ShutdownReason::UserInterrupt,
                }),
            }],
            shutdown: Some(ShutdownReason::UserInterrupt),
        };
        assert!(ok.is_success());

        let bad = StackOutcome {
            reports: vec![ServiceReport {
                service: name("api"),
                result: ServiceResult::Skipped {
                    dependency: name("db"),
                },
            }],
            shutdown: None,
        };
        assert!(!bad.is_success());
        assert_eq!(bad.failures().count(), 1);
    }

    #[tokio::test]
    async fn dependencies_are_awaited_until_running() {
        let (tx, rx) = watch::channel(ServiceState::Pending);
        let (_stop_tx, mut stop_rx) = shutdown_channel();
        let deps = vec![(name("db"), rx)];

        let waiter = tokio::spawn(async move {
            let (_stop_tx2, _) = shutdown_channel();
            matches!(
                wait_for_dependencies(&deps, &mut stop_rx).await,
                DependencyWait::Ready
            )
        });

        tokio::task::yield_now().await;
        tx.send(ServiceState::Starting).expect("receiver alive");
        tx.send(ServiceState::Running).expect("receiver alive");

        assert!(waiter.await.expect("task"));
    }

    #[tokio::test]
    async fn a_failed_dependency_blocks_the_dependent() {
        let (tx, rx) = watch::channel(ServiceState::Pending);
        let (_stop_tx, mut stop_rx) = shutdown_channel();
        let deps = vec![(name("db"), rx)];

        tx.send(ServiceState::Failed).expect("receiver alive");

        assert!(matches!(
            wait_for_dependencies(&deps, &mut stop_rx).await,
            DependencyWait::Blocked(blocked) if blocked.as_str() == "db"
        ));
    }

    #[tokio::test]
    async fn shutdown_interrupts_the_dependency_wait() {
        let (_tx, rx) = watch::channel(ServiceState::Pending);
        let (stop_tx, mut stop_rx) = shutdown_channel();
        let deps = vec![(name("db"), rx)];

        stop_tx
            .send(Some(ShutdownReason::UserInterrupt))
            .expect("receiver alive");

        assert!(matches!(
            wait_for_dependencies(&deps, &mut stop_rx).await,
            DependencyWait::Shutdown(ShutdownReason::UserInterrupt)
        ));
    }
}
