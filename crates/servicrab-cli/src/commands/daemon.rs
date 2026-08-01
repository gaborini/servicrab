//! The daemon-facing commands: `daemon`, `start`, `status` and `down`.
//!
//! `daemon` is the body that supervises the stack; `start` launches it
//! detached; `status` and `down` are thin socket clients.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use servicrab_core::{load, plan_stack, resolve_config_path, Config, Selection, ServiceName};
use servicrab_protocol::ErrorCode;

use crate::daemon::{stopped, DaemonPaths};
use crate::output::{self, CliError};

/// How long to wait for a freshly spawned daemon to answer.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// How often to check on a daemon that has not answered yet.
const START_POLL: Duration = Duration::from_millis(50);
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
pub(crate) fn setup(config: Option<&Path>) -> Result<(Config, PathBuf, DaemonPaths), CliError> {
    let path = resolve_config_path(config)
        .map_err(|e| CliError::from(format!("could not find config: {e}")))?;

    let (cfg, warnings) = load(&path).map_err(|errors| {
        CliError::new(
            ErrorCode::ValidationFailed,
            format!("{} has {} error(s)", path.display(), errors.len()),
        )
        .with_errors(errors.iter().map(ToString::to_string).collect())
    })?;
    for warning in &warnings {
        eprintln!("⚠  {warning}");
    }

    let paths = DaemonPaths::for_config(&path);
    // Nothing here can work if the socket path cannot be bound, and every
    // command would otherwise report it differently and unhelpfully: the daemon
    // as `ENAMETOOLONG` from `bind`, every client as `SUN_LEN` from `connect`.
    // Said once, with the reason each candidate directory was refused.
    if !paths.socket_advice().is_empty() {
        return Err(format!(
            "the socket for {} cannot be created{}",
            cfg.project.name,
            paths.socket_advice()
        )
        .into());
    }
    Ok((cfg, path, paths))
}

#[cfg(unix)]
mod imp {
    use super::*;

    use std::os::unix::process::CommandExt;

    use servicrab_protocol::{Request, Response, ServiceInfo};

    use crate::daemon::{client, server};
    use crate::output::no_daemon;
    use crate::style::{self, BOLD, DIM, GREEN, RED, RESET, YELLOW};

    /// Run the daemon in the foreground (this is the process `start` spawns).
    pub fn daemon(
        config: Option<&Path>,
        no_restart: bool,
        profiles: &[String],
    ) -> Result<i32, CliError> {
        let (cfg, config_path, paths) = setup(config)?;
        server::serve(
            &cfg,
            &config_path,
            &paths,
            server::DaemonOptions {
                no_restart,
                profiles: profiles.to_vec(),
            },
        )
        .map_err(CliError::from)
    }

    /// Start the daemon, or individual services inside a running one.
    pub fn start(
        config: Option<&Path>,
        selection: Selection<'_>,
        options: StartOptions,
    ) -> Result<i32, CliError> {
        let services = selection.services;
        if !services.is_empty() {
            let code = control(config, services, |name| Request::StartService { name })?;
            if code != 0 || !options.wait {
                return Ok(code);
            }
            let (_, _, paths) = setup(config)?;
            // Naming a service is a request to start it, so nothing it may
            // have been remembered for holds any more.
            return wait_for_ready(&paths.socket, services, &BTreeSet::new(), options.timeout);
        }

        let (cfg, config_path, paths) = setup(config)?;

        // An advisory fast path for a friendly message.  It cannot be
        // authoritative — another `start` may be between this check and its
        // daemon's lock — so the daemon takes the pidfile lock and this code
        // reports whatever it says.
        match client::check_running(&paths.socket) {
            Ok(()) => {
                return Err(CliError::new(
                    ErrorCode::AlreadyRunning,
                    format!(
                "a daemon is already running for {} — use `servicrab status` or `servicrab down`",
                cfg.project.name
            ),
                ))
            }
            // A daemon that refuses us is still a daemon, and spawning another
            // one over its socket would fail in a much less obvious way.
            Err(client::ClientError::Failed(why)) => return Err(why.into()),
            Err(client::ClientError::NotRunning) => {}
        }
        paths.ensure_dir()?;

        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&paths.log)
            .map_err(|e| CliError::from(format!("could not open {}: {e}", paths.log.display())))?;
        let errors = log
            .try_clone()
            .map_err(|e| CliError::from(format!("could not open {}: {e}", paths.log.display())))?;

        let exe = std::env::current_exe()
            .map_err(|e| CliError::from(format!("could not find the servicrab executable: {e}")))?;
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
        // The daemon process is where the profiles have to live: `reload`
        // re-plans the stack, and it has to plan the one that was started.
        for profile in selection.profiles {
            command.arg("--profile").arg(profile);
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
            .map_err(|e| CliError::from(format!("could not start the daemon: {e}")))?;

        wait_for_the_daemon(&mut child, &paths)?;

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
            // The daemon leaves hand-stopped services stopped, so waiting for
            // them to become ready would only ever time out.
            let held_back = plan_stack(&cfg, selection)
                .map(|plan| stopped::held_back(&cfg, &plan, &stopped::read(&paths.stopped)))
                .unwrap_or_default();
            return wait_for_ready(&paths.socket, &[], &held_back, options.timeout);
        }
        Ok(0)
    }

    /// Wait for the spawned daemon to answer, or to tell us why it will not.
    ///
    /// Two starts can race here, and only one of them gets the project lock.
    /// The loser exits straight away — while the *winner's* socket is up, so a
    /// live socket alone proves nothing about the child we spawned.  The lock
    /// holder records its pid before it binds, so the pidfile is what identifies
    /// whose daemon is answering; the child's exit status then turns a
    /// 15-second timeout into an immediate, accurate message, and reaps the
    /// process either way.
    fn wait_for_the_daemon(
        child: &mut std::process::Child,
        paths: &DaemonPaths,
    ) -> Result<(), CliError> {
        let deadline = std::time::Instant::now() + START_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if !status.success() => {
                    return Err(format!(
                        "the daemon we started exited with {} — see {}",
                        status
                            .code()
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "a signal".to_string()),
                        paths.log.display()
                    )
                    .into())
                }
                // A stack of nothing but one-shot services can be finished
                // before we look, and that is a start that worked.
                Ok(Some(_)) => return Ok(()),
                Ok(None) => {}
                Err(err) => return Err(format!("could not watch the daemon: {err}").into()),
            }
            if daemon_is_ours(child, paths) && client::is_running(&paths.socket) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                // Reap it so a failed start does not leave a zombie behind.
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the daemon did not come up within {}s — see {}",
                    START_TIMEOUT.as_secs(),
                    paths.log.display()
                )
                .into());
            }
            std::thread::sleep(START_POLL);
        }
    }

    /// Whether the daemon holding this project's lock is the child we spawned.
    ///
    /// The pid in the file is written under the lock and before the socket is
    /// bound, so a socket that answers while this says "not ours" belongs to a
    /// daemon somebody else started.
    fn daemon_is_ours(child: &std::process::Child, paths: &DaemonPaths) -> bool {
        std::fs::read_to_string(&paths.pid)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .is_some_and(|pid| pid == child.id())
    }

    /// What a status snapshot says about one service's readiness.    ///
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

    /// Poll the daemon until every service in `only` (or all of them) is ready,
    /// leaving out the ones in `skip`.
    ///
    /// The daemon is left running either way: a stack that came up wrong is
    /// easier to diagnose alive, with `status`, `logs` and `events`.
    fn wait_for_ready(
        socket: &Path,
        only: &[String],
        skip: &BTreeSet<ServiceName>,
        timeout: Option<Duration>,
    ) -> Result<i32, CliError> {
        let timeout = timeout.unwrap_or(WAIT_TIMEOUT);
        let deadline = std::time::Instant::now() + timeout;
        let color = style::color_enabled();

        loop {
            let services = match client::send(socket, &Request::Status) {
                Ok(Response::Status { services }) => services,
                Ok(Response::Error { code, message, .. }) => {
                    return Err(CliError::new(code, message))
                }
                Ok(other) => {
                    return Err(format!("unexpected response from the daemon: {other:?}").into())
                }
                Err(client::ClientError::NotRunning) => {
                    return Err(CliError::new(
                        ErrorCode::NotRunning,
                        "the daemon stopped while waiting for the stack",
                    )
                    .with_exit(output::EXIT_NO_DAEMON))
                }
                Err(err) => return Err(err.to_string().into()),
            };

            let watched: Vec<&ServiceInfo> = services
                .iter()
                .filter(|service| only.is_empty() || only.contains(&service.name))
                .filter(|service| !skip.iter().any(|name| name.as_str() == service.name))
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
    pub fn stop(config: Option<&Path>, services: &[String]) -> Result<i32, CliError> {
        control(config, services, |name| Request::StopService { name })
    }

    /// Refuse to go on unless a daemon we may talk to is there.
    ///
    /// "Is one running" and "will it talk to us" are different questions, and
    /// the second one has an answer worth repeating: a daemon belonging to
    /// another user refuses the connection and says so, and reporting that as
    /// "no daemon is running" would send the operator looking for one that is
    /// already there.
    fn expect_a_daemon(cfg: &Config, paths: &DaemonPaths) -> Result<(), CliError> {
        match client::check_running(&paths.socket) {
            Ok(()) => Ok(()),
            Err(client::ClientError::NotRunning) => {
                Err(no_daemon(cfg.project.name.as_str(), false))
            }
            Err(err) => Err(err.to_string().into()),
        }
    }

    /// Restart individual services.
    pub fn restart(config: Option<&Path>, services: &[String]) -> Result<i32, CliError> {
        control(config, services, |name| Request::RestartService { name })
    }

    /// Ask the daemon to re-read the configuration file.
    pub fn reload(config: Option<&Path>) -> Result<i32, CliError> {
        let (cfg, config_path, paths) = setup(config)?;
        expect_a_daemon(&cfg, &paths)?;

        let color = style::color_enabled();
        match client::send(&paths.socket, &Request::Reload) {
            Ok(Response::Ok { message, .. }) => {
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
            // A config the daemon refused is the operator's problem to fix, and
            // it is reported the way every other error is.
            Ok(Response::Error {
                code,
                message,
                errors,
            }) => Err(CliError::new(code, message).with_errors(errors)),
            Ok(other) => Err(format!("unexpected response from the daemon: {other:?}").into()),
            Err(err) => Err(err.to_string().into()),
        }
    }

    /// Send one per-service command per name, reporting each outcome.
    fn control(
        config: Option<&Path>,
        services: &[String],
        build: impl Fn(String) -> Request,
    ) -> Result<i32, CliError> {
        let (cfg, _, paths) = setup(config)?;
        expect_a_daemon(&cfg, &paths)?;

        let color = style::color_enabled();
        let mut failed = false;
        for name in services {
            match client::send(&paths.socket, &build(name.clone())) {
                Ok(Response::Ok { message, .. }) => println!(
                    "{} {}",
                    style::paint(color, GREEN, "✓"),
                    message.unwrap_or_else(|| format!("{name} done"))
                ),
                // Reported like every other error, and the loop carries on so
                // the remaining names are still attempted.
                Ok(Response::Error {
                    code,
                    message,
                    errors,
                }) => {
                    CliError::new(code, message).with_errors(errors).report();
                    failed = true;
                }
                Ok(other) => {
                    return Err(format!("unexpected response from the daemon: {other:?}").into())
                }
                Err(err) => return Err(err.to_string().into()),
            }
        }
        Ok(if failed { 1 } else { 0 })
    }

    /// Print what the daemon is doing.
    pub fn status(config: Option<&Path>, json: bool) -> Result<i32, CliError> {
        let (cfg, _, paths) = setup(config).map_err(|e| e.in_json(json))?;

        let response = match client::send(&paths.socket, &Request::Status) {
            Ok(response) => response,
            Err(client::ClientError::NotRunning) => {
                if json {
                    // Through serde like the running case, rather than a
                    // hand-written string that could drift from it.
                    output::print_document(StatusJson {
                        running: false,
                        services: Vec::new(),
                    })?;
                    return Ok(output::EXIT_NO_DAEMON);
                }
                let mut error = no_daemon(cfg.project.name.as_str(), false);
                // Nothing else would lead them to a relocated socket, and
                // "not running" is exactly the moment someone starts
                // looking for one.  Said only when it is somewhere
                // surprising, so the ordinary output does not change.
                if !paths.socket_is_in_place() {
                    error = error.with_hint(format!(
                        "its socket would be {} (the project's path is too long to hold one)",
                        paths.socket.display()
                    ));
                }
                return Err(error);
            }
            Err(err) => return Err(CliError::from(err.to_string()).in_json(json)),
        };

        let services = match response {
            Response::Status { services } => services,
            Response::Error {
                code,
                message,
                errors,
            } => {
                return Err(CliError::new(code, message)
                    .with_errors(errors)
                    .in_json(json))
            }
            other => {
                return Err(CliError::from(format!(
                    "unexpected response from the daemon: {other:?}"
                ))
                .in_json(json))
            }
        };

        if json {
            output::print_document(StatusJson {
                running: true,
                services,
            })?;
        } else {
            print_table(&services);
        }
        Ok(0)
    }

    /// The `status --json` document, for both the running and the absent case.
    ///
    /// One type for both, so the "not running" answer cannot drift away from
    /// the running one: it used to be a hand-written `{"running":false,
    /// "services":[]}` that never went through serde at all.
    #[derive(serde::Serialize)]
    struct StatusJson {
        running: bool,
        services: Vec<ServiceInfo>,
    }

    /// Ask the daemon to stop the stack and exit.
    pub fn down(config: Option<&Path>) -> Result<i32, CliError> {
        let (cfg, _, paths) = setup(config)?;

        match client::send(&paths.socket, &Request::Shutdown) {
            Ok(Response::Ok { .. }) => {}
            Ok(Response::Error {
                code,
                message,
                errors,
            }) => return Err(CliError::new(code, message).with_errors(errors)),
            Ok(other) => {
                return Err(format!("unexpected response from the daemon: {other:?}").into())
            }
            Err(client::ClientError::NotRunning) => {
                // Not a failure — `down` on an already stopped stack is meant
                // to be safe to run twice — but the exit code says there was
                // nothing to do, so a script can tell the two apart.
                println!("no daemon is running for {}", cfg.project.name);
                return Ok(output::EXIT_NO_DAEMON);
            }
            Err(err) => return Err(err.to_string().into()),
        }

        if !client::wait_until_stopped(&paths.socket, STOP_TIMEOUT) {
            return Err(format!(
                "the daemon did not stop within {}s — see {}",
                STOP_TIMEOUT.as_secs(),
                paths.log.display()
            )
            .into());
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
                    .pgid
                    .or(service.pid)
                    .map(|pgid| pgid.to_string())
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

    use servicrab_protocol::ErrorCode;

    const UNSUPPORTED: &str = "the background daemon is only supported on Linux and macOS";

    fn unsupported() -> CliError {
        CliError::new(ErrorCode::Unsupported, UNSUPPORTED)
    }

    pub fn daemon(
        _config: Option<&Path>,
        _no_restart: bool,
        _profiles: &[String],
    ) -> Result<i32, CliError> {
        Err(unsupported())
    }

    pub fn start(
        _config: Option<&Path>,
        _selection: Selection<'_>,
        _options: StartOptions,
    ) -> Result<i32, CliError> {
        Err(unsupported())
    }

    pub fn stop(_config: Option<&Path>, _services: &[String]) -> Result<i32, CliError> {
        Err(unsupported())
    }

    pub fn restart(_config: Option<&Path>, _services: &[String]) -> Result<i32, CliError> {
        Err(unsupported())
    }

    pub fn reload(_config: Option<&Path>) -> Result<i32, CliError> {
        Err(unsupported())
    }

    pub fn status(_config: Option<&Path>, json: bool) -> Result<i32, CliError> {
        Err(unsupported().in_json(json))
    }

    pub fn down(_config: Option<&Path>) -> Result<i32, CliError> {
        Err(unsupported())
    }
}

pub use imp::{daemon, down, reload, restart, start, status, stop};
