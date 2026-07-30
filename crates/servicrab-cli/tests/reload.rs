//! Integration tests for `servicrab reload` (config hot-reload).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

/// A long-lived service that exits cleanly on SIGTERM.
fn resident(dir: &Path, name: &str) -> PathBuf {
    script(
        dir,
        name,
        "trap 'exit 0' TERM INT\necho up\nwhile true; do sleep 0.2; done",
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

    fn reload(&self) -> (i32, String, String) {
        cli(&["reload"], &self.config)
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

    /// The pid column of one service, if it currently has one.
    fn pid_of(&self, service: &str) -> Option<u32> {
        pid_in(&self.status(), service)
    }

    fn wait_for_pid_change(&self, service: &str, was: Option<u32>) -> Option<u32> {
        let deadline = Instant::now() + CEILING;
        loop {
            let now = self.pid_of(service);
            if now.is_some() && now != was {
                return now;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {service} to be restarted; last status:\n{}",
                    self.status()
                );
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

/// Pull one service's pid out of a `status` table.
fn pid_in(status: &str, service: &str) -> Option<u32> {
    status
        .lines()
        .find(|line| line.split_whitespace().next() == Some(service))
        .and_then(|line| line.split_whitespace().nth(2))
        .and_then(|pid| pid.parse().ok())
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

#[test]
fn a_reload_without_changes_reports_no_changes() {
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let worker = resident(dir.path(), "worker.sh");
    let cfg = write_config(dir.path(), &two_service_config(&api, &worker));

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("both services to run", |s| {
        s.matches("running").count() == 2
    });
    let before = daemon.pid_of("api");

    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("no changes"), "{stdout}");
    assert_eq!(
        daemon.pid_of("api"),
        before,
        "an untouched service restarted"
    );
}

#[test]
fn a_reload_starts_an_added_service() {
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let worker = resident(dir.path(), "worker.sh");
    let cfg = write_config(
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
            api.display()
        ),
    );

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));
    let api_pid = daemon.pid_of("api");

    fs::write(&cfg, two_service_config(&api, &worker)).unwrap();
    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("1 added"), "{stdout}");

    let status = daemon.wait_for_status("worker to run", |s| {
        pid_in(s, "worker").is_some() && s.matches("running").count() == 2
    });
    assert!(status.contains("worker"), "{status}");
    // The service that was already up is left alone.
    assert_eq!(daemon.pid_of("api"), api_pid);
}

#[test]
fn a_reload_stops_a_removed_service() {
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let worker = resident(dir.path(), "worker.sh");
    let cfg = write_config(dir.path(), &two_service_config(&api, &worker));

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("both services to run", |s| {
        s.matches("running").count() == 2
    });
    let api_pid = daemon.pid_of("api");

    fs::write(
        &cfg,
        format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            api.display()
        ),
    )
    .unwrap();

    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("1 removed"), "{stdout}");

    let status = daemon.wait_for_status("worker to disappear", |s| !s.contains("worker"));
    assert!(status.contains("api"), "{status}");
    assert_eq!(daemon.pid_of("api"), api_pid);
}

#[test]
fn a_reload_restarts_a_changed_service() {
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let worker = resident(dir.path(), "worker.sh");
    let cfg = write_config(dir.path(), &two_service_config(&api, &worker));

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("both services to run", |s| {
        s.matches("running").count() == 2
    });
    let api_pid = daemon.pid_of("api");
    let worker_pid = daemon.pid_of("worker");

    fs::write(
        &cfg,
        format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
env = {{ EDITED = "yes" }}
[services.worker]
command = ["{}"]
restart = "always"
"#,
            api.display(),
            worker.display()
        ),
    )
    .unwrap();

    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("1 changed"), "{stdout}");

    let fresh = daemon.wait_for_pid_change("api", api_pid);
    assert_ne!(fresh, api_pid);
    // Only the edited service is restarted.
    assert_eq!(daemon.pid_of("worker"), worker_pid);
}

#[test]
fn an_invalid_config_is_refused_and_the_stack_keeps_running() {
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let worker = resident(dir.path(), "worker.sh");
    let cfg = write_config(dir.path(), &two_service_config(&api, &worker));

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("both services to run", |s| {
        s.matches("running").count() == 2
    });
    let api_pid = daemon.pid_of("api");

    fs::write(
        &cfg,
        format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
depends_on = ["ghost"]
"#,
            api.display()
        ),
    )
    .unwrap();

    let (code, _, stderr) = daemon.reload();
    assert_eq!(code, 1, "a broken config was accepted: {stderr}");
    assert!(stderr.contains("ghost"), "{stderr}");

    // The daemon is untouched, so restoring the file makes it work again.
    fs::write(&cfg, two_service_config(&api, &worker)).unwrap();
    assert_eq!(daemon.pid_of("api"), api_pid);
    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("no changes"), "{stdout}");
}

#[test]
fn reload_needs_a_running_daemon() {
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let cfg = write_config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
"#,
            api.display()
        ),
    );

    let (code, _, stderr) = cli(&["reload"], &cfg);
    assert_ne!(code, 0);
    assert!(stderr.contains("no daemon is running"), "{stderr}");
}

#[test]
fn a_reloaded_service_can_be_controlled_by_name() {
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let worker = resident(dir.path(), "worker.sh");
    let cfg = write_config(
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
            api.display()
        ),
    );

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    fs::write(&cfg, two_service_config(&api, &worker)).unwrap();
    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    daemon.wait_for_status("worker to run", |s| pid_in(s, "worker").is_some());

    // The daemon's idea of which services exist has to follow the reload.
    let (code, stdout, stderr) = cli(&["restart", "worker"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");

    // …and the service that is gone is no longer accepted.
    fs::write(
        &cfg,
        format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            api.display()
        ),
    )
    .unwrap();
    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    daemon.wait_for_status("worker to disappear", |s| !s.contains("worker"));

    let (code, _, stderr) = cli(&["restart", "worker"], &cfg);
    assert_ne!(code, 0);
    assert!(stderr.contains("unknown service"), "{stderr}");
}
