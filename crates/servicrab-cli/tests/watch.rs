//! Integration tests for `servicrab watch` and the `[watch]` config block.
//!
//! Every fixture is generated into a [`TempDir`] at test time.  The watched
//! service appends to a counter file on every start, so "did it restart?" is
//! a question about the number of lines in that file rather than about timing.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tempfile::TempDir;

/// Upper bound for any wait in this file; keeps a hung test from stalling CI.
const CEILING: Duration = Duration::from_secs(15);

fn binary() -> PathBuf {
    assert_cmd::cargo::cargo_bin("servicrab")
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A project whose single service records every start in `starts.log`.
///
/// `watch_body` is dropped verbatim into `[services.app.watch]`; an empty
/// string leaves the block out entirely.
fn project(dir: &Path, watch_body: &str) -> PathBuf {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/a.txt"), "one\n").unwrap();

    let starts = dir.join("starts.log");
    script(
        dir,
        "app.sh",
        &format!("echo start >> {}\nsleep 30", starts.display()),
    );

    let watch = if watch_body.is_empty() {
        String::new()
    } else {
        format!("\n[services.app.watch]\n{watch_body}\n")
    };

    let path = dir.join("servicrab.toml");
    fs::write(
        &path,
        format!(
            r#"
version = 1

[project]
name = "watchdemo"

[services.app]
command = ["{}"]
cwd = "{}"
restart = "never"
{watch}
"#,
            dir.join("app.sh").display(),
            dir.display(),
        ),
    )
    .unwrap();
    path
}

fn spawn(command: &str, config_path: &Path) -> Child {
    Command::new(binary())
        .arg(command)
        .arg("--config")
        .arg(config_path)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn servicrab {command}: {e}"))
}

fn starts(path: &Path) -> usize {
    fs::read_to_string(path).map_or(0, |text| text.lines().count())
}

/// Wait until `starts.log` has at least `count` lines.
fn wait_for_starts(path: &Path, count: usize) {
    let deadline = Instant::now() + CEILING;
    while Instant::now() < deadline {
        if starts(path) >= count {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("timed out waiting for {count} starts; saw {}", starts(path));
}

/// Stop the supervisor the way a user would, and drain its stderr.
fn interrupt(mut child: Child) -> String {
    let pid = Pid::from_raw(child.id() as i32);
    let _ = kill(pid, Signal::SIGINT);

    let deadline = Instant::now() + CEILING;
    loop {
        match child.try_wait().unwrap() {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("supervisor did not exit within {CEILING:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    stderr
}

// ── restart on change ──────────────────────────────────────────────────────

#[test]
fn a_change_under_a_watched_path_restarts_the_service() {
    let dir = TempDir::new().unwrap();
    let cfg = project(
        dir.path(),
        "paths = [\"src\"]\ninterval = \"200ms\"\ndebounce = \"100ms\"",
    );
    let starts_log = dir.path().join("starts.log");

    let child = spawn("watch", &cfg);
    wait_for_starts(&starts_log, 1);

    fs::write(dir.path().join("src/a.txt"), "two\n").unwrap();
    wait_for_starts(&starts_log, 2);

    let stderr = interrupt(child);
    assert!(
        stderr.contains("a.txt changed"),
        "expected the change to be reported, got: {stderr}"
    );
}

#[test]
fn a_new_file_under_a_watched_directory_restarts_the_service() {
    let dir = TempDir::new().unwrap();
    let cfg = project(
        dir.path(),
        "paths = [\"src\"]\ninterval = \"200ms\"\ndebounce = \"100ms\"",
    );
    let starts_log = dir.path().join("starts.log");

    let child = spawn("watch", &cfg);
    wait_for_starts(&starts_log, 1);

    fs::write(dir.path().join("src/new.txt"), "hello\n").unwrap();
    wait_for_starts(&starts_log, 2);

    interrupt(child);
}

#[test]
fn several_changes_in_a_row_cause_a_single_restart() {
    let dir = TempDir::new().unwrap();
    let cfg = project(
        dir.path(),
        "paths = [\"src\"]\ninterval = \"200ms\"\ndebounce = \"400ms\"",
    );
    let starts_log = dir.path().join("starts.log");

    let child = spawn("watch", &cfg);
    wait_for_starts(&starts_log, 1);

    for i in 0..5 {
        fs::write(dir.path().join(format!("src/f{i}.txt")), "x\n").unwrap();
        std::thread::sleep(Duration::from_millis(120));
    }
    wait_for_starts(&starts_log, 2);
    std::thread::sleep(Duration::from_millis(800));

    let seen = starts(&starts_log);
    interrupt(child);
    assert_eq!(seen, 2, "the debounce window should collapse the burst");
}

#[test]
fn ignored_paths_do_not_trigger_a_restart() {
    let dir = TempDir::new().unwrap();
    let cfg = project(
        dir.path(),
        "paths = [\"src\"]\nignore = [\"*.log\", \"vendor\"]\ninterval = \"200ms\"\ndebounce = \"100ms\"",
    );
    let starts_log = dir.path().join("starts.log");

    let child = spawn("watch", &cfg);
    wait_for_starts(&starts_log, 1);

    fs::create_dir_all(dir.path().join("src/vendor")).unwrap();
    fs::write(dir.path().join("src/vendor/lib.txt"), "x\n").unwrap();
    fs::write(dir.path().join("src/debug.log"), "noise\n").unwrap();
    std::thread::sleep(Duration::from_millis(900));

    let seen = starts(&starts_log);
    interrupt(child);
    assert_eq!(seen, 1, "ignored files must not restart the service");
}

// ── up honours the same config ─────────────────────────────────────────────

#[test]
fn up_also_restarts_on_a_watched_change() {
    let dir = TempDir::new().unwrap();
    let cfg = project(
        dir.path(),
        "paths = [\"src\"]\ninterval = \"200ms\"\ndebounce = \"100ms\"",
    );
    let starts_log = dir.path().join("starts.log");

    let child = spawn("up", &cfg);
    wait_for_starts(&starts_log, 1);

    fs::write(dir.path().join("src/a.txt"), "changed\n").unwrap();
    wait_for_starts(&starts_log, 2);

    interrupt(child);
}

// ── entry checks ───────────────────────────────────────────────────────────

#[test]
fn a_tree_that_never_settles_still_gets_its_restart() {
    // A file changing faster than the debounce window keeps the tree from ever
    // being quiet.  Without a cap on the settle loop the watcher re-scans
    // forever and the restart it exists to request is never made.
    let dir = TempDir::new().unwrap();
    let cfg = project(
        dir.path(),
        "paths = [\"src\"]\ninterval = \"200ms\"\ndebounce = \"100ms\"",
    );
    let starts_log = dir.path().join("starts.log");

    let child = spawn("watch", &cfg);
    wait_for_starts(&starts_log, 1);

    // A writer that keeps a file in the watched tree changing throughout.
    let noisy = dir.path().join("src/noisy.txt");
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                // Growing content, so the change is visible even to a
                // second-granularity mtime.
                let _ = fs::write(&noisy, "x".repeat(n as usize % 4096 + 1));
                std::thread::sleep(Duration::from_millis(40));
            }
        })
    };

    // The verdict comes from the service actually starting again, within the
    // file's own bounded wait.
    let result = std::panic::catch_unwind(|| wait_for_starts(&starts_log, 2));
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    let stderr = interrupt(child);
    result.unwrap_or_else(|_| {
        panic!("the watcher never requested a restart; supervisor said: {stderr}")
    });
}

#[test]
fn watch_refuses_to_start_when_nothing_is_watched() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path(), "");

    let output = Command::new(binary())
        .arg("watch")
        .arg("--config")
        .arg(&cfg)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run servicrab watch");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nothing to watch"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("[services.app.watch]"),
        "the error should show how to fix it: {stderr}"
    );
}

#[test]
fn a_watched_path_that_does_not_exist_is_a_config_error() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path(), "paths = [\"nope\"]");

    let output = Command::new(binary())
        .arg("check")
        .arg("--config")
        .arg(&cfg)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run servicrab check");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[watch]"), "unexpected stderr: {stderr}");
}

#[test]
fn an_empty_paths_list_is_a_config_error() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path(), "paths = []");

    let output = Command::new(binary())
        .arg("check")
        .arg("--config")
        .arg(&cfg)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run servicrab check");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("at least one file or directory"),
        "unexpected stderr: {stderr}"
    );
}
