//! `servicrab events [SERVICE...]` — follow a running daemon's event stream.
//!
//! This is `up`'s renderer without the supervision: the daemon owns the
//! processes, and this command only attaches to the fan-out socket and prints
//! what arrives until the user interrupts it.

use std::path::Path;

/// Command-line options for `events`.
#[derive(Debug, Clone, Copy, Default)]
pub struct EventsOptions {
    /// Print one JSON object per line instead of rendering for a terminal.
    pub json: bool,
    /// Do not prefix output lines with the service name.
    pub no_prefix: bool,
    /// Prefix output lines with a UTC timestamp.
    pub timestamps: bool,
    /// Leave captured stdout/stderr out of the stream.
    pub no_logs: bool,
}

#[cfg(unix)]
mod imp {
    use super::*;

    use std::collections::BTreeMap;
    use std::io::Write;

    use servicrab_protocol::{Event, Request, Response, Stream};

    use crate::daemon::client;
    use crate::style::{self, BOLD, DIM, RESET, SERVICE_COLORS};

    /// Attach to the daemon and render its events.
    pub fn events(
        services: &[String],
        config: Option<&Path>,
        options: EventsOptions,
    ) -> Result<i32, String> {
        let (cfg, _, paths) = crate::commands::daemon::setup(config)?;

        for name in services {
            if !cfg.services.keys().any(|known| known.as_str() == name) {
                return Err(format!("unknown service: {name}"));
            }
        }

        let request = Request::Subscribe {
            services: services.iter().cloned().collect(),
            logs: !options.no_logs,
        };

        let mut printer = Printer::new(services, options);
        if !options.json {
            printer.banner(cfg.project.name.as_str(), services);
        }

        match client::subscribe(&paths.socket, &request, |raw, response| {
            if options.json {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{raw}");
                let _ = out.flush();
                return true;
            }
            printer.response(&response);
            true
        }) {
            Ok(()) => Ok(0),
            Err(client::ClientError::NotRunning) => Err(format!(
                "no daemon is running for {} — start one with `servicrab start`",
                cfg.project.name
            )),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Renders streamed events for a terminal.
    struct Printer {
        colors: BTreeMap<String, &'static str>,
        width: usize,
        color: bool,
        options: EventsOptions,
    }

    impl Printer {
        fn new(services: &[String], options: EventsOptions) -> Self {
            let color = style::color_enabled();
            let width = services.iter().map(String::len).max().unwrap_or(0);
            let colors = services
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

        fn banner(&self, project: &str, services: &[String]) {
            let scope = if services.is_empty() {
                "all services".to_string()
            } else {
                services.join(", ")
            };
            eprintln!(
                "{} {project} → {scope}",
                style::paint(self.color, BOLD, "servicrab events")
            );
        }

        /// `api    | ` (or an empty string when prefixes are disabled).
        ///
        /// Colours are handed out as services first show up, so a stream that
        /// was not filtered still gets stable, distinct prefixes.
        fn prefix(&mut self, service: &str) -> String {
            if self.options.no_prefix {
                return String::new();
            }
            let next = SERVICE_COLORS[self.colors.len() % SERVICE_COLORS.len()];
            let color = *self
                .colors
                .entry(service.to_string())
                .or_insert_with(|| next);
            self.width = self.width.max(service.len());
            let label = format!("{:width$}", service, width = self.width);
            format!("{} {} ", style::paint(self.color, color, &label), "|")
        }

        fn timestamp(&self) -> String {
            if !self.options.timestamps {
                return String::new();
            }
            let now = style::utc_hms(std::time::SystemTime::now());
            format!("{} ", style::paint(self.color, DIM, &now))
        }

        fn response(&mut self, response: &Response) {
            match response {
                Response::Event { service, event } => self.event(service, event),
                Response::Lagged { skipped } => {
                    self.note(&format!("dropped {skipped} event(s): too slow to keep up"))
                }
                Response::Error { message } => self.note(message),
                _ => {}
            }
        }

        fn event(&mut self, service: &str, event: &Event) {
            match event {
                Event::Log { stream, line } => self.log(service, *stream, line),
                Event::Started { pgid } => {
                    self.status(service, "▶", &format!("started (pgid {pgid})"))
                }
                Event::Exited {
                    reason, uptime_ms, ..
                } => self.status(
                    service,
                    "■",
                    &format!("{reason} after {}", humanize(*uptime_ms)),
                ),
                Event::Backoff { delay_ms, attempt } => self.status(
                    service,
                    "↻",
                    &format!("restarting in {} (attempt {attempt})", humanize(*delay_ms)),
                ),
                Event::Stopping { reason } => {
                    self.status(service, "◼", &format!("stopping: {reason}"))
                }
                Event::Skipped { dependency } => self.status(
                    service,
                    "⊘",
                    &format!("skipped: dependency {dependency} never became available"),
                ),
                Event::Failed { message } => self.status(service, "✗", message),
                Event::Healthy => self.status(service, "✔", "healthy"),
                Event::HealthProbeFailed {
                    message,
                    consecutive,
                    retries,
                } => self.status(
                    service,
                    "!",
                    &format!("health probe failed ({consecutive}/{retries}): {message}"),
                ),
                Event::Unhealthy { message } => {
                    self.status(service, "✗", &format!("unhealthy: {message}"))
                }
                Event::WatchTriggered { path, changed } => {
                    let name = Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone());
                    let detail = if *changed == 1 {
                        format!("{name} changed")
                    } else {
                        format!("{name} and {} more changed", changed - 1)
                    };
                    self.status(service, "↻", &format!("{detail}; restarting"))
                }
                Event::WatchFailed { message } => {
                    self.status(service, "!", &format!("watch: {message}"))
                }
                Event::WatchTruncated { limit } => self.status(
                    service,
                    "!",
                    &format!(
                        "watch: more than {limit} files; narrow `paths` or add `ignore` entries"
                    ),
                ),
                Event::LogLinesDropped { count } => self.status(
                    service,
                    "!",
                    &format!("dropped {count} output line(s): faster than they could be consumed"),
                ),
                // Unlike `up`, a client that just attached has no idea what
                // the services are doing, so state changes are worth showing.
                Event::State { state } => self.status(service, "·", &state.to_string()),
                Event::Finished { summary } => self.status(service, "□", summary),
                _ => {}
            }
        }

        fn log(&mut self, service: &str, stream: Stream, line: &str) {
            let text = format!("{}{}{}", self.timestamp(), self.prefix(service), line);
            match stream {
                Stream::Stderr => {
                    let mut err = std::io::stderr().lock();
                    let _ = writeln!(err, "{text}");
                    let _ = err.flush();
                }
                _ => {
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "{text}");
                    let _ = out.flush();
                }
            }
        }

        fn status(&mut self, service: &str, symbol: &str, message: &str) {
            let text = format!(
                "{}{}{}",
                self.timestamp(),
                self.prefix(service),
                style::paint(self.color, DIM, &format!("{symbol} {message}"))
            );
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{text}");
            let _ = err.flush();
        }

        /// Report something about the stream itself, not about a service.
        fn note(&self, message: &str) {
            eprintln!("{}⚠  {message}{}", if self.color { DIM } else { "" }, {
                if self.color {
                    RESET
                } else {
                    ""
                }
            });
        }
    }

    /// Render a millisecond duration the way a human would say it.
    fn humanize(millis: u64) -> String {
        let secs = millis / 1000;
        if secs >= 60 {
            return format!("{}m{}s", secs / 60, secs % 60);
        }
        if secs > 0 {
            return format!("{secs}s");
        }
        format!("{millis}ms")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn durations_read_naturally() {
            assert_eq!(humanize(250), "250ms");
            assert_eq!(humanize(1_500), "1s");
            assert_eq!(humanize(90_000), "1m30s");
        }

        #[test]
        fn prefixes_are_padded_and_stable() {
            let mut printer = Printer::new(&[], EventsOptions::default());
            let first = printer.prefix("api");
            let second = printer.prefix("worker");
            assert!(first.contains("api"));
            assert!(second.contains("worker"));
            // The second, longer name widens the column for later lines.
            assert!(printer.prefix("api").contains("api   "));
            // A name keeps the colour it was first given.
            assert_eq!(printer.colors["api"], SERVICE_COLORS[0]);
            assert_eq!(printer.colors["worker"], SERVICE_COLORS[1]);
        }

        #[test]
        fn prefixes_can_be_disabled() {
            let mut printer = Printer::new(
                &[],
                EventsOptions {
                    no_prefix: true,
                    ..EventsOptions::default()
                },
            );
            assert_eq!(printer.prefix("api"), "");
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    pub fn events(
        _services: &[String],
        _config: Option<&Path>,
        _options: EventsOptions,
    ) -> Result<i32, String> {
        Err("the background daemon is only supported on Linux and macOS".to_string())
    }
}

pub use imp::events;
