//! Health and readiness probing.
//!
//! A service may declare a `[health]` table; when it does, the runtime runs
//! the configured probe on an interval for as long as the process is alive.
//! The first successful probe makes the service *ready* — which is what the
//! stack supervisor gates dependents on — and a run of consecutive failures
//! makes it *unhealthy*, which optionally stops the process so that the normal
//! restart policy applies.
//!
//! Probes are deliberately dependency-free: the HTTP probe speaks just enough
//! HTTP/1.1 to issue a `GET` and read the status line.  Anything more involved
//! (TLS, redirects, authentication) belongs in a `command` probe.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::config::{HealthCheck, HealthProbe, Service, ServiceName, UnhealthyAction};
use crate::runtime::event::{EventKind, EventSink};

/// What a health monitor reports back to the service runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthSignal {
    /// The service passed a probe and is ready.
    Ready,
    /// The service exhausted its retry budget.
    Unhealthy {
        /// Why the last probe failed.
        message: String,
    },
}

impl HealthSignal {
    /// Whether this signal reports an unhealthy service.
    pub fn is_unhealthy(&self) -> bool {
        matches!(self, HealthSignal::Unhealthy { .. })
    }
}

/// Environment needed to run a probe, cloned so the monitor can outlive the
/// borrow of the [`Service`].
#[derive(Debug, Clone)]
struct ProbeContext {
    cwd: PathBuf,
    env: BTreeMap<String, String>,
}

/// Run a single probe, returning `Err(message)` when it fails.
async fn probe_once(
    probe: &HealthProbe,
    ctx: &ProbeContext,
    timeout: Duration,
) -> Result<(), String> {
    let attempt = async {
        match probe {
            HealthProbe::Command { executable, args } => run_command_probe(executable, args, ctx)
                .await
                .map_err(|e| e.to_string()),
            HealthProbe::Http {
                host, port, path, ..
            } => run_http_probe(host, *port, path).await,
            HealthProbe::Tcp { host, port } => run_tcp_probe(host, *port).await,
        }
    };

    match tokio::time::timeout(timeout, attempt).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "probe timed out after {}",
            format_duration(timeout)
        )),
    }
}

/// Run a command probe: exit status `0` means healthy.
async fn run_command_probe(
    executable: &str,
    args: &[String],
    ctx: &ProbeContext,
) -> Result<(), String> {
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .current_dir(&ctx.cwd)
        .env_clear()
        .envs(&ctx.env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let output = command
        .output()
        .await
        .map_err(|e| format!("failed to run {executable:?}: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.lines().next_back().unwrap_or("").trim();
    let status = match output.status.code() {
        Some(code) => format!("exited with code {code}"),
        None => "terminated by a signal".to_string(),
    };
    Err(if detail.is_empty() {
        format!("probe command {status}")
    } else {
        format!("probe command {status}: {detail}")
    })
}

/// Issue a bare `HTTP/1.1` `GET` and accept any `2xx`/`3xx` status.
async fn run_http_probe(host: &str, port: u16, path: &str) -> Result<(), String> {
    let mut stream = connect(host, port).await?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: servicrab\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("failed to send the request: {e}"))?;

    let mut status_line = String::new();
    BufReader::new(stream)
        .read_line(&mut status_line)
        .await
        .map_err(|e| format!("failed to read the response: {e}"))?;

    let status_line = status_line.trim_end();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("malformed status line {status_line:?}"))?;

    if (200..400).contains(&code) {
        Ok(())
    } else {
        Err(format!("unhealthy HTTP status {code}"))
    }
}

/// Succeed as soon as a TCP connection can be established.
async fn run_tcp_probe(host: &str, port: u16) -> Result<(), String> {
    connect(host, port).await.map(|_| ())
}

async fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    // A bare IPv6 literal has to be bracketed before it can be resolved.
    let target = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    TcpStream::connect(&target)
        .await
        .map_err(|e| format!("failed to connect to {target}: {e}"))
}

/// Supervise the health of one running process.
///
/// The loop ends when the returned sender is dropped (the process stopped) or
/// once it has reported [`HealthSignal::Unhealthy`].
pub struct HealthMonitor {
    check: HealthCheck,
    ctx: ProbeContext,
    name: ServiceName,
    events: EventSink,
}

impl HealthMonitor {
    /// Build a monitor for `service`, or `None` when it declares no health
    /// check.
    pub fn for_service(service: &Service, events: EventSink) -> Option<Self> {
        let check = service.health.clone()?;
        Some(Self {
            check,
            ctx: ProbeContext {
                cwd: service.cwd.clone(),
                env: service.env.clone(),
            },
            name: service.name.clone(),
            events,
        })
    }

    /// Probe until the monitor is dropped, reporting transitions on the
    /// returned channel.
    pub fn spawn(self) -> mpsc::UnboundedReceiver<HealthSignal> {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move { self.run(tx).await });
        rx
    }

    async fn run(self, tx: mpsc::UnboundedSender<HealthSignal>) {
        let name = self.name.clone();
        let started = Instant::now();
        let mut ticker = tokio::time::interval(self.check.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The first tick completes immediately, which is what we want: probe
        // as soon as the process is up so readiness is not delayed.
        let mut consecutive = 0u32;
        let mut ready = false;

        loop {
            ticker.tick().await;
            if tx.is_closed() {
                return;
            }

            match probe_once(&self.check.probe, &self.ctx, self.check.timeout).await {
                Ok(()) => {
                    consecutive = 0;
                    if !ready {
                        ready = true;
                        info!(service = %name, probe = %self.check.probe, "service is healthy");
                        self.events.emit(&name, EventKind::Healthy);
                        if tx.send(HealthSignal::Ready).is_err() {
                            return;
                        }
                    } else {
                        debug!(service = %name, "health probe passed");
                    }
                }
                Err(message) => {
                    // Failures during the start period never count: the
                    // service is still allowed to be starting up.
                    if started.elapsed() < self.check.start_period {
                        debug!(
                            service = %name,
                            %message,
                            "health probe failed during the start period"
                        );
                        continue;
                    }

                    consecutive += 1;
                    warn!(
                        service = %name,
                        %message,
                        consecutive,
                        retries = self.check.retries,
                        "health probe failed"
                    );
                    self.events.emit(
                        &name,
                        EventKind::HealthProbeFailed {
                            message: message.clone(),
                            consecutive,
                            retries: self.check.retries,
                        },
                    );

                    // Report the transition once, when the budget is first
                    // exhausted.  Monitoring continues so that a service left
                    // running by `on_unhealthy = "ignore"` can recover.
                    if consecutive == self.check.retries {
                        warn!(service = %name, %message, "service is unhealthy");
                        ready = false;
                        self.events.emit(
                            &name,
                            EventKind::Unhealthy {
                                message: message.clone(),
                            },
                        );
                        if tx.send(HealthSignal::Unhealthy { message }).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Whether a failing health check should stop the process.
pub fn stops_process(check: &HealthCheck) -> bool {
    check.on_unhealthy == UnhealthyAction::Restart
}

fn format_duration(d: Duration) -> String {
    humantime::format_duration(d).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    fn ctx() -> ProbeContext {
        ProbeContext {
            cwd: std::env::temp_dir(),
            env: std::env::vars().collect(),
        }
    }

    #[tokio::test]
    async fn a_command_probe_succeeds_on_exit_code_zero() {
        let probe = HealthProbe::Command {
            executable: "true".to_string(),
            args: vec![],
        };
        assert!(probe_once(&probe, &ctx(), Duration::from_secs(5))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn a_command_probe_fails_on_a_non_zero_exit_code() {
        let probe = HealthProbe::Command {
            executable: "false".to_string(),
            args: vec![],
        };
        let err = probe_once(&probe, &ctx(), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.contains("exited with code 1"), "{err}");
    }

    #[tokio::test]
    async fn a_command_probe_reports_a_missing_executable() {
        let probe = HealthProbe::Command {
            executable: "servicrab-does-not-exist".to_string(),
            args: vec![],
        };
        let err = probe_once(&probe, &ctx(), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.contains("failed to run"), "{err}");
    }

    #[tokio::test]
    async fn a_command_probe_times_out() {
        let probe = HealthProbe::Command {
            executable: "sleep".to_string(),
            args: vec!["30".to_string()],
        };
        let err = probe_once(&probe, &ctx(), Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
    }

    #[tokio::test]
    async fn a_tcp_probe_succeeds_against_a_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = HealthProbe::Tcp {
            host: "127.0.0.1".to_string(),
            port,
        };
        assert!(probe_once(&probe, &ctx(), Duration::from_secs(5))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn a_tcp_probe_fails_when_nothing_listens() {
        // Bind and immediately drop to get a port that is very likely free.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let probe = HealthProbe::Tcp {
            host: "127.0.0.1".to_string(),
            port,
        };
        let err = probe_once(&probe, &ctx(), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.contains("failed to connect"), "{err}");
    }

    /// Serve a single request with the given status line.
    async fn serve_once(status: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(format!("{status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
                .await;
        });
        port
    }

    #[tokio::test]
    async fn an_http_probe_accepts_a_2xx_response() {
        let port = serve_once("HTTP/1.1 204 No Content").await;
        let probe = HealthProbe::Http {
            url: format!("http://127.0.0.1:{port}/health"),
            host: "127.0.0.1".to_string(),
            port,
            path: "/health".to_string(),
        };
        assert!(probe_once(&probe, &ctx(), Duration::from_secs(5))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn an_http_probe_rejects_a_5xx_response() {
        let port = serve_once("HTTP/1.1 503 Service Unavailable").await;
        let probe = HealthProbe::Http {
            url: format!("http://127.0.0.1:{port}/"),
            host: "127.0.0.1".to_string(),
            port,
            path: "/".to_string(),
        };
        let err = probe_once(&probe, &ctx(), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.contains("unhealthy HTTP status 503"), "{err}");
    }
}
