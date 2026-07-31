//! `servicrab logs [SERVICE...]` — read the captured service log files.
//!
//! Log files only exist when the config declares a `[project.logs]` table;
//! without it this command explains how to turn file logging on rather than
//! silently printing nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use servicrab_core::{load, resolve_config_path, Config, LogRouter, ServiceName};

use crate::style::{self, SERVICE_COLORS};

/// How long to sleep between polls while following.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Command-line options for `logs`.
#[derive(Debug, Clone, Copy)]
pub struct LogsOptions {
    /// Keep printing new lines as they are written.
    pub follow: bool,
    /// How many trailing lines to show per service.
    pub lines: usize,
    /// Do not prefix output lines with the service name.
    pub no_prefix: bool,
}

/// Build a [`LogRouter`] for a config, when file logging is enabled.
///
/// Services that set `[services.<name>.logs] enabled = false` are excluded.
pub fn router_for(cfg: &Config) -> Option<LogRouter> {
    let settings = cfg.project.logs.clone()?;
    let excluded: BTreeSet<ServiceName> = cfg
        .services
        .values()
        .filter(|svc| !svc.log_to_file)
        .map(|svc| svc.name.clone())
        .collect();
    Some(LogRouter::new(settings, excluded))
}

/// Run the `logs` subcommand.
pub fn run(services: &[String], config: Option<&Path>, options: LogsOptions) -> Result<(), String> {
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

    let Some(settings) = cfg.project.logs.clone() else {
        return Err(format!(
            "file logging is disabled: add a [project.logs] section to {} to capture service output",
            path.display()
        ));
    };

    let selected = select_services(&cfg, services)?;
    let mut sources: Vec<Source> = Vec::new();
    for name in &selected {
        let file = settings.file_for(name);
        let tail = read_tail(&file, options.lines)?;
        sources.push(Source {
            service: name.clone(),
            path: file,
            offset: 0,
            tail,
        });
    }

    if sources.iter().all(|s| s.tail.is_empty()) && !options.follow {
        return Err(format!(
            "no log output yet in {} — start the stack with `servicrab up` first",
            settings.dir.display()
        ));
    }

    let printer = Printer::new(&selected, options.no_prefix);
    // Interleaving files by line count is meaningless, so each service's tail
    // is printed as a block; following then merges new lines as they arrive.
    for source in &mut sources {
        for line in std::mem::take(&mut source.tail) {
            printer.line(&source.service, &line);
        }
        source.offset = file_len(&source.path);
    }

    if options.follow {
        follow(&mut sources, &printer)?;
    }
    Ok(())
}

/// Resolve the requested service names, defaulting to every service that logs.
fn select_services(cfg: &Config, requested: &[String]) -> Result<Vec<ServiceName>, String> {
    if requested.is_empty() {
        let all: Vec<ServiceName> = cfg
            .services
            .values()
            .filter(|svc| svc.log_to_file)
            .map(|svc| svc.name.clone())
            .collect();
        if all.is_empty() {
            return Err("every service has [logs] enabled = false".to_string());
        }
        return Ok(all);
    }

    let known: Vec<&str> = cfg.services.keys().map(|n| n.as_str()).collect();
    let mut selected = Vec::new();
    for name in requested {
        let Some(svc) = cfg.services.values().find(|s| s.name.as_str() == name) else {
            return Err(format!(
                "unknown service {name:?}; known services: {}",
                known.join(", ")
            ));
        };
        if !svc.log_to_file {
            return Err(format!(
                "service {name:?} has [logs] enabled = false, so it has no log file"
            ));
        }
        selected.push(svc.name.clone());
    }
    Ok(selected)
}

/// One followed log file.
struct Source {
    service: ServiceName,
    path: std::path::PathBuf,
    offset: u64,
    tail: Vec<String>,
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Read the last `count` lines of a file, tolerating a missing file.
fn read_tail(path: &Path, count: usize) -> Result<Vec<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("could not read {}: {err}", path.display())),
    };

    let mut lines: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if count > 0 && lines.len() == count {
            lines.pop_front();
        }
        if count > 0 {
            lines.push_back(line);
        }
    }
    Ok(lines.into())
}

/// Print new lines as they are appended, until interrupted.
fn follow(sources: &mut [Source], printer: &Printer) -> Result<(), String> {
    loop {
        let mut printed = false;
        for source in sources.iter_mut() {
            let len = file_len(&source.path);
            // A shrinking file means the log was rotated; start over from the
            // beginning of the fresh file so nothing is skipped.
            if len < source.offset {
                source.offset = 0;
            }
            if len == source.offset {
                continue;
            }

            let mut file = match std::fs::File::open(&source.path) {
                Ok(file) => file,
                Err(_) => continue,
            };
            if file.seek(SeekFrom::Start(source.offset)).is_err() {
                continue;
            }
            for line in BufReader::new(&mut file).lines() {
                let Ok(line) = line else { break };
                printer.line(&source.service, &line);
                printed = true;
            }
            source.offset = len;
        }

        if !printed {
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

/// Renders log lines with optional per-service prefixes.
struct Printer {
    colors: BTreeMap<ServiceName, &'static str>,
    width: usize,
    color: bool,
    no_prefix: bool,
}

impl Printer {
    fn new(services: &[ServiceName], no_prefix: bool) -> Self {
        let colors = services
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), SERVICE_COLORS[i % SERVICE_COLORS.len()]))
            .collect();
        let width = services
            .iter()
            .map(|n| n.as_str().len())
            .max()
            .unwrap_or(0)
            .min(20);
        Self {
            colors,
            width,
            color: style::color_enabled(),
            // A single service needs no prefix to tell its lines apart.
            no_prefix: no_prefix || services.len() < 2,
        }
    }

    fn line(&self, service: &ServiceName, line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = if self.no_prefix {
            writeln!(out, "{line}")
        } else {
            let color = self.colors.get(service).copied().unwrap_or(style::RESET);
            let label = format!("{:width$}", service.as_str(), width = self.width);
            writeln!(out, "{} | {line}", style::paint(self.color, color, &label))
        };
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn the_tail_of_a_missing_file_is_empty() {
        let dir = TempDir::new().unwrap();
        assert!(read_tail(&dir.path().join("nope.log"), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn only_the_last_lines_are_returned() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("api.log");
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

        assert_eq!(read_tail(&path, 2).unwrap(), vec!["three", "four"]);
        assert_eq!(
            read_tail(&path, 10).unwrap(),
            vec!["one", "two", "three", "four"]
        );
    }

    #[test]
    fn zero_lines_returns_nothing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("api.log");
        std::fs::write(&path, "one\ntwo\n").unwrap();

        assert!(read_tail(&path, 0).unwrap().is_empty());
    }
}
