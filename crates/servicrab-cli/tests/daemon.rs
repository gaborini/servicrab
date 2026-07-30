//! Integration tests for the background daemon (`start` / `status` / `down`).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tempfile::TempDir;

/// Upper bound for any wait in this file; keeps a hung test from stalling CI.
const CEILING: Duration = Duration::from_secs(20);

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn config(dir: &Path, toml: &str) -> PathBuf {
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

/// A long-lived service that exits cleanly on SIGTERM.
fn resident(dir: &Path, name: &str) -> PathBuf {
    script(
        dir,
        name,
        "trap 'exit 0' TERM INT\necho up\nwhile true; do sleep 0.2; done",
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
    fn wait_for_status(&self, what: &str, predicate: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + CEILING;
        loop {
            let status = self.status();
            if predicate(&status) {
                return status;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {what}; last status:\n{status}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
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

fn wait_bounded(child: &mut Child) -> i32 {
    let deadline = Instant::now() + CEILING;
    loop {
        match child.try_wait().unwrap() {
            Some(status) => return status.code().unwrap_or(-1),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the daemon did not exit within {CEILING:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[test]
fn a_started_daemon_reports_a_running_service() {
    let dir = TempDir::new().unwrap();
    let svc = resident(dir.path(), "api.sh");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            svc.display()
        ),
    );

    let daemon = Daemon::start(&cfg);
    let status = daemon.wait_for_status("api to run", |s| s.contains("running"));

    assert!(status.contains("api"), "{status}");
    assert!(status.contains("SERVICE"), "{status}");
}

#[test]
fn down_stops_the_daemon_and_removes_its_socket() {
    let dir = TempDir::new().unwrap();
    let svc = resident(dir.path(), "api.sh");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            svc.display()
        ),
    );

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let (code, stdout, stderr) = cli(&["down"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("stopped"), "{stdout}");
    assert!(!dir.path().join(".servicrab/daemon.sock").exists());
    assert!(!dir.path().join(".servicrab/daemon.pid").exists());

    let (code, stdout, _) = cli(&["status"], &cfg);
    assert_eq!(code, 1);
    assert!(stdout.contains("no daemon is running"), "{stdout}");
}

#[test]
fn starting_twice_is_refused() {
    let dir = TempDir::new().unwrap();
    let svc = resident(dir.path(), "api.sh");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            svc.display()
        ),
    );

    let _daemon = Daemon::start(&cfg);
    let (code, _, stderr) = cli(&["start"], &cfg);

    assert_eq!(code, 1);
    assert!(stderr.contains("already running"), "{stderr}");
}

#[test]
fn status_without_a_daemon_is_not_an_error_message_soup() {
    let dir = TempDir::new().unwrap();
    let svc = resident(dir.path(), "api.sh");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
"#,
            svc.display()
        ),
    );

    let (code, stdout, _) = cli(&["status"], &cfg);
    assert_eq!(code, 1);
    assert!(stdout.contains("servicrab start"), "{stdout}");

    let (code, stdout, _) = cli(&["status", "--json"], &cfg);
    assert_eq!(code, 1);
    assert!(stdout.contains("\"running\":false"), "{stdout}");

    // Stopping something that is not running is not a failure.
    let (code, stdout, _) = cli(&["down"], &cfg);
    assert_eq!(code, 0);
    assert!(stdout.contains("no daemon is running"), "{stdout}");
}

#[test]
fn status_json_describes_every_service() {
    let dir = TempDir::new().unwrap();
    let svc = resident(dir.path(), "api.sh");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            svc.display()
        ),
    );

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let (code, stdout, _) = cli(&["status", "--json"], &cfg);
    assert_eq!(code, 0);

    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["running"], serde_json::json!(true));
    let service = &parsed["services"][0];
    assert_eq!(service["name"], serde_json::json!("api"));
    assert_eq!(service["state"], serde_json::json!("running"));
    assert!(service["pid"].as_i64().unwrap() > 0);
}

#[test]
fn the_daemon_reports_health_check_results() {
    let dir = TempDir::new().unwrap();
    let svc = resident(dir.path(), "api.sh");
    let probe = script(dir.path(), "probe.sh", "exit 0");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
[services.api.health]
command = ["{}"]
interval = "200ms"
"#,
            svc.display(),
            probe.display()
        ),
    );

    let daemon = Daemon::start(&cfg);
    let status = daemon.wait_for_status("api to be healthy", |s| s.contains("healthy"));
    assert!(status.contains("running"), "{status}");
}

#[test]
fn the_daemon_writes_log_files() {
    let dir = TempDir::new().unwrap();
    let svc = resident(dir.path(), "api.sh");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
[services.api]
command = ["{}"]
restart = "always"
"#,
            svc.display()
        ),
    );

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let log = dir.path().join("logs/api.log");
    let deadline = Instant::now() + CEILING;
    while Instant::now() < deadline {
        if fs::read_to_string(&log)
            .map(|c| c.contains("up"))
            .unwrap_or(false)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the daemon never wrote {}", log.display());
}

/// A stack with one long-lived service, used by the control tests.
fn one_service(dir: &Path) -> PathBuf {
    let svc = resident(dir, "api.sh");
    config(
        dir,
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            svc.display()
        ),
    )
}

#[test]
fn a_single_service_can_be_stopped_and_started_again() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let (code, stdout, stderr) = cli(&["stop", "api"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("api stopped"), "{stdout}");

    let status = daemon.status();
    assert!(status.contains("stopped"), "{status}");

    // The daemon is still there, so the service can come back.
    let (code, stdout, _) = cli(&["start", "api"], &cfg);
    assert_eq!(code, 0);
    assert!(stdout.contains("api started"), "{stdout}");
    daemon.wait_for_status("api to run again", |s| s.contains("running"));
}

#[test]
fn restart_replaces_the_process() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));
    let before = pid_of(&daemon);

    let (code, stdout, stderr) = cli(&["restart", "api"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("api restarted"), "{stdout}");

    daemon.wait_for_status("a new process", |_| pid_of(&daemon) != before);
    assert_ne!(pid_of(&daemon), before);
}

#[test]
fn stopping_an_already_stopped_service_is_not_an_error() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    assert_eq!(cli(&["stop", "api"], &cfg).0, 0);
    let (code, stdout, _) = cli(&["stop", "api"], &cfg);
    assert_eq!(code, 0);
    assert!(stdout.contains("already stopped"), "{stdout}");
}

#[test]
fn starting_a_running_service_is_refused() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let (code, _, stderr) = cli(&["start", "api"], &cfg);
    assert_eq!(code, 1);
    assert!(stderr.contains("already running"), "{stderr}");
}

#[test]
fn per_service_commands_need_a_daemon() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    let (code, _, stderr) = cli(&["stop", "api"], &cfg);
    assert_eq!(code, 1);
    assert!(stderr.contains("no daemon is running"), "{stderr}");
}

#[test]
fn an_unknown_service_is_rejected_by_the_daemon() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let (code, _, stderr) = cli(&["restart", "nope"], &cfg);
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown service"), "{stderr}");
    assert!(stderr.contains("api"), "{stderr}");
}

/// The pid the daemon reports for `api`, or 0 when it is not running.
fn pid_of(daemon: &Daemon) -> i64 {
    let (_, stdout, _) = cli(&["status", "--json"], &daemon.config);
    let parsed: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    parsed["services"][0]["pid"].as_i64().unwrap_or(0)
}

#[test]
fn a_foreground_daemon_stops_on_sigterm() {
    let dir = TempDir::new().unwrap();
    let svc = resident(dir.path(), "api.sh");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            svc.display()
        ),
    );

    let mut child = Command::new(binary())
        .arg("daemon")
        .arg("--config")
        .arg(&cfg)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn the daemon");

    let socket = dir.path().join(".servicrab/daemon.sock");
    let deadline = Instant::now() + CEILING;
    while !socket.exists() {
        assert!(Instant::now() < deadline, "the socket never appeared");
        std::thread::sleep(Duration::from_millis(50));
    }

    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    assert_eq!(wait_bounded(&mut child), 0);
    assert!(!socket.exists(), "the socket outlived the daemon");
}
