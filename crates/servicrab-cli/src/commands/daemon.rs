//! The daemon-facing commands: `daemon`, `start`, `status` and `down`.
//!
//! `daemon` is the body that supervises the stack; `start` launches it
//! detached; `status` and `down` are thin socket clients.

use std::path::{Path, PathBuf};
use std::time::Duration;

use servicrab_core::{load, resolve_config_path, Config};

use crate::daemon::DaemonPaths;

/// How long to wait for a freshly spawned daemon to answer.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait for a stopping daemon to disappear.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `--wait` waits for readiness when `--timeout` is not given.
///
/// Generous on purpose: this has to cover a health check's `start_period` plus
/// a probe interval or two, and a wait that gives up too early is worse than
/// one that takes a moment.
const WAIT_TIMEOUT: Duration = Duration::from_secs(60);
/// How often `--wait` asks the daemon for a status snapshot.
const WAIT_POLL: Duration = Duration::from_millis(100);

/// Options for `servicrab start`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StartOptions {
    /// Never restart services, whatever their configured policy says.
    pub no_restart: bool,
    /// Return only once every started service is ready.
    pub wait: bool,
    /// How long to wait for that; [`WAIT_TIMEOUT`] when absent.
    pub timeout: Option<Duration>,
}

/// Load the config the daemon commands all need.
pub(crate) fn setup(config: Option<&Path>) -> Result<(Config, PathBuf, DaemonPaths), String> {
    let path = resolve_config_path(config).map_err(|e| format!("could not find config: {e}"))?;

    let (cfg, warnings) = load(&path).map_err(|errors| {
        let msgs: Vec<String> = errors.iter().map(|e| format!("  • {e}")).collect();
        format!(
            "✗ {} has {} error(s):\n{}",
            path.display(),
            errors.len(),
            msgs.join("\n")
        )
    })?;
    for warning in &warnings {
        eprintln!("⚠  {warning}");
    }

    let paths = DaemonPaths::for_config(&path);
    Ok((cfg, path, paths))
}

#[cfg(unix)]
mod imp {
    use super::*;

    use std::os::unix::process::CommandExt;

    use servicrab_protocol::{Request, Response, ServiceInfo};

    use crate::daemon::{client, server};
    use crate::style::{self, BOLD, DIM, GREEN, RED, RESET, YELLOW};

    /// Run the daemon in the foreground (this is the process `start` spawns).
    pub fn daemon(config: Option<&Path>, no_restart: bool) -> Result<i32, String> {
        let (cfg, config_path, paths) = setup(config)?;
        server::serve(
            &cfg,
            &config_path,
            &paths,
            server::DaemonOptions { no_restart },
        )
    }

    /// Start the daemon, or individual services inside a running one.
    pub fn start(
        config: Option<&Path>,
        services: &[String],
        options: StartOptions,
    ) -> Result<i32, String> {
        if !services.is_empty() {
            let code = control(config, services, |name| Request::StartService { name })?;
            if code != 0 || !options.wait {
                return Ok(code);
            }
            let (_, _, paths) = setup(config)?;
            return wait_for_ready(&paths.socket, services, options.timeout);
        }

        let (cfg, config_path, paths) = setup(config)?;

        if client::is_running(&paths.socket) {
            return Err(format!(
                "a daemon is already running for {} — use `servicrab status` or `servicrab down`",
                cfg.project.name
            ));
        }
        paths.ensure_dir()?;

        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&paths.log)
            .map_err(|e| format!("could not open {}: {e}", paths.log.display()))?;
        let errors = log
            .try_clone()
            .map_err(|e| format!("could not open {}: {e}", paths.log.display()))?;

        let exe = std::env::current_exe()
            .map_err(|e| format!("could not find the servicrab executable: {e}"))?;
        let mut command = std::process::Command::new(exe);
        command
            .arg("daemon")
            .arg("--config")
            .arg(&config_path)
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(errors);
        if options.no_restart {
            command.arg("--no-restart");
        }
        // A new session detaches the daemon from this terminal, so Ctrl+C here
        // no longer reaches it and it survives the shell that started it.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid().map_err(std::io::Error::from)?;
                Ok(())
            });
        }

        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start the daemon: {e}"))?;

        if !client::wait_until_running(&paths.socket, START_TIMEOUT) {
            // Reap it so a failed start does not leave a zombie behind.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "the daemon did not come up within {}s — see {}",
                START_TIMEOUT.as_secs(),
                paths.log.display()
            ));
        }

        let color = style::color_enabled();
        println!(
            "{} daemon started for {} ({} service(s))",
            style::paint(color, GREEN, "✓"),
            style::paint(color, BOLD, cfg.project.name.as_str()),
            cfg.services.len()
        );
        println!(
            "{}",
            style::paint(
                color,
                DIM,
                &format!(
                    "  logs: {}  ·  socket: {}",
                    paths.log.display(),
                    paths.socket.display()
                )
            )
        );

        if options.wait {
            return wait_for_ready(&paths.socket, &[], options.timeout);
        }
        Ok(0)
    }

    /// What a status snapshot says about one service's readiness.
    ///
    /// The same three answers the supervisor's own dependency gating uses under
    /// its *default* condition, so `--wait` returns when a dependent that did
    /// not spell out a condition would have been allowed to start.  A spelled
    /// out `service_completed_successfully` is not reflected: this asks whether
    /// a service is ready, not whether it satisfies a particular dependent, and
    /// a status snapshot carries no exit status to check.
    #[derive(Debug, PartialEq, Eq)]
    enum Readiness {
        /// Up, and health-checked if it promised a health check.
        Ready,
        /// Not there yet, but still on its way.
        Waiting,
        /// It will not become ready without someone intervening.
        Gone(String),
    }

    fn readiness(service: &ServiceInfo) -> Readiness {
        use servicrab_protocol::{Health, ServiceState};

        match service.state {
            ServiceState::Running => match service.health {
                Health::None | Health::Healthy => Readiness::Ready,
                // An unhealthy service may still be restarted back into shape,
                // so this is a wait rather than a verdict.  A verdict this
                // client does not know about is one to wait out, not to read as
                // readiness.
                Health::Starting | Health::Unhealthy | _ => Readiness::Waiting,
            },
            // A one-shot service — a migration, a build step — has done its job.
            // Unless it was stopped precisely because it was unhealthy.
            ServiceState::Exited if service.health != Health::Unhealthy => Readiness::Ready,
            ServiceState::Exited => Readiness::Gone("exited unhealthy".to_string()),
            ServiceState::Failed => Readiness::Gone(
                service
                    .message
                    .clone()
                    .unwrap_or_else(|| "failed".to_string()),
            ),
            ServiceState::Stopped => Readiness::Gone("stopped".to_string()),
            ServiceState::Pending
            | ServiceState::Starting
            | ServiceState::Backoff
            | ServiceState::Stopping
            // Same reasoning for a state added by a newer daemon: wait, and let
            // the timeout be the one to give up.
            | _ => Readiness::Waiting,
        }
    }

    /// Poll the daemon until every service in `only` (or all of them) is ready.
    ///
    /// The daemon is left running either way: a stack that came up wrong is
    /// easier to diagnose alive, with `status`, `logs` and `events`.
    fn wait_for_ready(
        socket: &Path,
        only: &[String],
        timeout: Option<Duration>,
    ) -> Result<i32, String> {
        let timeout = timeout.unwrap_or(WAIT_TIMEOUT);
        let deadline = std::time::Instant::now() + timeout;
        let color = style::color_enabled();

        loop {
            let services = match client::send(socket, &Request::Status) {
                Ok(Response::Status { services }) => services,
                Ok(Response::Error { message }) => return Err(message),
                Ok(other) => return Err(format!("unexpected response from the daemon: {other:?}")),
                Err(client::ClientError::NotRunning) => {
                    return Err("the daemon stopped while waiting for the stack".to_string())
                }
                Err(err) => return Err(err.to_string()),
            };

            let watched: Vec<&ServiceInfo> = services
                .iter()
                .filter(|service| only.is_empty() || only.contains(&service.name))
                .collect();

            let mut waiting: Vec<&str> = Vec::new();
            let mut gone: Vec<(&str, String)> = Vec::new();
            for service in &watched {
                match readiness(service) {
                    Readiness::Ready => {}
                    Readiness::Waiting => waiting.push(&service.name),
                    Readiness::Gone(why) => gone.push((&service.name, why)),
                }
            }

            if !gone.is_empty() {
                for (name, why) in &gone {
                    eprintln!("{} {name}: {why}", style::paint(color, RED, "✗"));
                }
                return Ok(1);
            }

            if waiting.is_empty() {
                println!(
                    "{} {} service(s) ready",
                    style::paint(color, GREEN, "✓"),
                    watched.len()
                );
                return Ok(0);
            }

            if std::time::Instant::now() >= deadline {
                eprintln!(
                    "{} timed out after {}: still waiting for {}",
                    style::paint(color, RED, "✗"),
                    humantime::format_duration(timeout),
                    waiting.join(", ")
                );
                eprintln!(
                    "{}",
                    style::paint(
                        color,
                        DIM,
                        "  the daemon is still running — see `servicrab status` and `servicrab logs`"
                    )
                );
                return Ok(1);
            }

            std::thread::sleep(WAIT_POLL);
        }
    }

    /// Stop individual services without touching the rest of the stack.
    pub fn stop(config: Option<&Path>, services: &[String]) -> Result<i32, String> {
        control(config, services, |name| Request::StopService { name })
    }

    /// Restart individual services.
    pub fn restart(config: Option<&Path>, services: &[String]) -> Result<i32, String> {
        control(config, services, |name| Request::RestartService { name })
    }

    /// Ask the daemon to re-read the configuration file.
    pub fn reload(config: Option<&Path>) -> Result<i32, String> {
        let (cfg, config_path, paths) = setup(config)?;
        if !client::is_running(&paths.socket) {
            return Err(format!(
                "no daemon is running for {} — start one with `servicrab start`",
                cfg.project.name
            ));
        }

        let color = style::color_enabled();
        match client::send(&paths.socket, &Request::Reload) {
            Ok(Response::Ok { message }) => {
                println!(
                    "{} {}",
                    style::paint(color, GREEN, "✓"),
                    message.unwrap_or_else(|| "reloaded".to_string())
                );
                println!(
                    "{}",
                    style::paint(color, DIM, &format!("  from {}", config_path.display()))
                );
                Ok(0)
            }
            Ok(Response::Error { message }) => {
                eprintln!("{} {message}", style::paint(color, RED, "✗"));
                Ok(1)
            }
            Ok(other) => Err(format!("unexpected response from the daemon: {other:?}")),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Send one per-service command per name, reporting each outcome.
    fn control(
        config: Option<&Path>,
        services: &[String],
        build: impl Fn(String) -> Request,
    ) -> Result<i32, String> {
        let (cfg, _, paths) = setup(config)?;
        if !client::is_running(&paths.socket) {
            return Err(format!(
                "no daemon is running for {} — start one with `servicrab start`",
                cfg.project.name
            ));
        }

        let color = style::color_enabled();
        let mut failed = false;
        for name in services {
            match client::send(&paths.socket, &build(name.clone())) {
                Ok(Response::Ok { message }) => println!(
                    "{} {}",
                    style::paint(color, GREEN, "✓"),
                    message.unwrap_or_else(|| format!("{name} done"))
                ),
                Ok(Response::Error { message }) => {
                    eprintln!("{} {message}", style::paint(color, RED, "✗"));
                    failed = true;
                }
                Ok(other) => return Err(format!("unexpected response from the daemon: {other:?}")),
                Err(err) => return Err(err.to_string()),
            }
        }
        Ok(if failed { 1 } else { 0 })
    }

    /// Print what the daemon is doing.
    pub fn status(config: Option<&Path>, json: bool) -> Result<i32, String> {
        let (cfg, _, paths) = setup(config)?;

        let response = match client::send(&paths.socket, &Request::Status) {
            Ok(response) => response,
            Err(client::ClientError::NotRunning) => {
                if json {
                    println!("{{\"running\":false,\"services\":[]}}");
                } else {
                    println!(
                        "no daemon is running for {} — start one with `servicrab start`",
                        cfg.project.name
                    );
                }
                return Ok(1);
            }
            Err(err) => return Err(err.to_string()),
        };

        let services = match response {
            Response::Status { services } => services,
            Response::Error { message } => return Err(message),
            other => return Err(format!("unexpected response from the daemon: {other:?}")),
        };

        if json {
            let payload = serde_json::json!({ "running": true, "services": services });
            println!(
                "{}",
                serde_json::to_string_pretty(&payload)
                    .map_err(|e| format!("could not render JSON: {e}"))?
            );
        } else {
            print_table(&services);
        }
        Ok(0)
    }

    /// Ask the daemon to stop the stack and exit.
    pub fn down(config: Option<&Path>) -> Result<i32, String> {
        let (cfg, _, paths) = setup(config)?;

        match client::send(&paths.socket, &Request::Shutdown) {
            Ok(Response::Ok { .. }) => {}
            Ok(Response::Error { message }) => return Err(message),
            Ok(other) => return Err(format!("unexpected response from the daemon: {other:?}")),
            Err(client::ClientError::NotRunning) => {
                println!("no daemon is running for {}", cfg.project.name);
                return Ok(0);
            }
            Err(err) => return Err(err.to_string()),
        }

        if !client::wait_until_stopped(&paths.socket, STOP_TIMEOUT) {
            return Err(format!(
                "the daemon did not stop within {}s — see {}",
                STOP_TIMEOUT.as_secs(),
                paths.log.display()
            ));
        }

        let color = style::color_enabled();
        println!(
            "{} stopped {}",
            style::paint(color, GREEN, "✓"),
            style::paint(color, BOLD, cfg.project.name.as_str())
        );
        Ok(0)
    }

    fn print_table(services: &[ServiceInfo]) {
        let color = style::color_enabled();
        let width = services
            .iter()
            .map(|s| s.name.len())
            .max()
            .unwrap_or(7)
            .max(7);

        println!(
            "{}",
            style::paint(
                color,
                BOLD,
                &format!(
                    "{:width$}  {:<9}  {:>7}  {:>8}  {:>8}  {}",
                    "SERVICE", "STATE", "PID", "UPTIME", "RESTARTS", "HEALTH"
                )
            )
        );

        for service in services {
            let state = service.state.to_string();
            let tint = match service.state {
                servicrab_protocol::ServiceState::Running => GREEN,
                servicrab_protocol::ServiceState::Failed => RED,
                servicrab_protocol::ServiceState::Backoff
                | servicrab_protocol::ServiceState::Stopping => YELLOW,
                _ => RESET,
            };
            println!(
                "{:width$}  {:<9}  {:>7}  {:>8}  {:>8}  {}",
                service.name,
                style::paint(color, tint, &state),
                service
                    .pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                service
                    .uptime_secs
                    .map(format_uptime)
                    .unwrap_or_else(|| "-".to_string()),
                service.restarts,
                service.health,
            );
        }

        for service in services {
            if let Some(message) = &service.message {
                println!(
                    "{}",
                    style::paint(color, DIM, &format!("  {}: {message}", service.name))
                );
            }
        }
    }

    /// Render seconds as `12s`, `4m30s` or `2h05m`.
    fn format_uptime(secs: u64) -> String {
        match secs {
            0..=59 => format!("{secs}s"),
            60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
            _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn uptime_is_rendered_compactly() {
            assert_eq!(format_uptime(0), "0s");
            assert_eq!(format_uptime(59), "59s");
            assert_eq!(format_uptime(60), "1m00s");
            assert_eq!(format_uptime(3599), "59m59s");
            assert_eq!(format_uptime(3600), "1h00m");
            assert_eq!(format_uptime(7860), "2h11m");
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    const UNSUPPORTED: &str = "the background daemon is only supported on Linux and macOS";

    pub fn daemon(_config: Option<&Path>, _no_restart: bool) -> Result<i32, String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn start(
        _config: Option<&Path>,
        _services: &[String],
        _options: StartOptions,
    ) -> Result<i32, String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn stop(_config: Option<&Path>, _services: &[String]) -> Result<i32, String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn restart(_config: Option<&Path>, _services: &[String]) -> Result<i32, String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn reload(_config: Option<&Path>) -> Result<i32, String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn status(_config: Option<&Path>, _json: bool) -> Result<i32, String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn down(_config: Option<&Path>) -> Result<i32, String> {
        Err(UNSUPPORTED.to_string())
    }
}

pub use imp::{daemon, down, reload, restart, start, status, stop};
