//! File watching: restart a service when its sources change.
//!
//! The watcher polls instead of using platform notification APIs.  Polling
//! keeps the dependency footprint at zero, behaves identically on Linux and
//! macOS, and is trivially testable: a scan is a pure function of the file
//! system, and a change is a difference between two scans.
//!
//! A restart is requested through the ordinary [`Control`] channel, so the
//! supervisor treats a watch-triggered restart exactly like `servicrab restart`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::oneshot;

use crate::config::{Config, ServiceName, WatchSettings};
use crate::runtime::event::{EventKind, EventSender, ServiceEvent};
use crate::runtime::stack::{Control, ControlTx};

/// Stop scanning after this many files, so a mis-configured `paths` entry
/// cannot pin a core.
const MAX_ENTRIES: usize = 20_000;

/// Stop descending after this many directory levels.
///
/// [`MAX_ENTRIES`] bounds the number of *files*, not the depth, and [`walk`] is
/// recursive: a pathologically deep tree would overflow the stack, which aborts
/// the whole supervisor rather than failing a scan.  Sixty-four levels is far
/// beyond any real source tree.
const MAX_DEPTH: usize = 64;

/// How many times the debounce loop may re-scan before it gives up on waiting
/// for quiet.
///
/// Any file that changes faster than `debounce` — a log inside the watched
/// tree, a build directory — keeps the tree from ever settling.  Without a cap
/// the loop re-scans forever, `changed` grows without bound, and the restart the
/// watcher exists to request is never made.  The pending change is not lost:
/// hitting the cap simply forces the restart out now.
const MAX_DEBOUNCE_ROUNDS: usize = 10;

/// What a single scan recorded about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStamp {
    /// Last modification time, when the platform reports one.
    pub modified: Option<SystemTime>,
    /// Size in bytes.
    pub len: u64,
}

/// The state of the watched tree at one point in time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Every watched file, keyed by absolute path.
    pub files: BTreeMap<PathBuf, FileStamp>,
    /// Whether the scan hit [`MAX_ENTRIES`] and stopped early.
    pub truncated: bool,
    /// Whether the scan hit [`MAX_DEPTH`] and left a subtree unvisited.
    pub too_deep: bool,
}

impl Scan {
    /// Paths that appeared, disappeared or changed between `self` and `next`.
    pub fn changes(&self, next: &Scan) -> Vec<PathBuf> {
        let mut changed = Vec::new();
        for (path, stamp) in &next.files {
            if self.files.get(path) != Some(stamp) {
                changed.push(path.clone());
            }
        }
        for path in self.files.keys() {
            if !next.files.contains_key(path) {
                changed.push(path.clone());
            }
        }
        changed.sort();
        changed.dedup();
        changed
    }
}

/// Walk the configured paths and record a stamp for every file.
///
/// Unreadable entries are skipped rather than reported: a file that vanishes
/// mid-scan is a change, not an error, and the next scan will notice it.  So is
/// a subtree deeper than [`MAX_DEPTH`], which sets [`Scan::too_deep`] instead of
/// risking a stack overflow.
///
/// This is blocking work — up to [`MAX_ENTRIES`] `symlink_metadata` calls plus a
/// `read_dir` per directory — so async callers go through [`scan_off_thread`].
pub fn scan(settings: &WatchSettings) -> Scan {
    let mut out = Scan::default();
    for root in &settings.paths {
        walk(root, root, 0, settings, &mut out);
        if out.truncated {
            break;
        }
    }
    out
}

/// [`scan`], moved onto a blocking thread.
///
/// A scan can stall for a long time on a slow, full or network-backed file
/// system, and the watcher shares its worker threads with the supervisor: a
/// scan on an async worker is a worker not driving child `wait()`s, health
/// probes or the control channel.
///
/// The scan is a pure function of the file system, so a cancelled caller can
/// simply abandon it; the detached thread finishes and throws its result away.
/// `None` means the blocking pool is gone — the runtime is shutting down — and
/// is deliberately not reported as "every file disappeared", which would ask
/// for a restart on the way out.
async fn scan_off_thread(settings: &Arc<WatchSettings>) -> Option<Scan> {
    let settings = Arc::clone(settings);
    tokio::task::spawn_blocking(move || scan(&settings))
        .await
        .ok()
}

fn walk(root: &Path, path: &Path, depth: usize, settings: &WatchSettings, out: &mut Scan) {
    if out.files.len() >= MAX_ENTRIES {
        out.truncated = true;
        return;
    }

    // Ignore rules are matched against the path relative to the watched root,
    // so `ignore = ["target"]` means "the target directory in here".
    let relative = path.strip_prefix(root).unwrap_or(path);
    if !relative.as_os_str().is_empty() && settings.is_ignored(relative) {
        return;
    }

    // `symlink_metadata` does not follow links, which keeps a symlink loop
    // from turning the walk into an infinite one.
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };

    if meta.is_dir() {
        // Recursion is bounded here rather than by [`MAX_ENTRIES`]: a deep tree
        // holding few files would still exhaust the stack, and a stack overflow
        // is an abort the supervisor cannot catch.
        if depth >= MAX_DEPTH {
            out.too_deep = true;
            tracing::warn!(
                path = %path.display(),
                limit = MAX_DEPTH,
                "not descending any further into the watched tree"
            );
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        children.sort();
        for child in children {
            walk(root, &child, depth + 1, settings, out);
            if out.truncated {
                return;
            }
        }
        return;
    }

    out.files.insert(
        path.to_path_buf(),
        FileStamp {
            modified: meta.modified().ok(),
            len: meta.len(),
        },
    );
}

/// Watch one service until the control channel goes away.
///
/// After a change the watcher waits for `debounce` of quiet before asking for
/// the restart, so a `cargo build` writing a hundred files causes one restart
/// rather than a hundred.  It waits for quiet at most [`MAX_DEBOUNCE_ROUNDS`]
/// times: a tree that never settles must still get its restart.
///
/// Every scan runs on a blocking thread; the loop itself only sleeps and talks
/// to channels.
pub async fn watch_service(
    service: ServiceName,
    settings: WatchSettings,
    control: ControlTx,
    events: EventSender,
) {
    // Shared with each blocking scan rather than cloned for it.
    let settings = Arc::new(settings);

    let Some(mut previous) = scan_off_thread(&settings).await else {
        return;
    };
    if previous.truncated {
        let _ = events.send(ServiceEvent::new(
            service.clone(),
            EventKind::WatchTruncated { limit: MAX_ENTRIES },
        ));
    }

    loop {
        tokio::time::sleep(settings.interval).await;

        let Some(mut current) = scan_off_thread(&settings).await else {
            return;
        };
        let mut changed = previous.changes(&current);
        if changed.is_empty() {
            continue;
        }

        // Wait for the tree to settle before acting on the first change — but
        // only for a bounded number of rounds.  A file that changes faster than
        // `debounce` (a log inside the tree, a build directory) would otherwise
        // keep the loop here forever and the restart would never be requested.
        for round in 0..MAX_DEBOUNCE_ROUNDS {
            tokio::time::sleep(settings.debounce).await;
            let Some(settled) = scan_off_thread(&settings).await else {
                return;
            };
            let extra = current.changes(&settled);
            current = settled;
            if extra.is_empty() {
                break;
            }
            changed.extend(extra);
            if round + 1 == MAX_DEBOUNCE_ROUNDS {
                tracing::warn!(
                    service = %service,
                    rounds = MAX_DEBOUNCE_ROUNDS,
                    "the watched tree is still changing; restarting anyway"
                );
            }
        }

        changed.sort();
        changed.dedup();

        let first = changed.first().cloned().unwrap_or_default();
        let _ = events.send(ServiceEvent::new(
            service.clone(),
            EventKind::WatchTriggered {
                path: first,
                changed: changed.len(),
            },
        ));

        let (ack_tx, ack_rx) = oneshot::channel();
        if control
            .send(Control::Restart {
                service: service.clone(),
                ack: ack_tx,
            })
            .is_err()
        {
            // The supervisor is gone; nothing left to watch for.
            return;
        }

        match ack_rx.await {
            Ok(Err(message)) => {
                let _ = events.send(ServiceEvent::new(
                    service.clone(),
                    EventKind::WatchFailed { message },
                ));
            }
            Ok(Ok(_)) => {}
            // The supervisor dropped the ack: it is shutting down.
            Err(_) => return,
        }

        // A restart rewrites files itself (pid files, sockets); re-scan so the
        // next comparison starts from the post-restart state.
        let Some(next) = scan_off_thread(&settings).await else {
            return;
        };
        previous = next;
    }
}

/// Spawn a watcher task for every planned service that declares `[watch]`.
///
/// Returns the join handles, and an empty vector when nothing is watched.
pub fn spawn_watchers(
    config: &Config,
    plan: &[ServiceName],
    control: &ControlTx,
    events: &EventSender,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();
    for name in plan {
        let Some(service) = config.services.get(name) else {
            continue;
        };
        let Some(settings) = service.watch.clone() else {
            continue;
        };
        handles.push(tokio::spawn(watch_service(
            name.clone(),
            settings,
            control.clone(),
            events.clone(),
        )));
    }
    handles
}

/// Services in `plan` that declare a `[watch]` block.
pub fn watched_services(config: &Config, plan: &[ServiceName]) -> Vec<ServiceName> {
    plan.iter()
        .filter(|name| {
            config
                .services
                .get(*name)
                .is_some_and(|svc| svc.watch.is_some())
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use tempfile::TempDir;

    fn settings(dir: &Path, ignore: &[&str]) -> WatchSettings {
        WatchSettings {
            paths: vec![dir.to_path_buf()],
            ignore: ignore.iter().map(|s| (*s).to_string()).collect(),
            interval: Duration::from_millis(100),
            debounce: Duration::from_millis(50),
        }
    }

    #[test]
    fn a_scan_records_every_file_below_the_root() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), "b").unwrap();

        let s = scan(&settings(dir.path(), &[]));
        assert_eq!(s.files.len(), 2);
        assert!(!s.truncated);
    }

    #[test]
    fn an_unchanged_tree_reports_no_changes() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let settings = settings(dir.path(), &[]);

        let before = scan(&settings);
        let after = scan(&settings);
        assert!(before.changes(&after).is_empty());
    }

    #[test]
    fn a_new_file_is_a_change() {
        let dir = TempDir::new().unwrap();
        let settings = settings(dir.path(), &[]);
        let before = scan(&settings);

        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        let after = scan(&settings);

        let changed = before.changes(&after);
        assert_eq!(changed.len(), 1);
        assert!(changed[0].ends_with("new.txt"));
    }

    #[test]
    fn a_removed_file_is_a_change() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("gone.txt"), "x").unwrap();
        let settings = settings(dir.path(), &[]);
        let before = scan(&settings);

        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        let after = scan(&settings);

        assert_eq!(before.changes(&after).len(), 1);
    }

    #[test]
    fn a_resized_file_is_a_change_even_with_a_coarse_clock() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "short").unwrap();
        let settings = settings(dir.path(), &[]);
        let before = scan(&settings);

        std::fs::write(dir.path().join("a.txt"), "considerably longer").unwrap();
        let after = scan(&settings);

        assert_eq!(before.changes(&after).len(), 1);
    }

    #[test]
    fn ignored_directories_are_not_scanned() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/big.bin"), "x").unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();

        let s = scan(&settings(dir.path(), &["target"]));
        assert_eq!(s.files.len(), 1);
    }

    #[test]
    fn ignored_extensions_are_not_scanned() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.log"), "a").unwrap();
        std::fs::write(dir.path().join("a.rs"), "a").unwrap();

        let s = scan(&settings(dir.path(), &["*.log"]));
        assert_eq!(s.files.len(), 1);
        assert!(s.files.keys().next().unwrap().ends_with("a.rs"));
    }

    #[test]
    fn an_ignore_prefix_with_a_slash_matches_a_subtree() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src/generated")).unwrap();
        std::fs::write(dir.path().join("src/generated/x.rs"), "x").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "m").unwrap();

        let s = scan(&settings(dir.path(), &["src/generated"]));
        assert_eq!(s.files.len(), 1);
    }

    #[test]
    fn a_watched_file_may_be_a_single_path() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("only.txt");
        std::fs::write(&file, "x").unwrap();

        let s = scan(&WatchSettings {
            paths: vec![file.clone()],
            ignore: Vec::new(),
            interval: Duration::from_secs(1),
            debounce: Duration::from_millis(50),
        });
        assert_eq!(s.files.len(), 1);
        assert!(s.files.contains_key(&file));
    }

    #[test]
    fn a_tree_deeper_than_the_limit_is_reported_rather_than_overflowing_the_stack() {
        // `MAX_ENTRIES` bounds files, not depth, and `walk` is recursive: a deep
        // tree would abort the supervisor with a stack overflow.
        let dir = TempDir::new().unwrap();
        let mut deep = dir.path().to_path_buf();
        for i in 0..(MAX_DEPTH + 5) {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("buried.txt"), "x").unwrap();
        std::fs::write(dir.path().join("shallow.txt"), "y").unwrap();

        let s = scan(&settings(dir.path(), &[]));
        assert!(s.too_deep, "the depth limit should be reported");
        // What is reachable is still recorded, so the watcher keeps working.
        assert!(
            s.files.keys().any(|p| p.ends_with("shallow.txt")),
            "{:?}",
            s.files.keys().collect::<Vec<_>>()
        );
        assert!(
            !s.files.keys().any(|p| p.ends_with("buried.txt")),
            "the file below the limit should not have been visited"
        );
    }

    #[test]
    fn a_tree_within_the_depth_limit_is_scanned_whole() {
        let dir = TempDir::new().unwrap();
        let mut deep = dir.path().to_path_buf();
        // The root itself is depth 0, so `MAX_DEPTH` levels of directories fit.
        for i in 0..(MAX_DEPTH - 1) {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("reachable.txt"), "x").unwrap();

        let s = scan(&settings(dir.path(), &[]));
        assert!(!s.too_deep, "a normal tree must not trip the limit");
        assert_eq!(s.files.len(), 1);
    }

    #[test]
    fn a_missing_root_scans_to_nothing() {
        let dir = TempDir::new().unwrap();
        let s = scan(&settings(&dir.path().join("nope"), &[]));
        assert!(s.files.is_empty());
    }

    #[test]
    fn a_scan_goes_through_the_blocking_pool_rather_than_the_worker() {
        // Deterministic rather than timing-based: with room for exactly one
        // blocking thread, an occupied pool must hold the scan up.  A scan that
        // ran inline on the async worker would finish regardless, so the timeout
        // below is the assertion.
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let settings = Arc::new(settings(dir.path(), &[]));

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
            // Make sure the pool's one thread is actually taken before timing
            // anything: `spawn_blocking` only queues.
            tokio::task::yield_now().await;

            let held_up = tokio::time::timeout(
                Duration::from_millis(200),
                scan_off_thread(&Arc::clone(&settings)),
            )
            .await;
            assert!(
                held_up.is_err(),
                "the scan did not wait for the blocking pool, so it ran on the worker"
            );

            drop(release);
            occupied.await.unwrap();
            let scanned = tokio::time::timeout(
                Duration::from_secs(5),
                scan_off_thread(&Arc::clone(&settings)),
            )
            .await
            .expect("the scan runs once the pool is free")
            .expect("the pool is up");
            assert_eq!(scanned.files.len(), 1);
        });
    }
}
