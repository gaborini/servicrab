//! `servicrab up [SERVICE...]` — run a whole stack in the foreground.
//!
//! `servicrab watch` is the same supervisor with a stricter entry check: it
//! refuses to start when nothing in the plan declares a `[watch]` block.
//!
//! This module only renders events; starting, restarting, and stopping the
//! services is entirely [`servicrab_core::runtime::stack`]'s job.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use servicrab_core::runtime::stack::{
    control_channel, ServiceResult, StackOptions, StackSupervisor,
};
use servicrab_core::{
    event_channel, load, plan_stack, resolve_config_path, spawn_watchers, watched_services, Config,
    EventKind, EventReceiver, LogRouter, Selection, ServiceName, ShutdownReason, SignalWatcher,
    Stream,
};

use crate::style::{self, BOLD, DIM, RESET, SERVICE_COLORS};

/// Exit code used when a run is cut short by Ctrl+C (`128 + SIGINT`).
const EXIT_SIGINT: i32 = 130;
/// Exit code used when the supervisor itself was terminated (`128 + SIGTERM`).
const EXIT_SIGTERM: i32 = 143;

/// Command-line options for `up`.
#[derive(Debug, Clone, Copy, Default)]
pub struct UpOptions {
    /// Disable automatic restarts for every service.
    pub no_restart: bool,
    /// Do not prefix output lines with the service name.
    pub no_prefix: bool,
    /// Prefix output lines with a UTC timestamp.
    pub timestamps: bool,
    /// Stop the whole stack as soon as one service fails.
    pub abort_on_failure: bool,
    /// Fail when no service in the plan declares a `[watch]` block.  Set by
    /// `servicrab watch`.
    pub require_watch: bool,
    /// Print one JSON event per line on stdout instead of rendering for a
    /// terminal.
    pub json: bool,
}

/// Run the `up` subcommand, returning the process exit code to use.
pub fn run(
    selection: Selection<'_>,
    config: Option<&Path>,
    options: UpOptions,
) -> Result<i32, String> {
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

    let plan = plan_stack(&cfg, selection).map_err(|e| e.to_string())?;

    let watched = watched_services(&cfg, &plan);
    if options.require_watch && watched.is_empty() {
        let names: Vec<&str> = plan.iter().map(|n| n.as_str()).collect();
        return Err(format!(
            "nothing to watch: none of {} declares a [watch] block.\n\
             Add one, for example:\n\n\
             \x20 [services.{}.watch]\n\
             \x20 paths = [\"src\"]",
            names.join(", "),
            names.first().copied().unwrap_or("api"),
        ));
    }

    let printer = Printer::new(&plan, options);
    printer.banner(&cfg, &plan);
    printer.watching(&watched);

    let logs = crate::commands::logs::router_for(&cfg);

    let stack_options = StackOptions {
        no_restart: options.no_restart,
        abort_on_failure: options.abort_on_failure,
        keep_running: false,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start the async runtime: {e}"))?;

    let outcome = runtime.block_on(async move {
        let signals = SignalWatcher::install(cfg.project.name.as_str())?;
        let mut shutdown = signals.subscribe();

        let (events_tx, events_rx) = event_channel();
        let renderer = tokio::spawn(render(events_rx, printer, logs));

        let (control_tx, control_rx) = control_channel();
        let watchers = spawn_watchers(&cfg, &plan, &control_tx, &events_tx);
        drop(control_tx);

        let supervisor =
            StackSupervisor::new(&cfg, plan, stack_options, events_tx).with_control(control_rx);
        let outcome = supervisor.run(&mut shutdown).await;

        for watcher in watchers {
            watcher.abort();
        }

        // The supervisor owned the last sender, so the renderer stops as soon
        // as it has drained the queue.
        let printer = renderer.await.expect("renderer task");
        printer.summary(&outcome);

        Ok::<_, servicrab_core::RuntimeError>(outcome)
    });

    let outcome = outcome.map_err(|e| e.to_string())?;

    if !outcome.is_success() {
        return Ok(1);
    }
    Ok(match outcome.shutdown {
        Some(ShutdownReason::UserInterrupt) => EXIT_SIGINT,
        Some(ShutdownReason::Terminated) => EXIT_SIGTERM,
        Some(_) => 1,
        None => 0,
    })
}

/// Drain the event stream, rendering everything as it arrives and copying
/// captured output to the log files when file logging is enabled.
async fn render(
    mut events: EventReceiver,
    printer: Printer,
    mut logs: Option<LogRouter>,
) -> Printer {
    while let Some(event) = events.recv().await {
        if let (Some(router), EventKind::Log { line, .. }) = (logs.as_mut(), &event.kind) {
            if let Some(problem) = router.record(&event.service, line) {
                printer.warn(&problem);
            }
        }
        printer.event(&event.service, &event.kind);
    }
    printer
}

/// Renders runtime events for a terminal.
struct Printer {
    colors: BTreeMap<ServiceName, &'static str>,
    width: usize,
    color: bool,
    options: UpOptions,
}

impl Printer {
    fn new(plan: &[ServiceName], options: UpOptions) -> Self {
        let color = style::color_enabled();
        let width = plan.iter().map(|n| n.as_str().len()).max().unwrap_or(0);
        let colors = plan
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), SERVICE_COLORS[index % SERVICE_COLORS.len()]))
            .collect();
        Self {
            colors,
            width,
            color,
            options,
        }
    }

    fn banner(&self, config: &Config, plan: &[ServiceName]) {
        if self.options.json {
            return;
        }
        let names: Vec<&str> = plan.iter().map(|n| n.as_str()).collect();
        let command = if self.options.require_watch {
            "servicrab watch"
        } else {
            "servicrab up"
        };
        eprintln!(
            "{} {} → {}",
            style::paint(self.color, BOLD, command),
            config.project.name,
            names.join(", ")
        );
    }

    fn watching(&self, watched: &[ServiceName]) {
        if self.options.json || watched.is_empty() {
            return;
        }
        let names: Vec<&str> = watched.iter().map(|n| n.as_str()).collect();
        eprintln!(
            "{}",
            style::paint(
                self.color,
                DIM,
                &format!("watching for changes: {}", names.join(", "))
            )
        );
    }

    /// `api    | ` (or an empty string when prefixes are disabled).
    fn prefix(&self, service: &ServiceName) -> String {
        if self.options.no_prefix {
            return String::new();
        }
        let color = self.colors.get(service).copied().unwrap_or(RESET);
        let label = format!("{:width$}", service.as_str(), width = self.width);
        format!("{} {} ", style::paint(self.color, color, &label), "|")
    }

    fn timestamp(&self) -> String {
        if !self.options.timestamps {
            return String::new();
        }
        let now = style::utc_hms(std::time::SystemTime::now());
        format!("{} ", style::paint(self.color, DIM, &now))
    }

    fn event(&self, service: &ServiceName, kind: &EventKind) {
        if self.options.json {
            self.json(service, kind);
            return;
        }
        match kind {
            EventKind::Log { stream, line } => self.log(service, *stream, line),
            EventKind::Started { pgid } => {
                self.status(service, "▶", &format!("started (pgid {pgid})"))
            }
            EventKind::Exited { reason, uptime } => self.status(
                service,
                "■",
                &format!("{reason} after {}", humanize(*uptime)),
            ),
            EventKind::Backoff { delay, attempt } => self.status(
                service,
                "↻",
                &format!("restarting in {} (attempt {attempt})", humanize(*delay)),
            ),
            EventKind::Stopping { reason } => {
                self.status(service, "◼", &format!("stopping: {reason}"))
            }
            EventKind::Skipped { dependency } => self.status(
                service,
                "⊘",
                &format!("skipped: dependency {dependency} never became available"),
            ),
            EventKind::Failed { message } => self.status(service, "✗", message),
            EventKind::Healthy => self.status(service, "✔", "healthy"),
            EventKind::HealthProbeFailed {
                message,
                consecutive,
                retries,
            } => self.status(
                service,
                "!",
                &format!("health probe failed ({consecutive}/{retries}): {message}"),
            ),
            EventKind::Unhealthy { message } => {
                self.status(service, "✗", &format!("unhealthy: {message}"))
            }
            EventKind::WatchTriggered { path, changed } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let detail = if *changed == 1 {
                    format!("{name} changed")
                } else {
                    format!("{name} and {} more changed", changed - 1)
                };
                self.status(service, "↻", &format!("{detail}; restarting"))
            }
            EventKind::WatchFailed { message } => {
                self.status(service, "!", &format!("watch: {message}"))
            }
            EventKind::WatchTruncated { limit } => self.status(
                service,
                "!",
                &format!("watch: more than {limit} files; narrow `paths` or add `ignore` entries"),
            ),
            // State transitions and the final summary are already conveyed by
            // the events above; showing them too would only add noise.
            EventKind::State(_) | EventKind::Finished { .. } => {}
        }
    }

    /// Emit one event in the same shape the daemon streams over its socket,
    /// so `up --json` and `events --json` can feed the same tooling.
    fn json(&self, service: &ServiceName, kind: &EventKind) {
        let response = servicrab_protocol::Response::Event {
            service: service.to_string(),
            event: crate::wire::to_wire_event(kind),
        };
        let Ok(line) = servicrab_protocol::encode(&response) else {
            return;
        };
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(line.as_bytes());
        let _ = out.flush();
    }

    fn log(&self, service: &ServiceName, stream: Stream, line: &str) {
        let text = format!("{}{}{}", self.timestamp(), self.prefix(service), line);
        match stream {
            Stream::Stdout => {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{text}");
                let _ = out.flush();
            }
            Stream::Stderr => {
                let mut err = std::io::stderr().lock();
                let _ = writeln!(err, "{text}");
                let _ = err.flush();
            }
        }
    }

    fn status(&self, service: &ServiceName, symbol: &str, message: &str) {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "{}{}{}",
            self.timestamp(),
            self.prefix(service),
            style::paint(self.color, DIM, &format!("{symbol} {message}"))
        );
        let _ = err.flush();
    }

    /// Report a problem with the supervisor itself (not with a service).
    fn warn(&self, message: &str) {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "⚠  {message}");
        let _ = err.flush();
    }

    fn summary(&self, outcome: &servicrab_core::runtime::stack::StackOutcome) {
        if self.options.json {
            return;
        }
        if outcome.is_success() {
            eprintln!(
                "{}",
                style::paint(self.color, DIM, "all services stopped cleanly")
            );
            return;
        }
        for report in outcome.failures() {
            let detail = match &report.result {
                ServiceResult::Failed(err) => err.to_string(),
                ServiceResult::Skipped { dependency } => {
                    format!("skipped: dependency {dependency} never became available")
                }
                ServiceResult::Finished(_) => continue,
            };
            eprintln!("✗ {}: {}", report.service, detail);
        }
    }
}

/// Render a duration the way a human would say it.
fn humanize(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 60 {
        return format!("{}m{}s", secs / 60, secs % 60);
    }
    if secs > 0 {
        return format!("{secs}s");
    }
    format!("{}ms", duration.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn service(name: &str) -> ServiceName {
        // Round-trip through the public planner types is overkill here; the
        // list command already proves name validation, so build the plan from
        // a real config instead.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("servicrab.toml");
        std::fs::write(
            &path,
            format!("version = 1\n\n[project]\nname = \"demo\"\n\n[services.{name}]\ncommand = [\"true\"]\n"),
        )
        .expect("write config");
        let (config, _) = load(&path).expect("valid config");
        config.services.keys().next().expect("one service").clone()
    }

    #[test]
    fn durations_are_humanized() {
        assert_eq!(humanize(Duration::from_millis(250)), "250ms");
        assert_eq!(humanize(Duration::from_secs(5)), "5s");
        assert_eq!(humanize(Duration::from_secs(125)), "2m5s");
    }

    #[test]
    fn prefixes_are_padded_to_the_longest_name() {
        let plan = vec![service("api"), service("database")];
        let printer = Printer::new(&plan, UpOptions::default());
        let prefix = printer.prefix(&plan[0]);
        assert!(
            prefix.starts_with("api     "),
            "unexpected prefix {prefix:?}"
        );
        assert!(prefix.ends_with("| "));
    }

    #[test]
    fn prefixes_can_be_disabled() {
        let plan = vec![service("api")];
        let printer = Printer::new(
            &plan,
            UpOptions {
                no_prefix: true,
                ..UpOptions::default()
            },
        );
        assert_eq!(printer.prefix(&plan[0]), "");
    }

    #[test]
    fn timestamps_are_only_added_when_requested() {
        let plan = vec![service("api")];
        let plain = Printer::new(&plan, UpOptions::default());
        assert_eq!(plain.timestamp(), "");

        let stamped = Printer::new(
            &plan,
            UpOptions {
                timestamps: true,
                ..UpOptions::default()
            },
        );
        let stamp = stamped.timestamp();
        assert_eq!(stamp.trim().len(), 8, "expected HH:MM:SS, got {stamp:?}");
    }
}
