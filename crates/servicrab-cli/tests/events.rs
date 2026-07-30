//! Integration tests for `servicrab events` (live stream over the socket).

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Upper bound for any wait in this file; keeps a hung test from stalling CI.
const CEILING: Duration = Duration::from_secs(20);

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A service that keeps printing a marker until it is told to stop.
fn chatty(dir: &Path, name: &str, marker: &str) -> PathBuf {
    script(
        dir,
        name,
        &format!("trap 'exit 0' TERM INT\nwhile true; do echo {marker}; sleep 0.2; done"),
    )
}

fn write_config(dir: &Path, toml: &str) -> PathBuf {
    let path = dir.join("servicrab.toml");
    fs::write(&path, toml).unwrap();
    path
}

fn binary() -> PathBuf {
    assert_cmd::cargo::cargo_bin("servicrab")
}

fn cli(args: &[&str], config_path: &Path) -> (i32, String, String) {
    let output = Command::new(binary())
        .args(args)
        .arg("--config")
        .arg(config_path)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run servicrab");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Stops the daemon when the test ends, however it ends.
struct Daemon {
    config: PathBuf,
}

impl Daemon {
    fn start(config: &Path) -> Self {
        let (code, stdout, stderr) = cli(&["start"], config);
        assert_eq!(code, 0, "start failed: {stdout}{stderr}");
        Self {
            config: config.to_path_buf(),
        }
    }

    fn status(&self) -> String {
        let (_, stdout, _) = cli(&["status"], &self.config);
        stdout
    }

    /// Poll `status` until `predicate` holds.
    fn wait_for_status(&self, what: &str, predicate: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + CEILING;
        loop {
            let status = self.status();
            if predicate(&status) {
                return;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {what}; last status:\n{status}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn down(&self) {
        let _ = cli(&["down"], &self.config);
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new(binary())
            .arg("down")
            .arg("--config")
            .arg(&self.config)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// A running `servicrab events` process whose output is collected in the
/// background, so a test can wait for a line to show up.
struct Subscriber {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Subscriber {
    fn start(config: &Path, args: &[&str]) -> Self {
        let mut child = Command::new(binary())
            .arg("events")
            .args(args)
            .arg("--config")
            .arg(config)
            .env_remove("RUST_LOG")
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to run servicrab events");

        let stdout = child.stdout.take().expect("piped stdout");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });

        Self { child, lines }
    }

    fn collected(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    /// Wait until at least one collected line satisfies `predicate`.
    fn wait_for(&self, what: &str, predicate: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + CEILING;
        loop {
            if let Some(line) = self.collected().iter().find(|l| predicate(l)) {
                return line.clone();
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {what}; collected so far:\n{}",
                    self.collected().join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait for the process to exit on its own.
    fn wait_for_exit(&mut self) -> i32 {
        let deadline = Instant::now() + CEILING;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code().unwrap_or(-1),
                Ok(None) => {}
                Err(err) => panic!("could not wait for the subscriber: {err}"),
            }
            if Instant::now() >= deadline {
                panic!("the event stream did not end when the daemon stopped");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn two_service_config(api: &Path, worker: &Path) -> String {
    format!(
        r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
[services.worker]
command = ["{}"]
restart = "always"
"#,
        api.display(),
        worker.display()
    )
}

/// Start a daemon with two chatty services and wait until both are running.
fn running_stack(dir: &TempDir) -> (PathBuf, Daemon) {
    let api = chatty(dir.path(), "api.sh", "api-line");
    let worker = chatty(dir.path(), "worker.sh", "worker-line");
    let cfg = write_config(dir.path(), &two_service_config(&api, &worker));
    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("both services to run", |s| {
        s.matches("running").count() == 2
    });
    (cfg, daemon)
}

#[test]
fn a_subscriber_sees_captured_output() {
    let dir = TempDir::new().unwrap();
    let (cfg, _daemon) = running_stack(&dir);

    let events = Subscriber::start(&cfg, &["--json"]);
    let line = events.wait_for("an api log line", |l| {
        l.contains("\"kind\":\"log\"") && l.contains("api-line")
    });
    assert!(line.contains("\"service\":\"api\""), "{line}");
}

#[test]
fn a_subscriber_can_follow_one_service() {
    let dir = TempDir::new().unwrap();
    let (cfg, _daemon) = running_stack(&dir);

    let events = Subscriber::start(&cfg, &["--json", "worker"]);
    events.wait_for("a worker log line", |l| l.contains("worker-line"));
    assert!(
        events.collected().iter().all(|l| !l.contains("api-line")),
        "the filter let another service through:\n{}",
        events.collected().join("\n")
    );
}

#[test]
fn logs_can_be_left_out_of_the_stream() {
    let dir = TempDir::new().unwrap();
    let (cfg, _daemon) = running_stack(&dir);

    let events = Subscriber::start(&cfg, &["--json", "--no-logs"]);
    // Give the subscription time to attach before making something happen.
    std::thread::sleep(Duration::from_millis(300));
    let (code, stdout, stderr) = cli(&["restart", "worker"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");

    events.wait_for("the worker to come back up", |l| {
        l.contains("\"service\":\"worker\"") && l.contains("\"kind\":\"started\"")
    });
    assert!(
        events.collected().iter().all(|l| !l.contains("\"log\"")),
        "log lines leaked into a --no-logs stream:\n{}",
        events.collected().join("\n")
    );
}

#[test]
fn the_human_stream_prefixes_lines_with_the_service() {
    let dir = TempDir::new().unwrap();
    let (cfg, _daemon) = running_stack(&dir);

    let events = Subscriber::start(&cfg, &["api"]);
    let line = events.wait_for("a prefixed api line", |l| l.contains("api-line"));
    assert!(line.starts_with("api "), "{line:?}");
    assert!(line.contains('|'), "{line:?}");
}

#[test]
fn prefixes_can_be_turned_off() {
    let dir = TempDir::new().unwrap();
    let (cfg, _daemon) = running_stack(&dir);

    let events = Subscriber::start(&cfg, &["api", "--no-prefix"]);
    let line = events.wait_for("an unprefixed api line", |l| l.contains("api-line"));
    assert_eq!(line, "api-line");
}

#[test]
fn the_stream_ends_when_the_daemon_stops() {
    let dir = TempDir::new().unwrap();
    let (cfg, daemon) = running_stack(&dir);

    let mut events = Subscriber::start(&cfg, &["--json"]);
    events.wait_for("any event", |l| l.contains("\"type\":\"event\""));
    daemon.down();

    assert_eq!(events.wait_for_exit(), 0);
}

#[test]
fn an_unknown_service_is_rejected() {
    let dir = TempDir::new().unwrap();
    let (cfg, _daemon) = running_stack(&dir);

    let (code, _, stderr) = cli(&["events", "nope"], &cfg);
    assert_ne!(code, 0);
    assert!(stderr.contains("unknown service"), "{stderr}");
}

#[test]
fn events_without_a_daemon_says_so() {
    let dir = TempDir::new().unwrap();
    let api = chatty(dir.path(), "api.sh", "api-line");
    let worker = chatty(dir.path(), "worker.sh", "worker-line");
    let cfg = write_config(dir.path(), &two_service_config(&api, &worker));

    let (code, _, stderr) = cli(&["events"], &cfg);
    assert_ne!(code, 0);
    assert!(stderr.contains("no daemon is running"), "{stderr}");
}
