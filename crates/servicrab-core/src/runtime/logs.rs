//! Writing service output to rotating log files.
//!
//! File logging is opt-in: it is active only when the config declares a
//! `[project.logs]` table.  Each service gets `<dir>/<service>.log`; when a
//! file grows past `max_size` it is rotated to `<service>.log.1`, the previous
//! `.1` becomes `.2`, and anything beyond `max_files` is deleted.
//!
//! The writer is line-oriented and synchronous, but it never runs on an async
//! worker: [`LogSink`] hands the lines to a blocking task.  `create_dir_all`,
//! `write_all` and `flush` can stall for a long time on a full or network-backed
//! disk, and a worker stalled here is a worker not driving child `wait()`s,
//! health probes or the control channel.
//!
//! Flushing is batched rather than per line: the writer flushes as soon as it has
//! caught up with the queue — which for the modest volumes a supervised service
//! produces is still after every single line — and, while a flood keeps the queue
//! full, at least every [`FLUSH_EVERY_LINES`] lines.  Nothing is left in the
//! buffer when the writer stops: it flushes when the queue ends and again when the
//! file is dropped.
//!
//! Handing a line over never waits.  A file system that cannot keep up with a
//! service's output at all costs log lines, loudly, rather than costing the
//! supervisor its responsiveness.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::config::{LogSettings, ServiceName};

/// Flush at least this often while a flood keeps the queue from draining.
///
/// Without a cap a service that never stops printing would never reach the
/// "caught up, flush now" branch, and a crash would lose everything still
/// buffered.
const FLUSH_EVERY_LINES: usize = 256;

/// How many lines may wait for the blocking writer.
///
/// Reaching this bound means the file system is not keeping up with a service's
/// output at all, and the line is dropped rather than made to wait: the callers
/// of [`LogSink::record`] are the pumps that also keep the status registry
/// current and feed the event stream, so making *them* wait for the disk is the
/// very thing this module exists to avoid.
const QUEUE_CAPACITY: usize = 4096;

/// A rotating, append-only log file for one service.
#[derive(Debug)]
pub struct LogWriter {
    path: PathBuf,
    file: BufWriter<File>,
    size: u64,
    max_size: u64,
    max_files: u32,
    /// Lines written since the last flush, so a flood still reaches the disk.
    unflushed: usize,
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
            file: BufWriter::new(file),
            size,
            max_size: settings.max_size,
            max_files: settings.max_files,
            unflushed: 0,
        })
    }

    /// Append one line, rotating first when the line would not fit.
    ///
    /// The line is buffered, not necessarily on disk yet; [`LogWriter::flush`]
    /// is what makes it durable, and [`Drop`] is the backstop.
    pub fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let needed = line.len() as u64 + 1;
        // Rotating before the write keeps whole lines together, so a log file
        // never ends mid-line.
        if self.size > 0 && self.size + needed > self.max_size {
            self.rotate()?;
        }
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.size += needed;
        self.unflushed += 1;
        // A flood never lets the writer catch up with its queue, so cap how
        // much can be in flight regardless.
        if self.unflushed >= FLUSH_EVERY_LINES {
            self.flush()?;
        }
        Ok(())
    }

    /// Push everything buffered out to the file.
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.unflushed = 0;
        self.file.flush()
    }

    /// Rotate the current file out of the way and start a fresh one.
    fn rotate(&mut self) -> std::io::Result<()> {
        // Whatever is still buffered belongs to the file being rotated away,
        // so it has to land before the rename.
        self.flush()?;

        if self.max_files == 0 {
            // No history is kept: start over from an empty file.
            self.file = BufWriter::new(
                OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(&self.path)?,
            );
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

        self.file = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.size = 0;
        Ok(())
    }

    /// Path of the file currently being written.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        // `BufWriter` flushes on drop too, but it swallows the result and only
        // does so for the buffer it currently owns; being explicit keeps the
        // "no line is lost when the writer goes away" promise where it can be
        // read.
        let _ = self.file.flush();
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
    /// Services that opted out of file logging.  A set, not a list: this is
    /// consulted for every single log line.
    excluded: BTreeSet<ServiceName>,
    /// Reported once, so a broken log directory does not spam the terminal.
    reported_error: bool,
}

impl LogRouter {
    /// Build a router for the given settings.
    pub fn new(settings: LogSettings, excluded: BTreeSet<ServiceName>) -> Self {
        Self {
            settings,
            writers: BTreeMap::new(),
            excluded,
            reported_error: false,
        }
    }

    /// Record one output line for `service`.
    ///
    /// The line is buffered; [`LogRouter::flush`] is what puts it on disk.
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

    /// Put every buffered line on disk.
    ///
    /// Reported the same way [`LogRouter::record`] reports: once.
    pub fn flush(&mut self) -> Option<String> {
        let mut failure = None;
        for writer in self.writers.values_mut() {
            if let Err(err) = writer.flush() {
                failure = failure.or_else(|| {
                    Some(format!(
                        "could not write to {}: {err}",
                        writer.path.display()
                    ))
                });
            }
        }
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

/// One line on its way to the writer.
#[derive(Debug)]
struct Entry {
    service: ServiceName,
    line: String,
}

/// A [`LogRouter`] driven from a blocking task.
///
/// The async pumps that consume captured output hand their lines here instead of
/// writing files themselves, so a slow disk delays the log rather than the
/// runtime.  Ordering per service is preserved: one queue, one writer, first in
/// first out.
///
/// [`LogSink::record`] never waits.  If the writer falls [`QUEUE_CAPACITY`]
/// lines behind, the line is dropped and said to be dropped: a supervisor that
/// stops answering its control channel because a log file is slow is worse than
/// a log with a hole in it.
#[derive(Debug)]
pub struct LogSink {
    entries: mpsc::Sender<Entry>,
    writer: tokio::task::JoinHandle<()>,
    problem: Arc<Mutex<Option<String>>>,
    /// Lines the writer was too far behind to accept.
    dropped: AtomicU64,
}

impl LogSink {
    /// Start the blocking writer for `router`.
    ///
    /// Must be called from within a Tokio runtime.
    pub fn spawn(router: LogRouter) -> Self {
        let (entries, queue) = mpsc::channel(QUEUE_CAPACITY);
        let problem = Arc::new(Mutex::new(None));
        let writer = tokio::task::spawn_blocking({
            let problem = Arc::clone(&problem);
            move || pump(queue, router, &problem)
        });
        Self {
            entries,
            writer,
            problem,
            dropped: AtomicU64::new(0),
        }
    }

    /// Hand one line to the writer, dropping it if the writer is too far behind.
    ///
    /// Returns something worth telling the operator: the first failure the
    /// writer ran into, or how far behind it has fallen.  The report is
    /// necessarily one-sided in time — the writer is asynchronous now, so a
    /// failure surfaces on a later call rather than on the one that caused it.
    pub fn record(&self, service: &ServiceName, line: &str) -> Option<String> {
        let entry = Entry {
            service: service.clone(),
            line: line.to_string(),
        };
        match self.entries.try_send(entry) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                let dropped = self.dropped.fetch_add(1, Ordering::AcqRel) + 1;
                // The first one, then one report per queue's worth: a disk that
                // stays stuck must not turn into its own flood of warnings.
                if dropped == 1 || dropped % QUEUE_CAPACITY as u64 == 0 {
                    return Some(format!(
                        "the log writer cannot keep up; dropped {dropped} line(s) so far"
                    ));
                }
            }
            // A closed queue means the writer task is gone, which only happens
            // when the runtime is shutting down; not worth a diagnostic.
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
        self.take_problem()
    }

    /// Stop the writer once every queued line has been written and flushed.
    pub async fn shutdown(self) -> Option<String> {
        let Self {
            entries,
            writer,
            problem,
            ..
        } = self;
        drop(entries);
        // The writer drains what is queued, flushes, and returns; it never
        // blocks on anything but the file system.
        let _ = writer.await;
        problem.lock().ok().and_then(|mut slot| slot.take())
    }

    fn take_problem(&self) -> Option<String> {
        self.problem.lock().ok().and_then(|mut slot| slot.take())
    }
}

/// Write queued lines until the queue ends, flushing whenever it runs dry.
fn pump(mut queue: mpsc::Receiver<Entry>, mut router: LogRouter, problem: &Mutex<Option<String>>) {
    loop {
        let next = match queue.try_recv() {
            Ok(entry) => Some(entry),
            Err(mpsc::error::TryRecvError::Empty) => {
                // Caught up: get everything on disk before parking, so the log
                // of an ordinarily quiet service is just as current as it was
                // when every line was flushed on its own.
                remember(problem, router.flush());
                queue.blocking_recv()
            }
            Err(mpsc::error::TryRecvError::Disconnected) => None,
        };
        let Some(entry) = next else { break };
        remember(problem, router.record(&entry.service, &entry.line));
    }

    // Nothing more is coming, so this is the last chance to reach the disk.
    remember(problem, router.flush());
}

/// Keep the first problem the writer reports, for the next caller to collect.
fn remember(slot: &Mutex<Option<String>>, message: Option<String>) {
    let Some(message) = message else { return };
    if let Ok(mut slot) = slot.lock() {
        slot.get_or_insert(message);
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
    fn lines_are_appended_in_order_when_flushed() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 1024, 3);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        writer.write_line("first").unwrap();
        writer.write_line("second").unwrap();
        writer.flush().unwrap();

        let contents = fs::read_to_string(settings.file_for(&name)).unwrap();
        assert_eq!(contents, "first\nsecond\n");
    }

    #[test]
    fn writing_a_line_does_not_cost_a_flush() {
        // The batching is the point of the writer: one `flush` per batch
        // instead of one per line is what keeps a slow disk off the hot path.
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 1024, 3);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        writer.write_line("buffered").unwrap();
        assert_eq!(
            fs::read_to_string(settings.file_for(&name)).unwrap(),
            "",
            "the line should still be in the buffer"
        );

        writer.flush().unwrap();
        assert_eq!(
            fs::read_to_string(settings.file_for(&name)).unwrap(),
            "buffered\n"
        );
    }

    #[test]
    fn a_flood_is_flushed_without_being_asked_to() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 1 << 20, 3);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        for i in 0..FLUSH_EVERY_LINES {
            writer.write_line(&format!("line {i}")).unwrap();
        }

        let contents = fs::read_to_string(settings.file_for(&name)).unwrap();
        assert_eq!(
            contents.lines().count(),
            FLUSH_EVERY_LINES,
            "a queue that never drains must still reach the disk"
        );
    }

    #[test]
    fn dropping_the_writer_flushes_what_is_left() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 1024, 3);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        writer.write_line("last words").unwrap();
        drop(writer);

        assert_eq!(
            fs::read_to_string(settings.file_for(&name)).unwrap(),
            "last words\n"
        );
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
        writer.flush().unwrap();

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
        writer.flush().unwrap();

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
    fn rotation_takes_the_buffered_lines_with_the_old_file() {
        // Rotation renames the file underneath the writer, so anything still
        // buffered has to land before the rename or it would reappear at the
        // head of the fresh file.
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), 8, 2);
        let name = service("api");
        let mut writer = LogWriter::open(&settings, &name).unwrap();

        writer.write_line("1234567").unwrap();
        // Not flushed by hand: the rotation the next line triggers must do it.
        writer.write_line("abcdefg").unwrap();
        writer.flush().unwrap();

        assert_eq!(
            fs::read_to_string(settings.rotated_file_for(&name, 1)).unwrap(),
            "1234567\n"
        );
        assert_eq!(
            fs::read_to_string(settings.file_for(&name)).unwrap(),
            "abcdefg\n"
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
        writer.flush().unwrap();

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
        writer.flush().unwrap();

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
        writer.flush().unwrap();

        assert_eq!(
            fs::read_to_string(settings.file_for(&name)).unwrap(),
            "this line is definitely too long\n"
        );
    }

    #[test]
    fn the_router_writes_one_file_per_service() {
        let dir = TempDir::new().unwrap();
        let mut router = LogRouter::new(settings(dir.path(), 1024, 3), BTreeSet::new());

        assert!(router.record(&service("api"), "from api").is_none());
        assert!(router.record(&service("db"), "from db").is_none());
        assert!(router.flush().is_none());

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
        let mut router = LogRouter::new(
            settings(dir.path(), 1024, 3),
            BTreeSet::from([service("quiet")]),
        );

        router.record(&service("quiet"), "nothing to see");

        assert!(!dir.path().join("quiet.log").exists());
    }

    #[test]
    fn a_write_failure_is_reported_only_once() {
        let dir = TempDir::new().unwrap();
        // A file where the directory should be makes every open fail.
        let blocked = dir.path().join("logs");
        fs::write(&blocked, "not a directory").unwrap();
        let mut router = LogRouter::new(settings(&blocked, 1024, 3), BTreeSet::new());

        assert!(router.record(&service("api"), "one").is_some());
        assert!(router.record(&service("api"), "two").is_none());
    }

    #[tokio::test]
    async fn the_sink_writes_every_line_in_order_by_the_time_it_is_shut_down() {
        let dir = TempDir::new().unwrap();
        let name = service("api");
        let sink = LogSink::spawn(LogRouter::new(
            settings(dir.path(), 1 << 20, 3),
            BTreeSet::new(),
        ));

        // Fewer lines than the queue holds, so nothing is dropped and the whole
        // batch has to be on disk once the sink is shut down.
        let total = QUEUE_CAPACITY / 2;
        for i in 0..total {
            sink.record(&name, &format!("line {i}"));
        }
        assert!(sink.shutdown().await.is_none());

        let contents = fs::read_to_string(dir.path().join("api.log")).unwrap();
        let expected: Vec<String> = (0..total).map(|i| format!("line {i}")).collect();
        assert_eq!(contents.lines().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn handing_a_line_over_never_waits_for_the_writer() {
        // The whole point of the sink: the caller is an async pump that also
        // drives the status registry and the event stream, so it must never be
        // made to wait for a file system.  With the blocking pool's one thread
        // occupied the writer cannot run at all, and `record` still has to
        // return.
        let dir = TempDir::new().unwrap();
        let name = service("api");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async move {
            let (release, blocked) = std::sync::mpsc::channel::<()>();
            let occupied = tokio::task::spawn_blocking(move || {
                let _ = blocked.recv();
            });
            tokio::task::yield_now().await;

            let sink = LogSink::spawn(LogRouter::new(
                settings(dir.path(), 1 << 20, 3),
                BTreeSet::new(),
            ));

            let mut complaint = None;
            // More than the queue holds, so the overflow has to be dropped
            // rather than queued or waited on.
            for i in 0..QUEUE_CAPACITY * 2 {
                complaint = complaint.or(sink.record(&name, &format!("line {i}")));
            }
            assert!(
                complaint.is_some_and(|c| c.contains("cannot keep up")),
                "a sink that cannot keep up has to say so"
            );

            drop(release);
            occupied.await.unwrap();
            sink.shutdown().await;
        });
    }

    #[tokio::test]
    async fn the_sink_reports_a_broken_log_directory() {
        let dir = TempDir::new().unwrap();
        let blocked = dir.path().join("logs");
        fs::write(&blocked, "not a directory").unwrap();
        let sink = LogSink::spawn(LogRouter::new(settings(&blocked, 1024, 3), BTreeSet::new()));

        sink.record(&service("api"), "one");
        // The writer is asynchronous, so the problem may only be collected by
        // the shutdown that waits for it.
        assert!(sink.shutdown().await.is_some());
    }
}
