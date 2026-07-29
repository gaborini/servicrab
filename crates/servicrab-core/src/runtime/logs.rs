//! Writing service output to rotating log files.
//!
//! File logging is opt-in: it is active only when the config declares a
//! `[project.logs]` table.  Each service gets `<dir>/<service>.log`; when a
//! file grows past `max_size` it is rotated to `<service>.log.1`, the previous
//! `.1` becomes `.2`, and anything beyond `max_files` is deleted.
//!
//! The writer is deliberately synchronous and line-oriented: supervised
//! services produce modest volumes, and flushing every line means the log on
//! disk is always current — including right after a crash.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::{LogSettings, ServiceName};

/// A rotating, append-only log file for one service.
#[derive(Debug)]
pub struct LogWriter {
    path: PathBuf,
    file: File,
    size: u64,
    max_size: u64,
    max_files: u32,
}

impl LogWriter {
    /// Open (or create) the log file for `service`.
    ///
    /// The parent directory is created when missing.
    pub fn open(settings: &LogSettings, service: &ServiceName) -> std::io::Result<Self> {
        fs::create_dir_all(&settings.dir)?;
        let path = settings.file_for(service);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            file,
            size,
            max_size: settings.max_size,
            max_files: settings.max_files,
        })
    }

    /// Append one line, rotating first when the line would not fit.
    pub fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let needed = line.len() as u64 + 1;
        // Rotating before the write keeps whole lines together, so a log file
        // never ends mid-line.
        if self.size > 0 && self.size + needed > self.max_size {
            self.rotate()?;
        }
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        self.size += needed;
        Ok(())
    }

    /// Rotate the current file out of the way and start a fresh one.
    fn rotate(&mut self) -> std::io::Result<()> {
        if self.max_files == 0 {
            // No history is kept: start over from an empty file.
            self.file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&self.path)?;
            self.size = 0;
            return Ok(());
        }

        let rotated = |n: u32| rotated_path(&self.path, n);

        // The oldest file falls off the end.
        let oldest = rotated(self.max_files);
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }
        for n in (1..self.max_files).rev() {
            let from = rotated(n);
            if from.exists() {
                fs::rename(&from, rotated(n + 1))?;
            }
        }
        fs::rename(&self.path, rotated(1))?;

        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.size = 0;
        Ok(())
    }

    /// Path of the file currently being written.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `<path>.<n>` for rotated files.
fn rotated_path(path: &Path, n: u32) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Fans service output out to one [`LogWriter`] per service.
///
/// Writers are opened lazily, so a service that never produces output does not
/// leave an empty file behind.
#[derive(Debug)]
pub struct LogRouter {
    settings: LogSettings,
    writers: BTreeMap<ServiceName, LogWriter>,
    /// Services that opted out of file logging.
    excluded: Vec<ServiceName>,
    /// Reported once, so a broken log directory does not spam the terminal.
    reported_error: bool,
}

impl LogRouter {
    /// Build a router for the given settings.
    pub fn new(settings: LogSettings, excluded: Vec<ServiceName>) -> Self {
        Self {
            settings,
            writers: BTreeMap::new(),
            excluded,
            reported_error: false,
        }
    }

    /// Record one output line for `service`.
    ///
    /// Returns an error message the first time writing fails; later failures
    /// are silent so a full disk cannot drown out the service's own output.
    pub fn record(&mut self, service: &ServiceName, line: &str) -> Option<String> {
        if self.excluded.contains(service) {
            return None;
        }

        if !self.writers.contains_key(service) {
            match LogWriter::open(&self.settings, service) {
                Ok(writer) => {
                    self.writers.insert(service.clone(), writer);
                }
                Err(err) => return self.report(format!("could not open a log file: {err}")),
            }
        }

        let writer = self.writers.get_mut(service).expect("writer just inserted");
        let failure = writer
            .write_line(line)
            .err()
            .map(|err| format!("could not write to {}: {err}", writer.path.display()));
        match failure {
            Some(message) => self.report(message),
            None => None,
        }
    }

    fn report(&mut self, message: String) -> Option<String> {
        if self.reported_error {
            return None;
        }
        self.reported_error = true;
        Some(message)
    }

    /// Whether this router writes a log file for `service`.
    pub fn handles(&self, service: &ServiceName) -> bool {
        !self.excluded.contains(service)
    }

    /// The settings this router writes with.
    pub fn settings(&self) -> &LogSettings {
        &self.settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn settings(dir: &Path, max_size: u64, max_files: u32) -> LogSettings {
        LogSettings {
            dir: dir.to_path_buf(),
            max_size,
            max_files,
        }
    }

    fn service(name: &str) -> ServiceName {
        crate::validation::validate_service_name(name).expect("valid name")
    }

    #[test]
    fn lines_are_appended_and_flushed() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 1024, 3);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        writer.write_line("first").unwrap();
        writer.write_line("second").unwrap();

        let contents = fs::read_to_string(settings.file_for(&name)).unwrap();
        assert_eq!(contents, "first\nsecond\n");
    }

    #[test]
    fn an_existing_log_is_appended_to() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 1024, 3);
        let name = service("api");
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(settings.file_for(&name), "old\n").unwrap();

        let mut writer = LogWriter::open(&settings, &name).unwrap();
        writer.write_line("new").unwrap();

        let contents = fs::read_to_string(settings.file_for(&name)).unwrap();
        assert_eq!(contents, "old\nnew\n");
    }

    #[test]
    fn the_file_rotates_once_it_is_full() {
        let dir = TempDir::new().unwrap();
        // Room for exactly one 8-byte line ("1234567\n").
        let settings = settings(dir.path(), 8, 2);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        writer.write_line("1234567").unwrap();
        writer.write_line("abcdefg").unwrap();

        assert_eq!(
            fs::read_to_string(settings.file_for(&name)).unwrap(),
            "abcdefg\n"
        );
        assert_eq!(
            fs::read_to_string(settings.rotated_file_for(&name, 1)).unwrap(),
            "1234567\n"
        );
    }

    #[test]
    fn rotated_files_are_shifted_and_dropped() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 8, 2);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        for line in ["aaaaaaa", "bbbbbbb", "ccccccc", "ddddddd"] {
            writer.write_line(line).unwrap();
        }

        assert_eq!(
            fs::read_to_string(settings.file_for(&name)).unwrap(),
            "ddddddd\n"
        );
        assert_eq!(
            fs::read_to_string(settings.rotated_file_for(&name, 1)).unwrap(),
            "ccccccc\n"
        );
        assert_eq!(
            fs::read_to_string(settings.rotated_file_for(&name, 2)).unwrap(),
            "bbbbbbb\n"
        );
        assert!(
            !settings.rotated_file_for(&name, 3).exists(),
            "history beyond max_files must be deleted"
        );
    }

    #[test]
    fn max_files_zero_truncates_instead_of_keeping_history() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 8, 0);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        writer.write_line("1234567").unwrap();
        writer.write_line("abcdefg").unwrap();

        assert_eq!(
            fs::read_to_string(settings.file_for(&name)).unwrap(),
            "abcdefg\n"
        );
        assert!(!settings.rotated_file_for(&name, 1).exists());
    }

    #[test]
    fn a_line_longer_than_the_limit_is_still_written_whole() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 8, 1);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        writer
            .write_line("this line is definitely too long")
            .unwrap();

        assert_eq!(
            fs::read_to_string(settings.file_for(&name)).unwrap(),
            "this line is definitely too long\n"
        );
    }

    #[test]
    fn the_router_writes_one_file_per_service() {
        let dir = TempDir::new().unwrap();
        let mut router = LogRouter::new(settings(dir.path(), 1024, 3), Vec::new());

        assert!(router.record(&service("api"), "from api").is_none());
        assert!(router.record(&service("db"), "from db").is_none());

        assert_eq!(
            fs::read_to_string(dir.path().join("api.log")).unwrap(),
            "from api\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("db.log")).unwrap(),
            "from db\n"
        );
    }

    #[test]
    fn excluded_services_are_not_written_to_disk() {
        let dir = TempDir::new().unwrap();
        let mut router = LogRouter::new(settings(dir.path(), 1024, 3), vec![service("quiet")]);

        router.record(&service("quiet"), "nothing to see");

        assert!(!dir.path().join("quiet.log").exists());
    }

    #[test]
    fn a_write_failure_is_reported_only_once() {
        let dir = TempDir::new().unwrap();
        // A file where the directory should be makes every open fail.
        let blocked = dir.path().join("logs");
        fs::write(&blocked, "not a directory").unwrap();
        let mut router = LogRouter::new(settings(&blocked, 1024, 3), Vec::new());

        assert!(router.record(&service("api"), "one").is_some());
        assert!(router.record(&service("api"), "two").is_none());
    }
}
