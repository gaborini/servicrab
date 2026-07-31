//! The daemon itself: a stack supervisor with a Unix socket bolted on.
//!
//! The socket never touches the supervisor directly. It reads a status
//! snapshot that a collector task keeps up to date, and it asks for shutdown
//! through the same channel the signal handler uses — so a slow or hostile
//! client cannot interfere with process supervision.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use servicrab_core::runtime::stack::{Control, ControlTx, StackOptions, StackSupervisor};
use servicrab_core::runtime::{control_channel, shutdown_channel, wait_for_shutdown};
use servicrab_core::{
    event_channel, load, plan_stack, Config, EventKind, EventReceiver, EventSender, LogRouter,
    ServiceName, ShutdownReason, SignalWatcher, StatusRegistry,
};
use servicrab_protocol::{decode, encode, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use super::paths::DaemonPaths;
use super::stopped;
use crate::wire::{to_wire_event, to_wire_status};

/// How many events a slow subscriber may fall behind before it is told that
/// it missed some.  Log lines dominate the stream, so this is generous.
const STREAM_BACKLOG: usize = 4096;

/// How long the daemon waits for the log collector to drain on shutdown.
const COLLECTOR_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Options for the daemon body.
#[derive(Debug, Clone, Default)]
pub struct DaemonOptions {
    /// Never restart services, whatever their configured policy says.
    pub no_restart: bool,
    /// Profiles this daemon supervises.  Kept for the lifetime of the process,
    /// because a reload has to plan the same stack that was started.
    pub profiles: Vec<String>,
}

impl DaemonOptions {
    fn selection(&self) -> servicrab_core::Selection<'_> {
        servicrab_core::Selection {
            services: &[],
            profiles: &self.profiles,
        }
    }
}

/// Everything the socket side of the daemon may touch.
///
/// The supervisor is never reached directly: commands travel the control
/// channel, shutdown travels the same channel the signal handler uses, and
/// status is read from a snapshot the collector task keeps current.
struct Session {
    registry: Arc<Mutex<StatusRegistry>>,
    stop: servicrab_core::runtime::ShutdownTx,
    control: ControlTx,
    /// The services the running configuration knows about; a reload replaces
    /// this list.
    names: Mutex<Vec<ServiceName>>,
    project: String,
    /// Where the configuration was loaded from, so it can be re-read.
    config_path: PathBuf,
    /// The profiles this daemon was started with: a reload has to plan the same
    /// stack, or it would quietly drop or adopt services nobody asked about.
    profiles: Vec<String>,
    /// Where the hand-stopped services are remembered.
    stopped: PathBuf,
    /// Kept so reloads can rebuild the watchers.  It is dropped when the
    /// daemon stops, which is what lets the collector task finish: it ends
    /// when the last event sender is gone.
    events: Mutex<Option<EventSender>>,
    /// Fan-out of the runtime event stream to subscribed clients.
    stream: tokio::sync::broadcast::Sender<servicrab_core::ServiceEvent>,
    /// File watchers, replaced wholesale on reload.
    watchers: Mutex<Vec<JoinHandle<()>>>,
    /// Serializes reloads: two clients must not diff against each other.
    reloading: tokio::sync::Mutex<()>,
}

impl Session {
    /// Remember, or forget, that an operator stopped `service` by hand.
    ///
    /// Only `restart = "unless-stopped"` acts on this, and only when a stack is
    /// started, so a file we could not write costs nothing right now — it is
    /// logged and the command still succeeds.
    fn remember(&self, service: &str, is_stopped: bool) {
        if let Err(problem) = stopped::record(&self.stopped, service, is_stopped) {
            tracing::warn!("{problem}");
        }
    }

    fn known_names(&self) -> Vec<ServiceName> {
        self.names
            .lock()
            .map(|names| names.clone())
            .unwrap_or_default()
    }

    /// Replace the file watchers with ones built from `cfg`.
    fn respawn_watchers(&self, cfg: &Config, plan: &[ServiceName]) {
        let Some(events) = self
            .events
            .lock()
            .ok()
            .and_then(|events| events.as_ref().cloned())
        else {
            // The daemon is shutting down; no point in starting watchers.
            return;
        };
        let fresh = servicrab_core::spawn_watchers(cfg, plan, &self.control, &events);
        let Ok(mut watchers) = self.watchers.lock() else {
            return;
        };
        for watcher in watchers.drain(..) {
            watcher.abort();
        }
        *watchers = fresh;
    }

    /// Stop the watchers and let go of the event sender.
    fn shutdown(&self) {
        if let Ok(mut watchers) = self.watchers.lock() {
            for watcher in watchers.drain(..) {
                watcher.abort();
            }
        }
        if let Ok(mut events) = self.events.lock() {
            *events = None;
        }
    }
}

/// Bind the project socket so that it is unreachable by anyone else from the
/// moment it exists.
///
/// Connecting to a Unix socket needs write permission on the socket file, and
/// a client that can connect can start, stop and restart every service in the
/// project.  `bind` applies the process umask, which is 022 on most systems but
/// 002 on distributions that give each user a private group — there the whole
/// group would be able to drive the daemon.
///
/// Restricting the mode after `bind` is not enough: between the two calls the
/// socket is live and group-writable, and whoever wins that race gets full
/// start/stop/shutdown authority.  The invariant this function keeps is
/// therefore stronger than "ends up 0600": **the socket is never reachable by
/// anyone but its owner, not even for an instant.**  A umask around `bind`
/// is what buys that, because the mode is applied when the inode is created.
///
/// The bind is deliberately synchronous and happens before the async runtime
/// exists: the umask is process-global, so it must not be visible to any other
/// thread creating a file.  `set_permissions` afterwards is not the mechanism
/// but a check, for a platform that might ignore the umask for sockets.
fn bind_socket(socket: &Path) -> Result<std::os::unix::net::UnixListener, String> {
    use nix::sys::stat::{umask, Mode};
    use std::os::unix::fs::PermissionsExt;

    // 0o177 masks away every bit but the owner's read and write.
    let previous = umask(Mode::from_bits_truncate(0o177));
    let bound = std::os::unix::net::UnixListener::bind(socket);
    umask(previous);

    let listener = bound.map_err(|e| format!("could not listen on {}: {e}", socket.display()))?;

    let mode = std::fs::metadata(socket)
        .map(|meta| meta.permissions().mode() & 0o777)
        .unwrap_or(0);
    if mode != 0o600 {
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("could not restrict {} to its owner: {e}", socket.display()))?;
    }

    listener
        .set_nonblocking(true)
        .map_err(|e| format!("could not prepare {}: {e}", socket.display()))?;
    Ok(listener)
}

/// Run the daemon in this process until the stack stops or shutdown is
/// requested. Returns the exit code to use.
pub fn serve(
    cfg: &Config,
    config_path: &Path,
    paths: &DaemonPaths,
    options: DaemonOptions,
) -> Result<i32, String> {
    let plan = plan_stack(cfg, options.selection()).map_err(|e| e.to_string())?;
    let held_back = stopped::held_back(cfg, &plan, &stopped::read(&paths.stopped));
    if !held_back.is_empty() {
        tracing::info!(
            services = %held_back
                .iter()
                .map(ServiceName::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            file = %paths.stopped.display(),
            "leaving hand-stopped services (and their dependents) stopped"
        );
    }

    paths.ensure_dir()?;
    // One daemon per project, decided by the kernel rather than by a check
    // that another start could slip past.  The lock is held for the whole run.
    let lock = match super::lock::ProjectLock::acquire(&paths.pid) {
        Ok(lock) => lock,
        Err(super::lock::LockError::Held) => {
            return Err(format!(
                "a daemon is already running for this project (pidfile: {})",
                paths.pid.display()
            ))
        }
        Err(super::lock::LockError::Failed(problem)) => return Err(problem),
    };
    // Holding the lock rules out every daemon that takes it, so anything still
    // answering on the socket is either a release from before the lock existed
    // or an impostor.  Refusing is the only safe answer to both.
    if super::client::is_running(&paths.socket) {
        return Err(format!(
            "a daemon is already running for this project (socket: {})",
            paths.socket.display()
        ));
    }
    // A socket file always survives its daemon, and the lock proves ours is
    // gone, so a leftover here is stale.
    let _ = std::fs::remove_file(&paths.socket);
    // Bound before the runtime exists: the umask that keeps the socket private
    // is process-global, so no other thread may be creating files while it is
    // in effect.
    let bound = bind_socket(&paths.socket)?;

    let registry = Arc::new(Mutex::new(StatusRegistry::new(plan.iter().map(|name| {
        let has_health = cfg
            .services
            .get(name)
            .is_some_and(|svc| svc.health.is_some());
        (name.clone(), has_health)
    }))));

    let logs = crate::commands::logs::router_for(cfg);
    let project = cfg.project.name.to_string();
    let stack_options = StackOptions {
        no_restart: options.no_restart,
        abort_on_failure: false,
        // An operator may stop every service and start one again later, which
        // is only possible while the supervisor is alive.
        keep_running: true,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start the async runtime: {e}"))?;

    let result = runtime.block_on(async {
        let listener = UnixListener::from_std(bound)
            .map_err(|e| format!("could not listen on {}: {e}", paths.socket.display()))?;

        // One channel drives shutdown, whether it was asked for by a signal or
        // over the socket.
        let (stop_tx, mut stop_rx) = shutdown_channel();
        let signals = SignalWatcher::install(&project).map_err(|e| e.to_string())?;
        let mut signal_rx = signals.subscribe();
        let signal_stop = stop_tx.clone();
        let signal_task = tokio::spawn(async move {
            let reason = wait_for_shutdown(&mut signal_rx).await;
            let _ = signal_stop.send(Some(reason));
        });

        let (events_tx, events_rx) = event_channel();
        let (control_tx, control_rx) = control_channel();
        let (stream_tx, _) = tokio::sync::broadcast::channel(STREAM_BACKLOG);
        let collector = tokio::spawn(collect(
            events_rx,
            Arc::clone(&registry),
            logs,
            stream_tx.clone(),
        ));

        let session = Arc::new(Session {
            registry: Arc::clone(&registry),
            stop: stop_tx.clone(),
            control: control_tx.clone(),
            names: Mutex::new(plan.clone()),
            project: project.clone(),
            config_path: config_path.to_path_buf(),
            profiles: options.profiles.clone(),
            stopped: paths.stopped.clone(),
            events: Mutex::new(Some(events_tx.clone())),
            stream: stream_tx,
            watchers: Mutex::new(Vec::new()),
            reloading: tokio::sync::Mutex::new(()),
        });
        drop(control_tx);

        // Watch-triggered restarts travel the same control channel as the
        // socket's `restart_service`, so the supervisor cannot tell them apart.
        session.respawn_watchers(cfg, &plan);
        let server = tokio::spawn(accept_loop(listener, Arc::clone(&session)));

        // The pidfile is already written and locked; this is the point where a
        // client can reach us.
        tracing::info!(project = %project, socket = %paths.socket.display(), "daemon ready");

        let supervisor = StackSupervisor::new(cfg, plan, stack_options, events_tx)
            .with_control(control_rx)
            .with_stopped(held_back);
        let outcome = supervisor.run(&mut stop_rx).await;

        // Dropping the last event senders is what ends the collector, so the
        // socket side has to let go of its clone first.
        session.shutdown();
        server.abort();
        signal_task.abort();
        // The collector only has queued events left; the timeout is a
        // backstop so a lost sender can never keep the daemon alive.
        let _ = tokio::time::timeout(COLLECTOR_GRACE, collector).await;

        Ok::<_, String>(outcome)
    });

    // Clean up even when the run failed, so the next start is not blocked by
    // our leftovers.  Dropping the lock removes the pidfile, and it goes last:
    // until then no other daemon can get as far as binding the socket.
    let _ = std::fs::remove_file(&paths.socket);
    drop(lock);

    let outcome = result?;
    if !outcome.is_success() {
        return Ok(1);
    }
    Ok(0)
}

/// Keep the status registry current, copy output to the log files, and hand
/// every event to whoever is subscribed.
async fn collect(
    mut events: EventReceiver,
    registry: Arc<Mutex<StatusRegistry>>,
    mut logs: Option<LogRouter>,
    stream: tokio::sync::broadcast::Sender<servicrab_core::ServiceEvent>,
) {
    while let Some(event) = events.recv().await {
        if let (Some(router), EventKind::Log { line, .. }) = (logs.as_mut(), &event.kind) {
            if let Some(problem) = router.record(&event.service, line) {
                tracing::warn!("{problem}");
            }
        }
        if let Ok(mut registry) = registry.lock() {
            registry.apply(&event);
        }
        // Nobody subscribed is the normal case, and not an error.
        let _ = stream.send(event);
    }
}

/// Serve clients until the task is cancelled.
async fn accept_loop(listener: UnixListener, session: Arc<Session>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let session = Arc::clone(&session);
        // One task per client keeps a stuck reader from blocking everybody
        // else.
        tokio::spawn(async move {
            handle_client(stream, session).await;
        });
    }
}

async fn handle_client(stream: UnixStream, session: Arc<Session>) {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let request = match decode::<Request>(&line) {
            Ok(request) => request,
            Err(err) => {
                if !write(
                    &mut write_half,
                    &Response::Error {
                        message: err.to_string(),
                    },
                )
                .await
                {
                    break;
                }
                continue;
            }
        };

        // Subscribing turns the connection one-way: the client stops asking
        // and only reads until it goes away.
        if let Request::Subscribe { services, logs } = request {
            let filter = Filter::new(services, logs);
            let receiver = session.stream.subscribe();
            if write(&mut write_half, &Response::Ok { message: None }).await {
                stream_events(receiver, filter, write_half).await;
            }
            return;
        }

        let response = respond(request, &session).await;

        if !write(&mut write_half, &response).await {
            break;
        }
    }
}

/// Write one response, reporting whether the client is still there.
async fn write(sink: &mut tokio::net::unix::OwnedWriteHalf, response: &Response) -> bool {
    let Ok(payload) = encode(response) else {
        return false;
    };
    if sink.write_all(payload.as_bytes()).await.is_err() {
        return false;
    }
    sink.flush().await.is_ok()
}

/// Which events a subscriber asked for.
struct Filter {
    services: Vec<String>,
    logs: bool,
}

impl Filter {
    fn new(services: Vec<String>, logs: bool) -> Self {
        Self { services, logs }
    }

    fn wants(&self, event: &servicrab_core::ServiceEvent) -> bool {
        if !self.logs && matches!(event.kind, EventKind::Log { .. }) {
            return false;
        }
        self.services.is_empty()
            || self
                .services
                .iter()
                .any(|name| name == event.service.as_str())
    }
}

/// Forward runtime events to one subscribed client until it disconnects.
async fn stream_events(
    mut events: tokio::sync::broadcast::Receiver<servicrab_core::ServiceEvent>,
    filter: Filter,
    mut sink: tokio::net::unix::OwnedWriteHalf,
) {
    loop {
        let response = match events.recv().await {
            Ok(event) => {
                if !filter.wants(&event) {
                    continue;
                }
                Response::Event {
                    service: event.service.to_string(),
                    event: to_wire_event(&event.kind),
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                Response::Lagged { skipped }
            }
            // The collector is gone, so the daemon is shutting down.
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        if !write(&mut sink, &response).await {
            return;
        }
    }
}

async fn respond(request: Request, session: &Session) -> Response {
    match request {
        Request::Ping => Response::Pong {
            project: session.project.clone(),
            pid: std::process::id(),
        },
        Request::Status => {
            let Ok(registry) = session.registry.lock() else {
                return Response::Error {
                    message: "the status registry is poisoned".to_string(),
                };
            };
            Response::Status {
                services: registry.snapshot().iter().map(to_wire_status).collect(),
            }
        }
        Request::Shutdown => {
            let _ = session.stop.send(Some(ShutdownReason::Terminated));
            Response::Ok {
                message: Some("stopping the stack".to_string()),
            }
        }
        Request::StartService { name } => {
            let response = command(session, &name, |service, ack| Control::Start {
                service,
                ack,
            })
            .await;
            // Starting a service is how an operator takes back a stop, whether
            // it happened in this daemon or in an earlier one.
            if matches!(response, Response::Ok { .. }) {
                session.remember(&name, false);
            }
            response
        }
        Request::StopService { name } => {
            let response = command(session, &name, |service, ack| Control::Stop {
                service,
                ack,
            })
            .await;
            if matches!(response, Response::Ok { .. }) {
                session.remember(&name, true);
            }
            response
        }
        Request::RestartService { name } => {
            let response = command(session, &name, |service, ack| Control::Restart {
                service,
                ack,
            })
            .await;
            if matches!(response, Response::Ok { .. }) {
                session.remember(&name, false);
            }
            response
        }
        Request::Reload => reload(session).await,
        // `Request` is `#[non_exhaustive]`, so an older daemon can still be
        // asked something it does not know about.
        _ => Response::Error {
            message: "this daemon does not support that request".to_string(),
        },
    }
}

/// Re-read the configuration and apply the difference to the running stack.
///
/// Only services change: project-level settings such as `[project.logs]` are
/// bound to the process and need a daemon restart.
async fn reload(session: &Session) -> Response {
    // Two clients reloading at once would each diff against a stack the other
    // one is still changing.
    let _guard = session.reloading.lock().await;

    let (cfg, warnings) = match load(&session.config_path) {
        Ok(loaded) => loaded,
        Err(errors) => {
            let details: Vec<String> = errors.iter().map(|e| format!("  • {e}")).collect();
            return Response::Error {
                message: format!(
                    "{} has {} error(s); the stack was left untouched:\n{}",
                    session.config_path.display(),
                    errors.len(),
                    details.join("\n")
                ),
            };
        }
    };
    for warning in &warnings {
        tracing::warn!("{warning}");
    }

    let selection = servicrab_core::Selection {
        services: &[],
        profiles: &session.profiles,
    };
    let plan = match plan_stack(&cfg, selection) {
        Ok(plan) => plan,
        Err(err) => {
            return Response::Error {
                message: format!("{err}; the stack was left untouched"),
            }
        }
    };

    // The registry is updated first so that events from services the reload
    // adds are not dropped for lack of an entry.
    let previous = session.known_names();
    sync_registry(session, &cfg, &plan);

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    let command = Control::Reload {
        config: Box::new(cfg.clone()),
        plan: plan.clone(),
        ack: ack_tx,
    };
    if session.control.send(command).is_err() {
        restore_registry(session, &cfg, &previous);
        return Response::Error {
            message: "the supervisor is no longer accepting commands".to_string(),
        };
    }

    match ack_rx.await {
        Ok(Ok(message)) => {
            session.respawn_watchers(&cfg, &plan);
            Response::Ok {
                message: Some(format!("reloaded {}: {message}", session.project)),
            }
        }
        Ok(Err(message)) => {
            restore_registry(session, &cfg, &previous);
            Response::Error { message }
        }
        Err(_) => {
            restore_registry(session, &cfg, &previous);
            Response::Error {
                message: "the stack stopped before the reload completed".to_string(),
            }
        }
    }
}

/// Point the registry and the known-name list at a new plan.
fn sync_registry(session: &Session, cfg: &Config, plan: &[ServiceName]) {
    if let Ok(mut registry) = session.registry.lock() {
        registry.sync(plan.iter().map(|name| {
            let has_health = cfg
                .services
                .get(name)
                .is_some_and(|svc| svc.health.is_some());
            (name.clone(), has_health)
        }));
    }
    if let Ok(mut names) = session.names.lock() {
        *names = plan.to_vec();
    }
}

/// Undo [`sync_registry`] after a reload the supervisor refused.
fn restore_registry(session: &Session, cfg: &Config, previous: &[ServiceName]) {
    sync_registry(session, cfg, previous);
}

/// Send one per-service command to the supervisor and wait for its verdict.
async fn command(
    session: &Session,
    name: &str,
    build: impl FnOnce(ServiceName, servicrab_core::runtime::stack::Ack) -> Control,
) -> Response {
    let names = session.known_names();
    let Some(service) = names.iter().find(|known| known.as_str() == name) else {
        return Response::Error {
            message: format!(
                "unknown service {name:?}; this daemon supervises: {}",
                names
                    .iter()
                    .map(|n| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    };

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    if session
        .control
        .send(build(service.clone(), ack_tx))
        .is_err()
    {
        return Response::Error {
            message: "the supervisor is no longer accepting commands".to_string(),
        };
    }

    match ack_rx.await {
        Ok(Ok(message)) => Response::Ok {
            message: Some(format!("{name} {message}")),
        },
        Ok(Err(message)) => Response::Error { message },
        // The ack channel is dropped when the stack shuts down mid-command.
        Err(_) => Response::Error {
            message: "the stack stopped before the command completed".to_string(),
        },
    }
}
