//! Unix implementation of the foreground process runner.
//!
//! Each supervised service is placed into its own process group so that
//! shutdown can target the whole tree, not just the direct child:
//!
//! ```text
//! servicrab
//!   └── npm            ← direct child, leader of a new process group
//!       └── node
//!           └── esbuild
//! ```
//!
//! Signalling only the direct child would leave `node` and `esbuild` running,
//! so every signal is delivered with `killpg(2)`.

use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};

use crate::config::{RestartPolicy, Service, ShutdownSignal};
use crate::error::RuntimeError;
use crate::lifecycle::{
    ExitReason, ProcessOutcome, RestartDecision, RestartTracker, ServiceState, ShutdownReason,
    StateMachine,
};
use crate::runtime::{RunOptions, RunOutcome};

/// Map a validated [`ShutdownSignal`] onto the OS signal.
fn os_signal(signal: ShutdownSignal) -> Signal {
    match signal {
        ShutdownSignal::Term => Signal::SIGTERM,
        ShutdownSignal::Int => Signal::SIGINT,
        ShutdownSignal::Quit => Signal::SIGQUIT,
        ShutdownSignal::Hup => Signal::SIGHUP,
    }
}

/// Whether a spawn error is worth retrying.
///
/// A missing executable or a permission problem will not fix itself, so those
/// are fatal; transient resource shortages are retryable.
fn spawn_error_is_retryable(err: &std::io::Error) -> bool {
    !matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    )
}

/// A running service process and the process group it leads.
#[derive(Debug)]
pub struct ProcessHandle {
    child: Child,
    pgid: Pid,
    started_at: Instant,
}

impl ProcessHandle {
    /// Spawn the service's configured executable.
    ///
    /// The executable and its arguments are passed verbatim — no shell is
    /// involved unless the user configured a shell as the executable.
    pub fn spawn(service: &Service) -> Result<Self, RuntimeError> {
        let mut command = Command::new(&service.executable);
        command
            .args(&service.args)
            .current_dir(&service.cwd)
            .env_clear()
            .envs(&service.env)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            // Put the child into a brand-new process group whose id equals the
            // child's pid.  This is done by the standard library between fork
            // and exec via `setpgid(2)`, so no `unsafe` pre-exec hook of our
            // own is required.
            .process_group(0)
            .kill_on_drop(true);

        let child = command
            .spawn()
            .map_err(|source| RuntimeError::SpawnFailed {
                service: service.name.to_string(),
                executable: service.executable.clone(),
                source,
            })?;

        // A successfully spawned child always has a pid; it is only taken away
        // once the child has been reaped, which cannot have happened yet.
        let pid = child.id().ok_or_else(|| RuntimeError::WaitFailed {
            service: service.name.to_string(),
            source: std::io::Error::other("child exited before its pid could be read"),
        })?;

        let pgid = Pid::from_raw(pid as i32);

        Ok(Self {
            child,
            pgid,
            started_at: Instant::now(),
        })
    }

    /// The process-group id (equal to the direct child's pid).
    pub fn pgid(&self) -> i32 {
        self.pgid.as_raw()
    }

    /// How long the process has been running.
    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Send a signal to the entire process group.
    ///
    /// A group that has already gone away is not an error.
    fn signal_group(&self, service: &str, sig: Signal) -> Result<(), RuntimeError> {
        match killpg(self.pgid, sig) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(errno) => Err(RuntimeError::SignalDeliveryFailed {
                service: service.to_string(),
                signal: sig.as_str().to_string(),
                pgid: self.pgid.as_raw(),
                source: std::io::Error::from(errno),
            }),
        }
    }

    /// `SIGKILL` any process left in the group, ignoring an already-empty
    /// group.  Used to make sure no descendant outlives the supervisor.
    fn kill_group(&self, service: &str, timeout: Duration) -> Result<(), RuntimeError> {
        match killpg(self.pgid, Signal::SIGKILL) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(errno) => Err(RuntimeError::ForceKillFailed {
                service: service.to_string(),
                pgid: self.pgid.as_raw(),
                timeout,
                source: std::io::Error::from(errno),
            }),
        }
    }

    /// Wait for the direct child to exit, reaping it.
    async fn wait(&mut self, service: &str) -> Result<ExitReason, RuntimeError> {
        let status = self
            .child
            .wait()
            .await
            .map_err(|source| RuntimeError::WaitFailed {
                service: service.to_string(),
                source,
            })?;
        Ok(exit_reason(status))
    }
}

fn exit_reason(status: std::process::ExitStatus) -> ExitReason {
    match (status.code(), status.signal()) {
        (Some(code), _) => ExitReason::Code(code),
        (None, Some(sig)) => ExitReason::Signal(sig),
        // Neither a code nor a signal should be possible, but do not panic on a
        // surprising status.
        (None, None) => ExitReason::Code(-1),
    }
}

/// Why the supervision loop woke up.
enum Wake {
    /// The child process exited on its own.
    Exited(ExitReason),
    /// A shutdown signal was received.
    Signalled(ShutdownReason),
}

/// Runs a single configured service in the foreground, applying its restart
/// policy and handling shutdown signals.
pub struct ForegroundRunner<'a> {
    service: &'a Service,
    tracker: RestartTracker,
    state: StateMachine,
}

impl<'a> ForegroundRunner<'a> {
    /// Build a runner for a validated service.
    pub fn new(service: &'a Service, options: RunOptions) -> Self {
        let policy = options.effective_policy(service.restart);
        Self {
            service,
            tracker: RestartTracker::from_service(service).with_policy(policy),
            state: StateMachine::new(),
        }
    }

    /// The effective restart policy after `--no-restart` is taken into account.
    pub fn policy(&self) -> RestartPolicy {
        self.tracker.policy()
    }

    fn transition(&mut self, next: ServiceState) -> Result<(), RuntimeError> {
        self.state
            .try_transition(next)
            .map_err(|source| RuntimeError::InvalidTransition {
                service: self.service.name.to_string(),
                source,
            })?;
        tracing::debug!(service = %self.service.name, state = %next, "state transition");
        Ok(())
    }

    /// Run the service until it stops for good.
    ///
    /// The returned [`RunOutcome`] describes how the run finished; mapping that
    /// to a process exit code is the caller's job.
    pub async fn run(&mut self) -> Result<RunOutcome, RuntimeError> {
        let name = self.service.name.to_string();

        let mut sigint = signal(SignalKind::interrupt()).map_err(|source| {
            RuntimeError::SignalRegistrationFailed {
                service: name.clone(),
                signal: "SIGINT",
                source,
            }
        })?;
        let mut sigterm = signal(SignalKind::terminate()).map_err(|source| {
            RuntimeError::SignalRegistrationFailed {
                service: name.clone(),
                signal: "SIGTERM",
                source,
            }
        })?;

        let mut restarts = 0u32;

        loop {
            self.transition(ServiceState::Starting)?;
            tracing::info!(
                service = %name,
                executable = %self.service.executable,
                cwd = %self.service.cwd.display(),
                "starting service"
            );

            let mut handle = match ProcessHandle::spawn(self.service) {
                Ok(handle) => handle,
                Err(err) => {
                    // Feed the spawn failure through the restart policy so that
                    // a transient failure can be retried like any other.
                    let retryable = match &err {
                        RuntimeError::SpawnFailed { source, .. } => {
                            spawn_error_is_retryable(source)
                        }
                        _ => false,
                    };
                    let outcome =
                        ProcessOutcome::new(ExitReason::SpawnFailure { retryable }, Duration::ZERO);
                    match self.tracker.decide(outcome, None) {
                        RestartDecision::Restart { delay, attempt } => {
                            tracing::warn!(service = %name, error = %err, "spawn failed");
                            self.transition(ServiceState::Backoff)?;
                            if let Some(reason) =
                                wait_backoff(&name, delay, attempt, &mut sigint, &mut sigterm).await
                            {
                                self.transition(ServiceState::Stopped)?;
                                return Ok(RunOutcome::Stopped { reason });
                            }
                            restarts += 1;
                            continue;
                        }
                        RestartDecision::Stop | RestartDecision::Fail { .. } => {
                            let _ = self.transition(ServiceState::Failed);
                            return Err(err);
                        }
                    }
                }
            };

            self.transition(ServiceState::Running)?;
            tracing::info!(service = %name, pid = handle.pgid(), "service running");

            // The select! branches cannot borrow `handle` twice, so the wake-up
            // reason is captured first and shutdown is driven afterwards.
            let wake = tokio::select! {
                result = handle.wait(&name) => Wake::Exited(result?),
                _ = sigint.recv() => Wake::Signalled(ShutdownReason::UserInterrupt),
                _ = sigterm.recv() => Wake::Signalled(ShutdownReason::Terminated),
            };

            let (reason, shutdown) = match wake {
                Wake::Exited(reason) => (reason, None),
                Wake::Signalled(why) => {
                    let reason = self
                        .shutdown(&mut handle, why, &mut sigint, &mut sigterm)
                        .await?;
                    (reason, Some(why))
                }
            };

            let uptime = handle.uptime();

            // Sweep the group so no descendant outlives the direct child.
            handle.kill_group(&name, self.service.shutdown_timeout)?;

            if let Some(reason) = shutdown {
                tracing::info!(service = %name, %reason, "service stopped");
                self.transition(ServiceState::Stopped)?;
                return Ok(RunOutcome::Stopped { reason });
            }

            tracing::info!(service = %name, %reason, ?uptime, "service exited");

            let outcome = ProcessOutcome::new(reason, uptime);
            match self.tracker.decide(outcome, None) {
                RestartDecision::Restart { delay, attempt } => {
                    self.transition(ServiceState::Backoff)?;
                    if let Some(reason) =
                        wait_backoff(&name, delay, attempt, &mut sigint, &mut sigterm).await
                    {
                        self.transition(ServiceState::Stopped)?;
                        return Ok(RunOutcome::Stopped { reason });
                    }
                    restarts += 1;
                }
                RestartDecision::Stop => {
                    self.transition(ServiceState::Exited)?;
                    return Ok(RunOutcome::Exited { reason, restarts });
                }
                RestartDecision::Fail { reason: why } => {
                    self.transition(ServiceState::Failed)?;
                    tracing::error!(service = %name, %why, restarts, "giving up");
                    return Err(RuntimeError::RestartLimitExhausted {
                        service: name,
                        attempts: restarts,
                    });
                }
            }
        }
    }

    /// Gracefully stop the running process group, escalating to `SIGKILL` when
    /// the configured timeout elapses or a second signal arrives.
    async fn shutdown(
        &mut self,
        handle: &mut ProcessHandle,
        reason: ShutdownReason,
        sigint: &mut tokio::signal::unix::Signal,
        sigterm: &mut tokio::signal::unix::Signal,
    ) -> Result<ExitReason, RuntimeError> {
        let name = self.service.name.to_string();
        self.transition(ServiceState::Stopping)?;

        let sig = os_signal(self.service.shutdown_signal);
        tracing::info!(
            service = %name,
            %reason,
            signal = sig.as_str(),
            pgid = handle.pgid(),
            timeout = ?self.service.shutdown_timeout,
            "stopping service"
        );
        handle.signal_group(&name, sig)?;

        let timeout = self.service.shutdown_timeout;
        let escalate = tokio::select! {
            result = tokio::time::timeout(timeout, handle.wait(&name)) => {
                match result {
                    Ok(exit) => return exit,
                    Err(_) => {
                        tracing::warn!(
                            service = %name,
                            ?timeout,
                            "shutdown timed out; escalating to SIGKILL"
                        );
                        true
                    }
                }
            }
            _ = sigint.recv() => {
                tracing::warn!(service = %name, "second interrupt; escalating to SIGKILL");
                true
            }
            _ = sigterm.recv() => {
                tracing::warn!(service = %name, "second termination signal; escalating to SIGKILL");
                true
            }
        };

        if escalate {
            handle.kill_group(&name, timeout)?;
        }
        handle.wait(&name).await
    }
}

/// Sleep out a restart delay, returning `Some(reason)` if a shutdown signal
/// interrupted the wait.
async fn wait_backoff(
    service: &str,
    delay: Duration,
    attempt: u32,
    sigint: &mut tokio::signal::unix::Signal,
    sigterm: &mut tokio::signal::unix::Signal,
) -> Option<ShutdownReason> {
    tracing::info!(service = %service, ?delay, attempt, "restarting after backoff");
    tokio::select! {
        _ = tokio::time::sleep(delay) => None,
        _ = sigint.recv() => Some(ShutdownReason::UserInterrupt),
        _ = sigterm.recv() => Some(ShutdownReason::Terminated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_signals_map_to_os_signals() {
        assert_eq!(os_signal(ShutdownSignal::Term), Signal::SIGTERM);
        assert_eq!(os_signal(ShutdownSignal::Int), Signal::SIGINT);
        assert_eq!(os_signal(ShutdownSignal::Quit), Signal::SIGQUIT);
        assert_eq!(os_signal(ShutdownSignal::Hup), Signal::SIGHUP);
    }

    #[test]
    fn missing_executable_is_not_retryable() {
        let err = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(!spawn_error_is_retryable(&err));
        let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(!spawn_error_is_retryable(&err));
        let err = std::io::Error::from(std::io::ErrorKind::WouldBlock);
        assert!(spawn_error_is_retryable(&err));
    }

    #[test]
    fn exit_status_maps_to_exit_reason() {
        assert_eq!(
            exit_reason(std::process::ExitStatus::from_raw(0)),
            ExitReason::Code(0)
        );
        // 0x0100 == exit code 1 in wait(2) encoding.
        assert_eq!(
            exit_reason(std::process::ExitStatus::from_raw(0x0100)),
            ExitReason::Code(1)
        );
        // A raw status of 9 means "terminated by SIGKILL".
        assert_eq!(
            exit_reason(std::process::ExitStatus::from_raw(9)),
            ExitReason::Signal(9)
        );
    }
}
