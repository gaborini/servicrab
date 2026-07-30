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
use crate::lifecycle::{ExitReason, ServiceState, ShutdownReason};
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
    /// Keep supervising even when every service has stopped.
    ///
    /// The daemon needs this: an operator may stop every service and start one
    /// again later, which is only possible while the supervisor is alive.
    pub keep_running: bool,
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

/// Acknowledgement channel for a [`Control`] command.
///
/// The message describes what happened ("started", "restarted", …); an error
/// explains why the command was refused.
pub type Ack = tokio::sync::oneshot::Sender<Result<String, String>>;

/// A command an operator sends to a running stack.
#[derive(Debug)]
pub enum Control {
    /// Start a service that is not running.
    Start {
        /// Which service.
        service: ServiceName,
        /// Answered once the service has been spawned.
        ack: Ack,
    },
    /// Stop a running service, leaving the rest of the stack alone.
    Stop {
        /// Which service.
        service: ServiceName,
        /// Answered once the service has actually stopped.
        ack: Ack,
    },
    /// Stop a service and start it again.
    Restart {
        /// Which service.
        service: ServiceName,
        /// Answered once the replacement has been spawned.
        ack: Ack,
    },
}

/// Sending half of the control channel.
pub type ControlTx = mpsc::UnboundedSender<Control>;
/// Receiving half of the control channel.
pub type ControlRx = mpsc::UnboundedReceiver<Control>;

/// Create a control channel.
pub fn control_channel() -> (ControlTx, ControlRx) {
    mpsc::unbounded_channel()
}

/// Everything the supervisor needs to run — and re-run — one service.
struct Slot {
    service: Arc<Service>,
    deps: Vec<(ServiceName, watch::Receiver<Readiness>)>,
    readiness: Arc<watch::Sender<Readiness>>,
    /// Present exactly while a supervision task is alive.
    stop: Option<crate::runtime::ShutdownTx>,
    handle: Option<JoinHandle<()>>,
    /// Set while a stop is only the first half of a restart.
    restart_when_stopped: bool,
    /// The client waiting for the in-flight command to complete.
    pending: Option<Ack>,
}

impl Slot {
    /// Start a supervision task for this service.
    fn spawn(
        &mut self,
        events: EventSender,
        options: RunOptions,
        reports: mpsc::UnboundedSender<ServiceReport>,
    ) {
        // A previous run may have left the signal at `Gone`; dependents that
        // are still waiting must not act on that stale verdict.
        let _ = self.readiness.send(Readiness::Pending);

        let (stop_tx, stop_rx) = shutdown_channel();
        self.stop = Some(stop_tx);
        self.handle = Some(tokio::spawn(supervise_service(
            Arc::clone(&self.service),
            self.deps.clone(),
            Arc::clone(&self.readiness),
            stop_rx,
            events,
            options,
            reports,
        )));
    }

    fn is_running(&self) -> bool {
        self.handle.is_some()
    }

    /// Answer the client waiting on this slot, if there is one.
    fn answer(&mut self, result: Result<String, String>) {
        if let Some(ack) = self.pending.take() {
            let _ = ack.send(result);
        }
    }
}

/// Supervises several services concurrently.
pub struct StackSupervisor<'a> {
    config: &'a Config,
    plan: Vec<ServiceName>,
    options: StackOptions,
    events: EventSender,
    control: Option<ControlRx>,
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
            control: None,
        }
    }

    /// Accept per-service commands while the stack runs.
    pub fn with_control(mut self, control: ControlRx) -> Self {
        self.control = Some(control);
        self
    }

    /// The services this supervisor will start, in start order.
    pub fn plan(&self) -> &[ServiceName] {
        &self.plan
    }

    /// Run the stack until every service has stopped or a shutdown is
    /// requested.
    pub async fn run(mut self, shutdown: &mut ShutdownRx) -> StackOutcome {
        let run_options = RunOptions {
            no_restart: self.options.no_restart,
            output: OutputMode::Capture,
        };

        let (report_tx, mut report_rx) = mpsc::unbounded_channel::<ServiceReport>();
        let mut slots: BTreeMap<ServiceName, Slot> = BTreeMap::new();

        for name in &self.plan {
            let Some(service) = self.config.services.get(name) else {
                continue;
            };
            let (state_tx, state_rx) = watch::channel(Readiness::Pending);

            // Dependencies always appear earlier in the plan, so their slots
            // already exist.
            let deps: Vec<(ServiceName, watch::Receiver<Readiness>)> = service
                .depends_on
                .iter()
                .filter_map(|dep| {
                    slots
                        .get(dep)
                        .map(|slot| (dep.clone(), slot.readiness.subscribe()))
                })
                .collect();

            let mut slot = Slot {
                service: Arc::new(service.clone()),
                deps,
                readiness: Arc::new(state_tx),
                stop: None,
                handle: None,
                restart_when_stopped: false,
                pending: None,
            };
            drop(state_rx);
            slot.spawn(self.events.clone(), run_options, report_tx.clone());
            slots.insert(name.clone(), slot);
        }

        let mut running = slots.len();
        let mut reports: Vec<ServiceReport> = Vec::with_capacity(running);
        let mut shutdown_reason: Option<ShutdownReason> = None;

        loop {
            // Without a control channel the stack is done when its services
            // are; the daemon instead waits to be told to stop.
            if running == 0 && !self.options.keep_running {
                break;
            }

            tokio::select! {
                reason = wait_for_shutdown(shutdown) => {
                    shutdown_reason = Some(reason);
                    break;
                }
                Some(report) = report_rx.recv() => {
                    running -= 1;
                    let failed = report.result.is_failure();
                    tracing::debug!(service = %report.service, "service finished");

                    if let Some(slot) = slots.get_mut(&report.service) {
                        slot.stop = None;
                        slot.handle = None;
                        if slot.restart_when_stopped {
                            slot.restart_when_stopped = false;
                            slot.spawn(self.events.clone(), run_options, report_tx.clone());
                            running += 1;
                            slot.answer(Ok("restarted".to_string()));
                        } else {
                            slot.answer(Ok("stopped".to_string()));
                        }
                    }

                    reports.push(report);
                    if failed && self.options.abort_on_failure {
                        shutdown_reason = Some(ShutdownReason::StackFailure);
                        break;
                    }
                }
                command = next_control(&mut self.control) => {
                    self.handle_control(
                        command,
                        &mut slots,
                        &mut running,
                        run_options,
                        &report_tx,
                    );
                }
            }
        }

        let stops: BTreeMap<ServiceName, crate::runtime::ShutdownTx> = slots
            .iter()
            .filter_map(|(name, slot)| slot.stop.clone().map(|stop| (name.clone(), stop)))
            .collect();
        let mut handles: BTreeMap<ServiceName, JoinHandle<()>> = slots
            .iter_mut()
            .filter_map(|(name, slot)| slot.handle.take().map(|handle| (name.clone(), handle)))
            .collect();

        if let Some(reason) = shutdown_reason {
            self.stop_all(reason, &stops, &mut handles).await;
        }

        for (name, handle) in handles {
            if let Err(err) = handle.await {
                tracing::warn!(service = %name, error = %err, "supervision task did not finish cleanly");
            }
        }

        // Every task has finished, so the remaining reports are already
        // queued.  The supervisor holds the last sender, so the channel is
        // drained rather than waited on.
        drop(report_tx);
        while let Some(report) = report_rx.recv().await {
            reports.push(report);
        }

        StackOutcome {
            reports,
            shutdown: shutdown_reason,
        }
    }

    /// Apply one operator command to the running stack.
    fn handle_control(
        &self,
        command: Control,
        slots: &mut BTreeMap<ServiceName, Slot>,
        running: &mut usize,
        run_options: RunOptions,
        reports: &mpsc::UnboundedSender<ServiceReport>,
    ) {
        let service = match &command {
            Control::Start { service, .. }
            | Control::Stop { service, .. }
            | Control::Restart { service, .. } => service.clone(),
        };

        let Some(slot) = slots.get_mut(&service) else {
            let ack = into_ack(command);
            let _ = ack.send(Err(format!("{service} is not part of the running stack")));
            return;
        };

        // A slot can only track one command at a time; queuing them would
        // make "stop then restart" ambiguous.
        if slot.pending.is_some() {
            let ack = into_ack(command);
            let _ = ack.send(Err(format!(
                "{service} is already busy with another command"
            )));
            return;
        }

        match command {
            Control::Start { ack, .. } => {
                if slot.is_running() {
                    let _ = ack.send(Err(format!("{service} is already running")));
                    return;
                }
                slot.spawn(self.events.clone(), run_options, reports.clone());
                *running += 1;
                let _ = ack.send(Ok("started".to_string()));
            }
            Control::Stop { ack, .. } => {
                let Some(stop) = slot.stop.clone() else {
                    let _ = ack.send(Ok("already stopped".to_string()));
                    return;
                };
                slot.pending = Some(ack);
                let _ = stop.send(Some(ShutdownReason::Requested));
            }
            Control::Restart { ack, .. } => {
                let Some(stop) = slot.stop.clone() else {
                    slot.spawn(self.events.clone(), run_options, reports.clone());
                    *running += 1;
                    let _ = ack.send(Ok("started".to_string()));
                    return;
                };
                slot.restart_when_stopped = true;
                slot.pending = Some(ack);
                let _ = stop.send(Some(ShutdownReason::Requested));
            }
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

/// Wait for the next operator command, or forever when there is no control
/// channel.
async fn next_control(control: &mut Option<ControlRx>) -> Control {
    match control {
        Some(rx) => match rx.recv().await {
            Some(command) => command,
            // Every client is gone; the stack keeps running unattended.
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// Take the acknowledgement channel out of a command.
fn into_ack(command: Control) -> Ack {
    match command {
        Control::Start { ack, .. } | Control::Stop { ack, .. } | Control::Restart { ack, .. } => {
            ack
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

/// Whether a service can be depended on yet.
///
/// This is a coarser signal than [`ServiceState`]: dependents do not care
/// about backoff or restarts, only about "can I start now", "keep waiting" and
/// "this will never happen".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Not available yet; keep waiting.
    Pending,
    /// Available: dependents may start.
    Ready,
    /// It will never become available.
    Gone,
}

/// Supervise one service: wait for its dependencies, then run it.
#[allow(clippy::too_many_arguments)]
async fn supervise_service(
    service: Arc<Service>,
    deps: Vec<(ServiceName, watch::Receiver<Readiness>)>,
    readiness: Arc<watch::Sender<Readiness>>,
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
            let _ = readiness.send(Readiness::Gone);
            let _ = reports.send(ServiceReport {
                service: name,
                result: ServiceResult::Skipped { dependency },
            });
            return;
        }
        DependencyWait::Shutdown(reason) => {
            let _ = readiness.send(Readiness::Gone);
            let _ = reports.send(ServiceReport {
                service: name,
                result: ServiceResult::Finished(RunOutcome::Stopped { reason }),
            });
            return;
        }
    }

    // Relay the runner's events to the stack-wide stream while deriving the
    // readiness signal that dependents are waiting on.
    let (tx, mut rx) = event_channel();
    let relay = {
        let global = events.clone();
        // A service with a health check is only ready once a probe passes;
        // without one, "the process is up" is the best signal available.
        let mut tracker = ReadinessTracker::new(service.health.is_some());
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Some(next) = tracker.observe(&event.kind) {
                    let _ = readiness.send(next);
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

/// Derives the readiness signal dependents wait on from a service's events.
struct ReadinessTracker {
    /// Whether the service promised a health check, in which case being up is
    /// not enough to be ready.
    gate_on_health: bool,
    /// Whether the current process run is known to be unhealthy.
    unhealthy: bool,
}

impl ReadinessTracker {
    fn new(gate_on_health: bool) -> Self {
        Self {
            gate_on_health,
            unhealthy: false,
        }
    }

    /// Map one runtime event to the readiness it implies, if any.
    fn observe(&mut self, kind: &EventKind) -> Option<Readiness> {
        match kind {
            // A passing health probe always means ready.
            EventKind::Healthy => {
                self.unhealthy = false;
                Some(Readiness::Ready)
            }
            // A failing one takes readiness away again, so a dependent that
            // has not started yet keeps waiting for the service to recover.
            EventKind::Unhealthy { .. } => {
                self.unhealthy = true;
                Some(Readiness::Pending)
            }
            EventKind::Exited {
                reason: ExitReason::Unhealthy,
                ..
            } => {
                self.unhealthy = true;
                None
            }
            EventKind::State(state) => match state {
                // A restart gets a clean slate: the new process has to prove
                // itself healthy again.
                ServiceState::Starting => {
                    self.unhealthy = false;
                    None
                }
                // The process is up: enough on its own, unless the service
                // promised a health check that has not passed yet.
                ServiceState::Running if !self.gate_on_health => Some(Readiness::Ready),
                // A one-shot dependency (a migration, a build step) that
                // already did its job counts as available — unless it was
                // stopped precisely because it was unhealthy.
                ServiceState::Exited if !self.unhealthy => Some(Readiness::Ready),
                ServiceState::Exited | ServiceState::Failed | ServiceState::Stopped => {
                    Some(Readiness::Gone)
                }
                _ => None,
            },
            _ => None,
        }
    }
}

/// Wait until every dependency is available (or will never be).
async fn wait_for_dependencies(
    deps: &[(ServiceName, watch::Receiver<Readiness>)],
    stop: &mut ShutdownRx,
) -> DependencyWait {
    for (name, receiver) in deps {
        let mut receiver = receiver.clone();
        loop {
            match *receiver.borrow_and_update() {
                Readiness::Ready => break,
                Readiness::Gone => return DependencyWait::Blocked(name.clone()),
                Readiness::Pending => {}
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
    async fn dependencies_are_awaited_until_ready() {
        let (tx, rx) = watch::channel(Readiness::Pending);
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
        tx.send(Readiness::Ready).expect("receiver alive");

        assert!(waiter.await.expect("task"));
    }

    #[tokio::test]
    async fn a_failed_dependency_blocks_the_dependent() {
        let (tx, rx) = watch::channel(Readiness::Pending);
        let (_stop_tx, mut stop_rx) = shutdown_channel();
        let deps = vec![(name("db"), rx)];

        tx.send(Readiness::Gone).expect("receiver alive");

        assert!(matches!(
            wait_for_dependencies(&deps, &mut stop_rx).await,
            DependencyWait::Blocked(blocked) if blocked.as_str() == "db"
        ));
    }

    #[test]
    fn readiness_follows_the_process_when_there_is_no_health_check() {
        let mut tracker = ReadinessTracker::new(false);
        assert_eq!(
            tracker.observe(&EventKind::State(ServiceState::Running)),
            Some(Readiness::Ready)
        );
        assert_eq!(
            tracker.observe(&EventKind::State(ServiceState::Backoff)),
            None
        );
        assert_eq!(
            tracker.observe(&EventKind::State(ServiceState::Failed)),
            Some(Readiness::Gone)
        );
    }

    #[test]
    fn a_health_checked_service_is_only_ready_once_a_probe_passes() {
        let mut tracker = ReadinessTracker::new(true);
        assert_eq!(
            tracker.observe(&EventKind::State(ServiceState::Running)),
            None
        );
        assert_eq!(tracker.observe(&EventKind::Healthy), Some(Readiness::Ready));
        assert_eq!(
            tracker.observe(&EventKind::Unhealthy {
                message: "boom".to_string()
            }),
            Some(Readiness::Pending)
        );
    }

    #[test]
    fn a_one_shot_dependency_counts_as_ready_once_it_exits() {
        for gate in [false, true] {
            let mut tracker = ReadinessTracker::new(gate);
            assert_eq!(
                tracker.observe(&EventKind::State(ServiceState::Exited)),
                Some(Readiness::Ready)
            );
        }
    }

    #[test]
    fn a_service_stopped_for_being_unhealthy_never_becomes_available() {
        let mut tracker = ReadinessTracker::new(true);
        assert_eq!(
            tracker.observe(&EventKind::Unhealthy {
                message: "boom".to_string()
            }),
            Some(Readiness::Pending)
        );
        assert_eq!(
            tracker.observe(&EventKind::Exited {
                reason: ExitReason::Unhealthy,
                uptime: Duration::from_secs(1),
            }),
            None
        );
        assert_eq!(
            tracker.observe(&EventKind::State(ServiceState::Exited)),
            Some(Readiness::Gone)
        );
    }

    #[test]
    fn a_restart_resets_the_unhealthy_verdict() {
        let mut tracker = ReadinessTracker::new(true);
        tracker.observe(&EventKind::Unhealthy {
            message: "boom".to_string(),
        });
        assert_eq!(
            tracker.observe(&EventKind::State(ServiceState::Starting)),
            None
        );
        assert_eq!(tracker.observe(&EventKind::Healthy), Some(Readiness::Ready));
    }

    #[tokio::test]
    async fn shutdown_interrupts_the_dependency_wait() {
        let (_tx, rx) = watch::channel(Readiness::Pending);
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
