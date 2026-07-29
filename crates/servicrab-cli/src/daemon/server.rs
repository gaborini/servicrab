//! The daemon itself: a stack supervisor with a Unix socket bolted on.
//!
//! The socket never touches the supervisor directly. It reads a status
//! snapshot that a collector task keeps up to date, and it asks for shutdown
//! through the same channel the signal handler uses — so a slow or hostile
//! client cannot interfere with process supervision.

use std::path::Path;
use std::sync::{Arc, Mutex};

use servicrab_core::runtime::stack::{StackOptions, StackSupervisor};
use servicrab_core::runtime::{shutdown_channel, wait_for_shutdown};
use servicrab_core::{
    event_channel, plan_stack, Config, EventKind, EventReceiver, LogRouter, ServiceState,
    ShutdownReason, SignalWatcher, StatusRegistry,
};
use servicrab_protocol::{decode, encode, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::paths::DaemonPaths;

/// Options for the daemon body.
#[derive(Debug, Clone, Copy, Default)]
pub struct DaemonOptions {
    /// Never restart services, whatever their configured policy says.
    pub no_restart: bool,
}

/// Run the daemon in this process until the stack stops or shutdown is
/// requested. Returns the exit code to use.
pub fn serve(cfg: &Config, paths: &DaemonPaths, options: DaemonOptions) -> Result<i32, String> {
    let plan = plan_stack(cfg, &[]).map_err(|e| e.to_string())?;
    if plan.is_empty() {
        return Err(
            "no services to start: none of the configured services have autostart = true"
                .to_string(),
        );
    }

    paths.ensure_dir()?;
    // A socket file always survives its daemon, so a stale one is only an
    // error if somebody is still listening on it.
    if super::client::is_running(&paths.socket) {
        return Err(format!(
            "a daemon is already running for this project (socket: {})",
            paths.socket.display()
        ));
    }
    let _ = std::fs::remove_file(&paths.socket);

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
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start the async runtime: {e}"))?;

    let result = runtime.block_on(async {
        let listener = UnixListener::bind(&paths.socket)
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
        let collector = tokio::spawn(collect(events_rx, Arc::clone(&registry), logs));
        let server = tokio::spawn(accept_loop(
            listener,
            Arc::clone(&registry),
            stop_tx.clone(),
            project.clone(),
        ));

        write_pid(&paths.pid)?;
        tracing::info!(project = %project, socket = %paths.socket.display(), "daemon ready");

        let supervisor = StackSupervisor::new(cfg, plan, stack_options, events_tx);
        let outcome = supervisor.run(&mut stop_rx).await;

        server.abort();
        signal_task.abort();
        let _ = collector.await;

        Ok::<_, String>(outcome)
    });

    // Clean up even when the run failed, so the next start is not blocked by
    // our leftovers.
    let _ = std::fs::remove_file(&paths.socket);
    let _ = std::fs::remove_file(&paths.pid);

    let outcome = result?;
    if !outcome.is_success() {
        return Ok(1);
    }
    Ok(0)
}

/// Keep the status registry current and copy output to the log files.
async fn collect(
    mut events: EventReceiver,
    registry: Arc<Mutex<StatusRegistry>>,
    mut logs: Option<LogRouter>,
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
    }
}

/// Serve clients until the task is cancelled.
async fn accept_loop(
    listener: UnixListener,
    registry: Arc<Mutex<StatusRegistry>>,
    stop: servicrab_core::runtime::ShutdownTx,
    project: String,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let registry = Arc::clone(&registry);
        let stop = stop.clone();
        let project = project.clone();
        // One task per client keeps a stuck reader from blocking everybody
        // else.
        tokio::spawn(async move {
            handle_client(stream, registry, stop, project).await;
        });
    }
}

async fn handle_client(
    stream: UnixStream,
    registry: Arc<Mutex<StatusRegistry>>,
    stop: servicrab_core::runtime::ShutdownTx,
    project: String,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let response = match decode::<Request>(&line) {
            Ok(request) => respond(request, &registry, &stop, &project),
            Err(err) => Response::Error {
                message: err.to_string(),
            },
        };

        let Ok(payload) = encode(&response) else {
            break;
        };
        if write_half.write_all(payload.as_bytes()).await.is_err() {
            break;
        }
        let _ = write_half.flush().await;
    }
}

fn respond(
    request: Request,
    registry: &Arc<Mutex<StatusRegistry>>,
    stop: &servicrab_core::runtime::ShutdownTx,
    project: &str,
) -> Response {
    match request {
        Request::Ping => Response::Pong {
            project: project.to_string(),
            pid: std::process::id(),
        },
        Request::Status => {
            let Ok(registry) = registry.lock() else {
                return Response::Error {
                    message: "the status registry is poisoned".to_string(),
                };
            };
            Response::Status {
                services: registry.snapshot().iter().map(to_wire).collect(),
            }
        }
        Request::Shutdown => {
            let _ = stop.send(Some(ShutdownReason::Terminated));
            Response::Ok {
                message: Some("stopping the stack".to_string()),
            }
        }
        // `Request` is `#[non_exhaustive]`, so an older daemon can still be
        // asked something it does not know about.
        _ => Response::Error {
            message: "this daemon does not support that request".to_string(),
        },
    }
}

/// Convert a runtime status into its wire representation.
fn to_wire(status: &servicrab_core::ServiceStatus) -> servicrab_protocol::ServiceInfo {
    use servicrab_protocol::ServiceState as Wire;

    servicrab_protocol::ServiceInfo {
        name: status.name.to_string(),
        state: match status.state {
            ServiceState::Pending => Wire::Pending,
            ServiceState::Starting => Wire::Starting,
            ServiceState::Running => Wire::Running,
            ServiceState::Backoff => Wire::Backoff,
            ServiceState::Stopping => Wire::Stopping,
            ServiceState::Stopped => Wire::Stopped,
            ServiceState::Exited => Wire::Exited,
            ServiceState::Failed => Wire::Failed,
        },
        pid: status.pid,
        uptime_secs: status.uptime.map(|d| d.as_secs()),
        restarts: status.restarts,
        health: match status.health {
            servicrab_core::Health::None => servicrab_protocol::Health::None,
            servicrab_core::Health::Starting => servicrab_protocol::Health::Starting,
            servicrab_core::Health::Healthy => servicrab_protocol::Health::Healthy,
            servicrab_core::Health::Unhealthy => servicrab_protocol::Health::Unhealthy,
        },
        message: status.message.clone(),
    }
}

fn write_pid(path: &Path) -> Result<(), String> {
    std::fs::write(path, format!("{}\n", std::process::id()))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}
