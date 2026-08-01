//! Which services the operator stopped by hand, remembered between daemon
//! runs.
//!
//! Only `restart = "unless-stopped"` consults this, and only when a stack is
//! started: a hand-stopped service is never restarted under any policy, but
//! only this one survives `servicrab down` followed by `servicrab start`.
//!
//! The file is a version line followed by a plain list of names, one per line,
//! next to the daemon's socket and log — small enough to read and edit when a
//! stack ends up in a state nobody wanted.  Deleting a line is a legitimate way
//! to forget a stop, so the format has to stay hand-editable: the version line
//! is optional on reading, and a file without one is read as a bare list, which
//! is exactly what earlier versions wrote.
//!
//! One daemon at a time is the only writer, which the pidfile lock now
//! guarantees.  Within that daemon the writes come from per-connection tasks,
//! so two concurrent `stop`s can interleave — a read-modify-write would lose
//! one of them.  A mutex serialises them, and the write goes to a temporary file
//! that is renamed into place, so a crash mid-write cannot leave a truncated
//! file that silently means "nothing was ever stopped".

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use servicrab_core::{with_dependents, Config, RestartPolicy, ServiceName};

/// The format marker written at the top of the file.
///
/// It exists so the format can change later without a daemon having to guess
/// what it is looking at.  A file without it is version 0 — a bare list — which
/// is what 0.3 and earlier wrote and what a hand edit that removes the line
/// leaves behind.
const VERSION_LINE: &str = "# servicrab stopped v1";

/// Serialises the read-modify-write.
///
/// The writes come from per-connection tasks inside one daemon, so a lock in
/// the process is enough; across processes the pidfile lock already guarantees
/// a single writer.  A poisoned mutex is recovered from rather than propagated:
/// losing the memory of a stop is a smaller problem than refusing to record the
/// next one.
static WRITING: Mutex<()> = Mutex::new(());

/// Distinguishes the temporary files two writes could otherwise share.
static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Read the remembered set.
///
/// A missing or unreadable file simply means nothing is remembered: the memory
/// of a stopped service is a convenience, and refusing to start a stack over it
/// would be worse than starting one service too many.
pub fn read(path: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    parse(&text)
}

/// Split the file into names, ignoring the version line and any other comment.
fn parse(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToString::to_string)
        .collect()
}

/// Render a set as the file's contents.
fn render(names: &BTreeSet<String>) -> String {
    let mut text = String::from(VERSION_LINE);
    text.push('\n');
    for name in names {
        text.push_str(name);
        text.push('\n');
    }
    text
}

/// Record whether `service` is currently stopped by hand.
///
/// Read-modify-write on every change rather than a set held in memory: the file
/// is the state, so a hand edit is picked up.  [`WRITING`] is what makes the
/// sequence atomic against the other connection tasks in this daemon.
///
/// This blocks on a mutex and does synchronous file I/O, so async callers have
/// to reach it through [`record_blocking`].
pub fn record(path: &Path, service: &str, stopped: bool) -> Result<(), String> {
    let _guard = WRITING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut names = read(path);
    let changed = if stopped {
        names.insert(service.to_string())
    } else {
        names.remove(service)
    };
    // Forgetting a name that was never there must not create the file.
    if !changed {
        return Ok(());
    }

    write_atomically(path, &render(&names))
}

/// [`record`], moved off the async runtime.
///
/// The work is a lock plus three filesystem calls; on a busy or networked
/// filesystem that is long enough to stall a worker thread that is also
/// supervising processes.
pub async fn record_blocking(path: PathBuf, service: String, stopped: bool) -> Result<(), String> {
    tokio::task::spawn_blocking(move || record(&path, &service, stopped))
        .await
        .map_err(|e| format!("could not record the stop: {e}"))?
}

/// Replace `path`'s contents in a way that cannot be observed half-written.
///
/// `std::fs::write` truncates first, so a crash between the truncate and the
/// write leaves an empty file — which reads back as "nothing was ever stopped",
/// silently undoing every stop the operator asked for.  A temporary file
/// renamed into place is either entirely the old contents or entirely the new
/// ones, because `rename` within a directory is atomic.
fn write_atomically(path: &Path, contents: &str) -> Result<(), String> {
    use std::io::Write;

    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    // Unique per write, not merely per process: [`WRITING`] serialises the
    // writers inside this daemon, but a temporary name that two of them could
    // share would make the invariant depend on that lock rather than stand on
    // its own.
    let temporary = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("stopped"),
        std::process::id(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));

    // The temporary file has to be in the same directory as its destination:
    // `rename` is only atomic within one filesystem.
    let outcome = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(contents.as_bytes())?;
        // Without this the rename can land before the contents do, and a
        // machine that loses power in between finds an empty file under the
        // real name — the very thing the rename was meant to prevent.
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();

    if let Err(problem) = outcome {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "could not write {} (via {}): {problem}",
            path.display(),
            directory.display()
        ));
    }
    Ok(())
}

/// Forget every remembered name the configuration no longer declares.
///
/// The file only ever grew: every stop was recorded, while [`held_back`] only
/// ever consults the `unless-stopped` services, so a renamed or deleted service
/// left its name behind for good.  Reconciling at startup — where the daemon has
/// just read the configuration and no request can be in flight — keeps the file
/// as small as the project.
///
/// Names are compared against every service in the configuration rather than
/// against the started plan, because a profile that is not active this time is
/// not a service that is gone.
pub fn reconcile(path: &Path, config: &Config) -> Result<BTreeSet<String>, String> {
    let remembered = read(path);
    if remembered.is_empty() {
        return Ok(BTreeSet::new());
    }

    let known: BTreeSet<&str> = config.services.keys().map(ServiceName::as_str).collect();
    let (kept, dropped): (BTreeSet<String>, BTreeSet<String>) = remembered
        .into_iter()
        .partition(|name| known.contains(name.as_str()));

    if !dropped.is_empty() {
        let _guard = WRITING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if kept.is_empty() {
            // An empty file and no file mean the same thing; removing it is
            // tidier and matches what an operator would do by hand.
            std::fs::remove_file(path)
                .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
        } else {
            write_atomically(path, &render(&kept))?;
        }
    }
    Ok(dropped)
}

/// The planned services that must not be started, given what is remembered.
///
/// A remembered name only counts while the service asks for it with
/// `restart = "unless-stopped"`; every other policy starts as it always has, so
/// adopting this file cannot change an existing stack.  Dependents of a held
/// back service are held back too — see [`with_dependents`].
pub fn held_back(
    config: &Config,
    plan: &[ServiceName],
    remembered: &BTreeSet<String>,
) -> BTreeSet<ServiceName> {
    let seeds: BTreeSet<ServiceName> = plan
        .iter()
        .filter(|name| remembered.contains(name.as_str()))
        .filter(|name| {
            config
                .services
                .get(*name)
                .is_some_and(|service| service.restart == RestartPolicy::UnlessStopped)
        })
        .cloned()
        .collect();

    if seeds.is_empty() {
        return BTreeSet::new();
    }
    with_dependents(config, plan, &seeds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn config_with(body: &str) -> (TempDir, Config, Vec<ServiceName>) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("servicrab.toml");
        let mut file = std::fs::File::create(&path).expect("create config");
        file.write_all(body.as_bytes()).expect("write config");
        let (config, _) = servicrab_core::load(&path).expect("valid config");
        let plan = config.start_order.clone();
        (dir, config, plan)
    }

    const STACK: &str = r#"
version = 1
[project]
name = "demo"

[services.db]
command = ["true"]
restart = "unless-stopped"

[services.api]
command = ["true"]
depends_on = ["db"]
restart = "always"

[services.cache]
command = ["true"]
restart = "always"
"#;

    #[test]
    fn an_absent_file_remembers_nothing() {
        let dir = TempDir::new().expect("temp dir");
        assert!(read(&dir.path().join("stopped")).is_empty());
    }

    #[test]
    fn a_recorded_name_survives_a_reread() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");

        record(&path, "api", true).expect("record");
        record(&path, "db", true).expect("record");
        assert_eq!(read(&path), set(&["api", "db"]));

        record(&path, "api", false).expect("record");
        assert_eq!(read(&path), set(&["db"]));
    }

    /// The exact bytes on disk, because a human may have to read or edit them.
    ///
    /// The version line lets the format change later without a daemon having to
    /// guess what it is looking at; everything below it is still one name per
    /// line, so deleting a line remains a legitimate way to forget a stop.
    #[test]
    fn the_file_is_a_version_line_and_then_names() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");

        record(&path, "api", true).expect("record");
        record(&path, "db", true).expect("record");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "# servicrab stopped v1\napi\ndb\n"
        );
    }

    /// A file written by 0.3 has no version line, and neither does one an
    /// operator edited it out of.  Both have to keep working.
    #[test]
    fn a_file_without_a_version_line_is_still_read() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");
        std::fs::write(&path, "api\ndb\n").expect("write");

        assert_eq!(read(&path), set(&["api", "db"]));

        // And the next write brings it up to date without losing anything.
        record(&path, "cache", true).expect("record");
        assert_eq!(read(&path), set(&["api", "cache", "db"]));
        assert!(std::fs::read_to_string(&path)
            .expect("read")
            .starts_with(VERSION_LINE));
    }

    /// Deleting a name by hand is how an operator forgets a stop.
    #[test]
    fn a_hand_edited_file_is_honoured() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");
        record(&path, "api", true).expect("record");
        record(&path, "db", true).expect("record");

        let edited = std::fs::read_to_string(&path)
            .expect("read")
            .lines()
            .filter(|line| *line != "api")
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(&path, edited).expect("write");

        assert_eq!(read(&path), set(&["db"]));
    }

    /// Two `stop`s at once come from two connection tasks, and a
    /// read-modify-write would lose one of them.
    #[test]
    fn concurrent_records_do_not_lose_each_other() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");

        let names: Vec<String> = (0..32).map(|n| format!("svc-{n:02}")).collect();
        std::thread::scope(|scope| {
            for name in &names {
                let path = path.clone();
                scope.spawn(move || record(&path, name, true).expect("record"));
            }
        });

        let remembered = read(&path);
        assert_eq!(
            remembered.len(),
            names.len(),
            "an update was lost: {remembered:?}"
        );
        for name in &names {
            assert!(remembered.contains(name), "{name} was lost");
        }
    }

    /// Interleaved stops and starts must leave the file agreeing with the last
    /// decision about each service, not with whichever writer finished last.
    #[test]
    fn concurrent_stops_and_starts_each_land() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");
        for n in 0..16 {
            record(&path, &format!("svc-{n:02}"), true).expect("record");
        }

        // Forget the even ones and remember sixteen more, all at once.
        std::thread::scope(|scope| {
            for n in 0..16 {
                let path = path.clone();
                scope.spawn(move || {
                    if n % 2 == 0 {
                        record(&path, &format!("svc-{n:02}"), false).expect("record");
                    }
                    record(&path, &format!("extra-{n:02}"), true).expect("record");
                });
            }
        });

        let remembered = read(&path);
        for n in 0..16 {
            let name = format!("svc-{n:02}");
            assert_eq!(
                remembered.contains(&name),
                n % 2 != 0,
                "{name} is wrong in {remembered:?}"
            );
            assert!(remembered.contains(&format!("extra-{n:02}")));
        }
    }

    /// A crash between truncate and write used to leave an empty file, which
    /// reads back as "nothing was ever stopped".  The temporary file it goes
    /// through must not be left behind either.
    #[test]
    fn a_write_leaves_no_debris() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");
        record(&path, "api", true).expect("record");

        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["stopped".to_string()], "{entries:?}");
    }

    #[test]
    fn recording_the_same_state_twice_is_a_no_op() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");

        record(&path, "api", true).expect("record");
        record(&path, "api", true).expect("record");
        assert_eq!(read(&path), set(&["api"]));

        // Forgetting a name that was never there must not create the file.
        let empty = dir.path().join("none");
        record(&empty, "api", false).expect("record");
        assert!(!empty.exists());
    }

    #[test]
    fn blank_lines_and_stray_whitespace_are_ignored() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");
        std::fs::write(&path, "api\n\n  db  \n\n").expect("write");
        assert_eq!(read(&path), set(&["api", "db"]));
    }

    /// The file only ever grew: every stop was recorded, while `held_back` only
    /// consults the `unless-stopped` services, so a renamed or deleted service
    /// left its name behind for good.
    #[test]
    fn reconciliation_forgets_services_the_config_no_longer_declares() {
        let (dir, config, _plan) = config_with(STACK);
        let path = dir.path().join("stopped");
        for name in ["db", "gone", "renamed"] {
            record(&path, name, true).expect("record");
        }

        let dropped = reconcile(&path, &config).expect("reconcile");

        assert_eq!(dropped, set(&["gone", "renamed"]));
        assert_eq!(read(&path), set(&["db"]));
    }

    /// A service left out of this run's profiles is not a service that is gone.
    #[test]
    fn reconciliation_keeps_a_service_that_is_merely_not_planned() {
        let (dir, config, _plan) = config_with(STACK);
        let path = dir.path().join("stopped");
        record(&path, "cache", true).expect("record");

        assert!(reconcile(&path, &config).expect("reconcile").is_empty());
        assert_eq!(read(&path), set(&["cache"]));
    }

    #[test]
    fn reconciliation_removes_a_file_with_nothing_left_in_it() {
        let (dir, config, _plan) = config_with(STACK);
        let path = dir.path().join("stopped");
        record(&path, "gone", true).expect("record");

        assert_eq!(
            reconcile(&path, &config).expect("reconcile"),
            set(&["gone"])
        );
        assert!(!path.exists(), "an empty file should have been removed");
    }

    #[test]
    fn reconciling_an_absent_file_does_nothing() {
        let (dir, config, _plan) = config_with(STACK);
        let path = dir.path().join("stopped");

        assert!(reconcile(&path, &config).expect("reconcile").is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn only_unless_stopped_services_are_held_back() {
        let (_dir, config, plan) = config_with(STACK);

        // `cache` is remembered, but its policy does not ask to be.
        assert!(held_back(&config, &plan, &set(&["cache"])).is_empty());
    }

    #[test]
    fn a_held_back_service_takes_its_dependents_with_it() {
        let (_dir, config, plan) = config_with(STACK);
        let held = held_back(&config, &plan, &set(&["db"]));

        let held: Vec<&str> = held.iter().map(ServiceName::as_str).collect();
        assert_eq!(held, vec!["api", "db"]);
    }

    #[test]
    fn a_name_outside_the_plan_is_ignored() {
        let (_dir, config, plan) = config_with(STACK);
        let plan: Vec<ServiceName> = plan
            .into_iter()
            .filter(|name| name.as_str() != "db")
            .collect();

        assert!(held_back(&config, &plan, &set(&["db", "gone"])).is_empty());
    }
}
