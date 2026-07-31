//! Unix implementation of the process runner.
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
//! so every signal is delivered with `killpg(2)`, and [`ProcessHandle`] sweeps
//! its group with `SIGKILL` when it is dropped without having been swept — an
//! early return or an aborted supervision task must not orphan a grandchild
//! either.  `kill_on_drop(true)` alone would only reach the direct child.
//!
//! [`ServiceRunner`] supervises exactly one service and is driven entirely by a
//! shutdown channel, which makes it reusable both for the single-service `run`
//! command (see [`ForegroundRunner`]) and for the multi-service stack
//! supervisor in [`crate::runtime::stack`].

use std::process::Stdio;
use std::time::{Duration, Instant};

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::{RestartPolicy, Service, ShutdownSignal};
use crate::error::RuntimeError;
use crate::lifecycle::{
    ExitReason, ProcessOutcome, RestartDecision, RestartTracker, ServiceState, ShutdownReason,
    StateMachine,
};
use crate::runtime::event::{EventKind, EventSink, Stream};
use crate::runtime::health::{HealthMonitor, HealthSignal};
use crate::runtime::{
    shutdown_channel, wait_for_shutdown, OutputMode, RunOptions, RunOutcome, ShutdownRx, ShutdownTx,
};

/// How long to wait for the output readers to drain after the child exited.
const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

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
///
/// Dropping the handle sweeps the group with `SIGKILL` unless it has already
/// been swept, so an early return — a failed `wait`, a rejected state
/// transition, a cancelled supervision task — cannot leave a grandchild
/// behind.  `kill_on_drop(true)` alone would only reach the direct child.
#[derive(Debug)]
pub struct ProcessHandle {
    child: Child,
    pgid: Pid,
    started_at: Instant,
    /// Set once the group has been swept.  After that the leader has been
    /// reaped and the kernel may hand its id to somebody else, so signalling
    /// it again would be signalling a stranger.
    swept: bool,
}

impl ProcessHandle {
    /// Spawn the service's configured executable.
    ///
    /// The executable and its arguments are passed verbatim — no shell is
    /// involved unless the user configured a shell as the executable.
    pub fn spawn(service: &Service, output: OutputMode) -> Result<Self, RuntimeError> {
        let (stdout, stderr) = match output {
            OutputMode::Inherit => (Stdio::inherit(), Stdio::inherit()),
            OutputMode::Capture => (Stdio::piped(), Stdio::piped()),
        };

        let mut command = Command::new(&service.executable);
        command
            .args(&service.args)
            .current_dir(&service.cwd)
            .env_clear()
            .envs(&service.env)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
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
            swept: false,
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
            Ok(()) => Ok(()),
            Err(errno) if group_is_gone(errno) => Ok(()),
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
    fn kill_group(&mut self, service: &str, timeout: Duration) -> Result<(), RuntimeError> {
        self.swept = true;
        match killpg(self.pgid, Signal::SIGKILL) {
            Ok(()) => Ok(()),
            Err(errno) if group_is_gone(errno) => {
                tracing::debug!(
                    service = %service,
                    pgid = self.pgid.as_raw(),
                    %errno,
                    "the process group was already gone"
                );
                Ok(())
            }
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

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // Invariant: no process group of ours outlives its handle.  The normal
        // path sweeps the group explicitly and sets `swept`; this covers every
        // other way the handle can go away — a `?` on a failed `wait` or a
        // rejected state transition, or the supervision task being aborted.
        if self.swept {
            return;
        }
        if let Err(errno) = killpg(self.pgid, Signal::SIGKILL) {
            if !group_is_gone(errno) {
                tracing::warn!(
                    pgid = self.pgid.as_raw(),
                    %errno,
                    "could not sweep the process group while dropping its handle"
                );
            }
        }
    }
}

fn exit_reason(status: std::process::ExitStatus) -> ExitReason {
    use std::os::unix::process::ExitStatusExt;
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
    /// A shutdown was requested.
    Signalled(ShutdownReason),
    /// The service's health check gave up on it.
    Unhealthy,
}

/// Wait for the health monitor to declare the service unhealthy.
///
/// Pends forever when the service has no health check, when failures are only
/// reported (`on_unhealthy = "ignore"`), or once the monitor is gone: in those
/// cases the process is only ever stopped by an exit or a shutdown request.
async fn next_unhealthy(health: &mut Option<mpsc::UnboundedReceiver<HealthSignal>>, act: bool) {
    match health {
        Some(rx) if act => loop {
            match rx.recv().await {
                Some(signal) if signal.is_unhealthy() => return,
                Some(_) => continue,
                None => std::future::pending().await,
            }
        },
        _ => std::future::pending().await,
    }
}

/// SIGINT/SIGTERM handling for the current process.
///
/// The watcher forwards every signal it sees into a shutdown channel so that
/// one or many [`ServiceRunner`]s can react to it.  A second signal is
/// forwarded as well, which the runners interpret as "stop waiting, kill now".
#[derive(Debug)]
pub struct SignalWatcher {
    tx: ShutdownTx,
    task: JoinHandle<()>,
}

impl SignalWatcher {
    /// Install SIGINT and SIGTERM handlers.
    ///
    /// `label` only gives signal-registration errors a useful subject: a
    /// service name for `run`, the project name for `up`.
    pub fn install(label: &str) -> Result<Self, RuntimeError> {
        let mut sigint = signal(SignalKind::interrupt()).map_err(|source| {
            RuntimeError::SignalRegistrationFailed {
                service: label.to_string(),
                signal: "SIGINT",
                source,
            }
        })?;
        let mut sigterm = signal(SignalKind::terminate()).map_err(|source| {
            RuntimeError::SignalRegistrationFailed {
                service: label.to_string(),
                signal: "SIGTERM",
                source,
            }
        })?;

        let (tx, _rx) = shutdown_channel();
        let sender = tx.clone();
        let task = tokio::spawn(async move {
            loop {
                let reason = tokio::select! {
                    _ = sigint.recv() => ShutdownReason::UserInterrupt,
                    _ = sigterm.recv() => ShutdownReason::Terminated,
                };
                // `send` fails only when every receiver is gone, in which case
                // there is nothing left to shut down.
                if sender.send(Some(reason)).is_err() {
                    return;
                }
            }
        });

        Ok(Self { tx, task })
    }

    /// A fresh receiver for the shutdown channel.
    pub fn subscribe(&self) -> ShutdownRx {
        self.tx.subscribe()
    }

    /// A clone of the sender, for code that requests a shutdown itself (for
    /// example when a service fails and `--abort-on-failure` is in effect).
    pub fn sender(&self) -> ShutdownTx {
        self.tx.clone()
    }
}

impl Drop for SignalWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Supervises a single service process: spawning, restarting, and shutting it
/// down.
///
/// The runner owns no signal handlers of its own; it reacts to a shutdown
/// channel so that many runners can be driven together.
pub struct ServiceRunner<'a> {
    service: &'a Service,
    tracker: RestartTracker,
    state: StateMachine,
    output: OutputMode,
    events: EventSink,
}

impl<'a> ServiceRunner<'a> {
    /// Build a runner for a validated service.
    pub fn new(service: &'a Service, options: RunOptions) -> Self {
        let policy = options.effective_policy(service.restart);
        Self {
            service,
            tracker: RestartTracker::from_service(service).with_policy(policy),
            state: StateMachine::new(),
            output: options.output,
            events: EventSink::none(),
        }
    }

    /// Publish lifecycle events and captured output to `events`.
    pub fn with_events(mut self, events: EventSink) -> Self {
        self.events = events;
        self
    }

    /// The effective restart policy after `--no-restart` is taken into
    /// account.
    pub fn policy(&self) -> RestartPolicy {
        self.tracker.policy()
    }

    fn emit(&self, kind: EventKind) {
        self.events.emit(&self.service.name, kind);
    }

    fn transition(&mut self, next: ServiceState) -> Result<(), RuntimeError> {
        self.state
            .try_transition(next)
            .map_err(|source| RuntimeError::InvalidTransition {
                service: self.service.name.to_string(),
                source,
            })?;
        tracing::debug!(service = %self.service.name, state = %next, "state transition");
        self.emit(EventKind::State(next));
        Ok(())
    }

    /// Attach line readers to the child's piped output, if capturing.
    fn spawn_readers(&self, handle: &mut ProcessHandle) -> Vec<JoinHandle<()>> {
        if !matches!(self.output, OutputMode::Capture) {
            return Vec::new();
        }

        let mut readers = Vec::new();
        if let Some(stdout) = handle.child.stdout.take() {
            readers.push(self.spawn_reader(stdout, Stream::Stdout));
        }
        if let Some(stderr) = handle.child.stderr.take() {
            readers.push(self.spawn_reader(stderr, Stream::Stderr));
        }
        readers
    }

    fn spawn_reader<R>(&self, pipe: R, stream: Stream) -> JoinHandle<()>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let events = self.events.clone();
        let name = self.service.name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(pipe).lines();
            // Invalid UTF-8 ends the reader rather than the service; the
            // process keeps running and its exit status is still reported.
            while let Ok(Some(line)) = lines.next_line().await {
                events.emit(&name, EventKind::Log { stream, line });
            }
        })
    }

    /// Wait for the output readers to finish, but never block shutdown on a
    /// descendant that inherited the pipe and refuses to close it.
    async fn drain_readers(readers: Vec<JoinHandle<()>>) {
        for reader in readers {
            if tokio::time::timeout(READER_DRAIN_TIMEOUT, reader)
                .await
                .is_err()
            {
                // The timeout dropped the JoinHandle, which detaches the task;
                // it ends as soon as the pipe closes.
                tracing::debug!("output reader did not finish within the drain timeout");
            }
        }
    }

    /// Run the service until it stops for good.
    ///
    /// The returned [`RunOutcome`] describes how the run finished; mapping that
    /// to a process exit code is the caller's job.
    pub async fn run(&mut self, shutdown: &mut ShutdownRx) -> Result<RunOutcome, RuntimeError> {
        let name = self.service.name.to_string();
        let mut restarts = 0u32;

        loop {
            // A shutdown that arrived before (or during) the backoff must not
            // be overtaken by a new process.
            if let Some(reason) = *shutdown.borrow_and_update() {
                self.transition(ServiceState::Stopped)?;
                return Ok(RunOutcome::Stopped { reason });
            }

            self.transition(ServiceState::Starting)?;
            tracing::info!(
                service = %name,
                executable = %self.service.executable,
                cwd = %self.service.cwd.display(),
                "starting service"
            );

            let mut handle = match ProcessHandle::spawn(self.service, self.output) {
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
                            self.emit(EventKind::Backoff { delay, attempt });
                            if let Some(reason) = self.wait_backoff(delay, attempt, shutdown).await
                            {
                                self.transition(ServiceState::Stopped)?;
                                return Ok(RunOutcome::Stopped { reason });
                            }
                            restarts += 1;
                            continue;
                        }
                        RestartDecision::Stop | RestartDecision::Fail { .. } => {
                            let _ = self.transition(ServiceState::Failed);
                            self.emit(EventKind::Failed {
                                message: err.to_string(),
                            });
                            return Err(err);
                        }
                    }
                }
            };

            let readers = self.spawn_readers(&mut handle);

            // The monitor lives exactly as long as this process run: dropping
            // the receiver at the end of the iteration stops the probing task.
            let mut health = HealthMonitor::for_service(self.service, self.events.clone())
                .map(HealthMonitor::spawn);
            let act_on_unhealthy = self
                .service
                .health
                .as_ref()
                .is_some_and(crate::runtime::health::stops_process);

            self.transition(ServiceState::Running)?;
            self.emit(EventKind::Started {
                pgid: handle.pgid(),
            });
            tracing::info!(service = %name, pid = handle.pgid(), "service running");

            // The select! branches cannot borrow `handle` twice, so the wake-up
            // reason is captured first and shutdown is driven afterwards.
            let wake = tokio::select! {
                result = handle.wait(&name) => Wake::Exited(result?),
                reason = wait_for_shutdown(shutdown) => Wake::Signalled(reason),
                () = next_unhealthy(&mut health, act_on_unhealthy) => Wake::Unhealthy,
            };

            let (reason, stopped_by) = match wake {
                Wake::Exited(reason) => (reason, None),
                Wake::Signalled(why) => {
                    self.emit(EventKind::Stopping { reason: why });
                    let reason = self.shutdown(&mut handle, why, shutdown).await?;
                    (reason, Some(why))
                }
                // An unhealthy service is stopped like any other, but the
                // outcome is fed to the restart policy instead of ending the
                // supervision loop.
                Wake::Unhealthy => {
                    let why = ShutdownReason::Unhealthy;
                    self.emit(EventKind::Stopping { reason: why });
                    self.shutdown(&mut handle, why, shutdown).await?;
                    (ExitReason::Unhealthy, None)
                }
            };

            let uptime = handle.uptime();

            // Sweep the group so no descendant outlives the direct child.
            handle.kill_group(&name, self.service.shutdown_timeout)?;
            Self::drain_readers(readers).await;

            if let Some(why) = stopped_by {
                tracing::info!(service = %name, reason = %why, "service stopped");
                self.transition(ServiceState::Stopped)?;
                self.emit(EventKind::Finished {
                    summary: format!("stopped ({why}), last status: {reason}"),
                });
                return Ok(RunOutcome::Stopped { reason: why });
            }

            tracing::info!(service = %name, %reason, ?uptime, "service exited");
            self.emit(EventKind::Exited { reason, uptime });

            let outcome = ProcessOutcome::new(reason, uptime);
            match self.tracker.decide(outcome, None) {
                RestartDecision::Restart { delay, attempt } => {
                    self.transition(ServiceState::Backoff)?;
                    self.emit(EventKind::Backoff { delay, attempt });
                    if let Some(why) = self.wait_backoff(delay, attempt, shutdown).await {
                        self.transition(ServiceState::Stopped)?;
                        self.emit(EventKind::Finished {
                            summary: format!("stopped ({why})"),
                        });
                        return Ok(RunOutcome::Stopped { reason: why });
                    }
                    restarts += 1;
                }
                RestartDecision::Stop => {
                    self.transition(ServiceState::Exited)?;
                    self.emit(EventKind::Finished {
                        summary: reason.to_string(),
                    });
                    return Ok(RunOutcome::Exited { reason, restarts });
                }
                RestartDecision::Fail { reason: why } => {
                    self.transition(ServiceState::Failed)?;
                    tracing::error!(service = %name, %why, restarts, "giving up");
                    let err = RuntimeError::RestartLimitExhausted {
                        service: name,
                        attempts: restarts,
                    };
                    self.emit(EventKind::Failed {
                        message: err.to_string(),
                    });
                    return Err(err);
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
        shutdown: &mut ShutdownRx,
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
            // Any further shutdown request while we are already stopping means
            // "do not wait any longer".
            _ = shutdown.changed() => {
                tracing::warn!(service = %name, "second shutdown request; escalating to SIGKILL");
                true
            }
        };

        if escalate {
            handle.kill_group(&name, timeout)?;
        }
        handle.wait(&name).await
    }

    /// Sleep out a restart delay, returning `Some(reason)` if a shutdown was
    /// requested during the wait.
    async fn wait_backoff(
        &self,
        delay: Duration,
        attempt: u32,
        shutdown: &mut ShutdownRx,
    ) -> Option<ShutdownReason> {
        tracing::info!(service = %self.service.name, ?delay, attempt, "restarting after backoff");
        tokio::select! {
            _ = tokio::time::sleep(delay) => None,
            reason = wait_for_shutdown(shutdown) => Some(reason),
        }
    }
}

/// Runs a single configured service in the foreground, installing its own
/// SIGINT/SIGTERM handlers.
///
/// This is the engine behind `servicrab run`.
pub struct ForegroundRunner<'a> {
    runner: ServiceRunner<'a>,
    label: String,
}

impl<'a> ForegroundRunner<'a> {
    /// Build a runner for a validated service.
    pub fn new(service: &'a Service, options: RunOptions) -> Self {
        Self {
            label: service.name.to_string(),
            runner: ServiceRunner::new(service, options),
        }
    }

    /// Publish lifecycle events and captured output to `events`.
    pub fn with_events(mut self, events: EventSink) -> Self {
        self.runner = self.runner.with_events(events);
        self
    }

    /// The effective restart policy after `--no-restart` is taken into account.
    pub fn policy(&self) -> RestartPolicy {
        self.runner.policy()
    }

    /// Run the service until it stops for good.
    pub async fn run(&mut self) -> Result<RunOutcome, RuntimeError> {
        let signals = SignalWatcher::install(&self.label)?;
        let mut shutdown = signals.subscribe();
        self.runner.run(&mut shutdown).await
    }
}

/// Whether a `killpg(2)` error means "there is nothing of ours left to
/// signal".
///
/// `ESRCH` is the documented answer for an empty group.  macOS also answers
/// `EPERM` once the group holds nothing we own — for example when the leader
/// has been reaped and only unrelated processes could still claim the id.
/// Either way there is no process of ours left to kill, and failing the run
/// over it would turn a successful shutdown into a spurious error.
pub(crate) fn group_is_gone(errno: nix::errno::Errno) -> bool {
    matches!(errno, nix::errno::Errno::ESRCH | nix::errno::Errno::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A validated service running `body` through `/bin/sh`.
    fn sh_service(name: &str, body: &str) -> Service {
        let toml = format!(
            "version = 1\n[project]\nname = \"probe\"\n\
             [services.{name}]\ncommand = [\"/bin/sh\", \"-c\", \"{body}\"]\n"
        );
        let raw: crate::raw::RawConfig = toml::from_str(&toml).expect("valid test toml");
        let cfg = crate::validation::validate_raw(raw, std::path::Path::new("/tmp/servicrab.toml"))
            .expect("valid test config")
            .0;
        cfg.services.values().next().cloned().expect("one service")
    }

    fn is_alive(pid: i32) -> bool {
        nix::sys::signal::kill(Pid::from_raw(pid), None).is_ok()
    }

    /// Wait, with an upper bound, for `pid` to disappear.
    fn wait_until_gone(pid: i32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !is_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Read the grandchild's pid the fixture wrote, with an upper bound.
    fn wait_for_pid(path: &std::path::Path) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(pid) = text.trim().parse() {
                    return pid;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the fixture never recorded its grandchild");
    }

    #[tokio::test]
    async fn dropping_a_handle_sweeps_the_whole_process_group() {
        // Every `?` in `ServiceRunner::run` returns while the handle still owns
        // a live group, and so does an aborted supervision task.
        // `kill_on_drop(true)` would only reach the direct child, orphaning a
        // grandchild that ignores the shutdown signal.
        let dir = tempfile::TempDir::new().unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        let service = sh_service(
            "tree",
            &format!(
                "trap '' TERM INT; sleep 30 & echo $! > {}; wait",
                pidfile.display()
            ),
        );

        let handle = ProcessHandle::spawn(&service, OutputMode::Capture).expect("spawned");
        let leader = handle.pgid();
        let grandchild = wait_for_pid(&pidfile);
        assert!(is_alive(grandchild));

        drop(handle);

        assert!(
            wait_until_gone(grandchild),
            "grandchild {grandchild} of group {leader} survived the handle"
        );
    }

    #[tokio::test]
    async fn a_swept_group_is_not_signalled_again_on_drop() {
        // Once the leader has been reaped the kernel may hand its group id to
        // somebody else, so the drop guard has to stay out of the way.
        let service = sh_service("quick", "exit 0");
        let mut handle = ProcessHandle::spawn(&service, OutputMode::Capture).expect("spawned");
        handle.wait("quick").await.expect("reaped");
        handle
            .kill_group("quick", Duration::from_secs(1))
            .expect("sweeping an empty group is fine");
        assert!(handle.swept);
    }

    #[test]
    fn a_vanished_group_is_not_an_error() {
        assert!(group_is_gone(nix::errno::Errno::ESRCH));
        // macOS reports a group we can no longer signal this way.
        assert!(group_is_gone(nix::errno::Errno::EPERM));
        // Anything else is a real failure worth reporting.
        assert!(!group_is_gone(nix::errno::Errno::EINVAL));
    }

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
        use std::os::unix::process::ExitStatusExt;
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
