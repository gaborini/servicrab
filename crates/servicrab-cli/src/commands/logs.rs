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
        let tail = read_tail(&file, options.lines, options.follow)?;
        sources.push(Source {
            service: name.clone(),
            path: file,
            offset: tail.end,
            lines: tail.lines,
        });
    }

    // An empty log directory is what every stack looks like before it has run,
    // so this is a state of the world to report rather than a way for the
    // command to fail: `servicrab logs && echo ok` should not stop here.
    if sources.iter().all(|s| s.lines.is_empty()) && !options.follow {
        eprintln!(
            "no log output yet in {} — start the stack with `servicrab up` first",
            settings.dir.display()
        );
        return Ok(());
    }

    let printer = Printer::new(&selected, options.no_prefix);
    // Interleaving files by line count is meaningless, so each service's tail
    // is printed as a block; following then merges new lines as they arrive.
    for source in &mut sources {
        for line in std::mem::take(&mut source.lines) {
            printer.line(&source.service, &line);
        }
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
    /// Where reading stopped: the byte just past the last complete line that
    /// was printed.  It is the position a read actually reached, never a
    /// separately sampled file length — sampling the length and then reading
    /// are two different moments, and a log file grows in between.
    offset: u64,
    lines: Vec<String>,
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// What one read of a log file yielded.
struct Chunk {
    /// The complete lines, newline stripped.
    lines: Vec<String>,
    /// The byte just past the last complete line.
    ///
    /// A line still being written stays unread, so the next read picks it up
    /// once its newline has arrived and prints it once, whole.
    end: u64,
    /// The unterminated remainder, if the file ended mid-line.
    partial: Option<String>,
}

/// Read every complete line from `offset` to the end of the file.
///
/// Log files hold whatever the services printed, and a service is free to print
/// a stray byte that is not UTF-8 — a binary blob, a half-written multi-byte
/// character, output in another encoding.  That is not a reason to refuse to
/// show the log, so the undecodable bytes are replaced and the rest is
/// readable.
fn read_lines(path: &Path, offset: u64) -> Result<Chunk, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;

    let mut reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut end = offset;
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if read == 0 {
            return Ok(Chunk {
                lines,
                end,
                partial: None,
            });
        }
        if buffer.last() != Some(&b'\n') {
            return Ok(Chunk {
                lines,
                end,
                partial: Some(decode(&buffer)),
            });
        }
        end += read as u64;
        lines.push(decode(&buffer));
    }
}

/// Turn one raw line into text, dropping the line ending.
fn decode(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    text.strip_suffix('\r').unwrap_or(text).to_string()
}

/// The last `count` lines of a file, and where they end.
struct Tail {
    lines: Vec<String>,
    end: u64,
}

/// Read the last `count` lines of a file, tolerating a missing file.
///
/// A file that ends mid-line is the normal state of a log a service is writing
/// to.  Without `--follow` the half line is all there will ever be to show, so
/// it is shown; with it, the line is left for the pass that finds it finished,
/// which is the only way to print it once rather than twice.
fn read_tail(path: &Path, count: usize, follow: bool) -> Result<Tail, String> {
    if !path.exists() {
        return Ok(Tail {
            lines: Vec::new(),
            end: 0,
        });
    }
    let chunk = read_lines(path, 0)?;

    let mut lines = chunk.lines;
    if !follow {
        lines.extend(chunk.partial);
    }
    if count == 0 {
        lines.clear();
    } else if lines.len() > count {
        lines.drain(..lines.len() - count);
    }
    Ok(Tail {
        lines,
        end: chunk.end,
    })
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

            let Ok(chunk) = read_lines(&source.path, source.offset) else {
                continue;
            };
            for line in &chunk.lines {
                printer.line(&source.service, line);
                printed = true;
            }
            source.offset = chunk.end;
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
            color: style::color_enabled_for(style::Stream::Stdout),
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

    fn tail(path: &Path, count: usize) -> Vec<String> {
        read_tail(path, count, false).unwrap().lines
    }

    #[test]
    fn the_tail_of_a_missing_file_is_empty() {
        let dir = TempDir::new().unwrap();
        assert!(tail(&dir.path().join("nope.log"), 10).is_empty());
    }

    #[test]
    fn only_the_last_lines_are_returned() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("api.log");
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

        assert_eq!(tail(&path, 2), vec!["three", "four"]);
        assert_eq!(tail(&path, 10), vec!["one", "two", "three", "four"]);
    }

    #[test]
    fn zero_lines_returns_nothing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("api.log");
        std::fs::write(&path, "one\ntwo\n").unwrap();

        assert!(tail(&path, 0).is_empty());
    }

    #[test]
    fn a_line_that_is_not_utf8_is_shown_with_replacements_rather_than_refused() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("api.log");
        std::fs::write(&path, b"before\n\xffbad\nafter\n").unwrap();

        let lines = tail(&path, 10);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0], "before");
        assert_eq!(lines[1], "\u{fffd}bad");
        assert_eq!(lines[2], "after");
    }

    #[test]
    fn a_read_stops_after_the_last_complete_line() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("api.log");
        std::fs::write(&path, "done\nhalf").unwrap();

        let chunk = read_lines(&path, 0).unwrap();
        assert_eq!(chunk.lines, vec!["done"]);
        assert_eq!(chunk.partial.as_deref(), Some("half"));
        // Just past "done\n", so the half line is read again once it is whole.
        assert_eq!(chunk.end, 5);
    }

    #[test]
    fn a_half_written_line_is_left_for_the_next_pass_while_following() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("api.log");
        std::fs::write(&path, "done\nhalf").unwrap();

        let followed = read_tail(&path, 10, true).unwrap();
        assert_eq!(followed.lines, vec!["done"]);
        assert_eq!(followed.end, 5);

        // Without --follow there is no next pass, so the fragment is all there
        // will be to show and it is shown.
        let once = read_tail(&path, 10, false).unwrap();
        assert_eq!(once.lines, vec!["done", "half"]);
    }

    #[test]
    fn following_resumes_where_the_last_read_stopped_even_when_the_file_grew() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("api.log");
        std::fs::write(&path, "one\n").unwrap();

        let first = read_lines(&path, 0).unwrap();
        assert_eq!(first.lines, vec!["one"]);

        // What an append between the sample and the read used to cost: the old
        // code rewound to a length taken before reading, so everything appended
        // meanwhile came out a second time.
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let second = read_lines(&path, first.end).unwrap();
        assert_eq!(second.lines, vec!["two"]);

        let third = read_lines(&path, second.end).unwrap();
        assert!(third.lines.is_empty(), "{:?}", third.lines);
    }

    #[test]
    fn a_windows_line_ending_is_not_shown_as_a_stray_carriage_return() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("api.log");
        std::fs::write(&path, "one\r\n").unwrap();

        assert_eq!(tail(&path, 10), vec!["one"]);
    }
}
