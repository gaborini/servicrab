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

/// Start a servicrab subcommand without waiting for it, for the tests that need
/// to act while it is still running.
fn spawn_cli(args: &[&str], config_path: &Path) -> Child {
    Command::new(binary())
        .args(args)
        .arg("--config")
        .arg(config_path)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to run servicrab")
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
        Self::start_with(config, &[])
    }

    fn start_with(config: &Path, args: &[&str]) -> Self {
        let mut argv = vec!["start"];
        argv.extend_from_slice(args);
        let (code, stdout, stderr) = cli(&argv, config);
        assert_eq!(code, 0, "start failed: {stdout}{stderr}");
        Self {
            config: config.to_path_buf(),
        }
    }

    /// Only the cleanup half: for tests that run `start` themselves because
    /// they are about how it fails.
    fn guard(config: &Path) -> Self {
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

// ── start --wait ───────────────────────────────────────────────────────────

/// A stack whose service can only pass its health check once the test says so.
///
/// The gate is a file this test creates, not a sleep: whether the service is
/// ready is then a fact the test controls rather than a race it hopes to win on
/// a loaded machine. Returns `(config path, gate path)`.
fn gated_by_a_marker(dir: &Path) -> (PathBuf, PathBuf) {
    let gate = dir.join("open-the-gate");
    script(
        dir,
        "db.sh",
        "trap 'exit 0' TERM INT\nwhile true; do sleep 0.1; done",
    );
    script(dir, "probe.sh", &format!("test -f {}", gate.display()));
    let cfg = config(
        dir,
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.db]
command = ["{db}"]
restart = "always"
[services.db.health]
command = ["{probe}"]
interval = "100ms"
start_period = "30s"
"#,
            db = dir.join("db.sh").display(),
            probe = dir.join("probe.sh").display()
        ),
    );
    (cfg, gate)
}

#[test]
fn start_wait_returns_only_once_the_health_check_is_green() {
    let dir = TempDir::new().unwrap();
    let (cfg, gate) = gated_by_a_marker(dir.path());
    let daemon = Daemon::guard(&cfg);

    let mut start = spawn_cli(&["start", "--wait", "--timeout", "20s"], &cfg);

    // The probe cannot pass while the gate is closed, so an exit here is the
    // supervisor claiming a readiness it cannot have observed.
    daemon.wait_for_status("db to be running", |status| status.contains("running"));
    assert!(
        start.try_wait().unwrap().is_none(),
        "--wait returned before the health check could pass"
    );

    fs::write(&gate, "go").unwrap();
    assert_eq!(wait_bounded(&mut start), 0);

    let (code, stdout, _) = cli(&["status"], &cfg);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("healthy"), "not healthy:\n{stdout}");
}

/// The counterpart: plain `start` returns while the stack is still starting.
/// This is the difference the flag exists for, and it keeps the test above from
/// being a tautology.
#[test]
fn start_without_wait_returns_before_the_health_check_is_green() {
    let dir = TempDir::new().unwrap();
    let (cfg, _gate) = gated_by_a_marker(dir.path());

    let _daemon = Daemon::start(&cfg);

    let (_, stdout, _) = cli(&["status"], &cfg);
    assert!(
        !stdout.contains("healthy"),
        "the gate is closed, so the probe cannot have passed:\n{stdout}"
    );
}

#[test]
fn start_wait_gives_up_when_a_service_never_becomes_healthy() {
    let dir = TempDir::new().unwrap();
    let svc = resident(dir.path(), "api.sh");
    script(dir.path(), "probe.sh", "exit 1");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{api}"]
restart = "never"
[services.api.health]
command = ["{probe}"]
interval = "100ms"
start_period = "10s"
retries = 100
"#,
            api = svc.display(),
            probe = dir.path().join("probe.sh").display()
        ),
    );

    // The daemon outlives the failed wait on purpose, so `down` still has work.
    let _daemon = Daemon::guard(&cfg);
    let (code, stdout, stderr) = cli(&["start", "--wait", "--timeout", "1s"], &cfg);

    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stderr.contains("timed out"), "{stderr}");
    assert!(stderr.contains("api"), "{stderr}");

    // A stack that came up wrong is easier to diagnose alive.
    let (code, stdout, _) = cli(&["status"], &cfg);
    assert_eq!(code, 0, "the daemon should still be running:\n{stdout}");
}

#[test]
fn start_wait_reports_a_service_that_gave_up() {
    let dir = TempDir::new().unwrap();
    let svc = script(dir.path(), "api.sh", "exit 3");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "on-failure"
max_restarts = 1
restart_delay = "50ms"
"#,
            svc.display()
        ),
    );

    let _daemon = Daemon::guard(&cfg);
    let (code, stdout, stderr) = cli(&["start", "--wait", "--timeout", "10s"], &cfg);

    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stderr.contains("api"), "{stderr}");
}

#[test]
fn timeout_without_wait_is_rejected() {
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

    let (code, _, stderr) = cli(&["start", "--timeout", "5s"], &cfg);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("--wait"), "{stderr}");
}

/// Connecting to the socket is enough to start and stop every service in the
/// project, so the file permissions are the whole access control.  Leaving them
/// to the umask means a distribution that ships 002 hands that to the group.
#[test]
fn the_socket_is_only_reachable_by_its_owner() {
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

    let mode = fs::metadata(dir.path().join(".servicrab/daemon.sock"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "socket mode is {mode:o}, expected 600");
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

// ── profiles ───────────────────────────────────────────────────────────────

#[test]
fn a_daemon_keeps_its_profiles_across_a_reload() {
    // The profiles live in the daemon process, so a reload has to plan the
    // stack that was started rather than the one a bare `start` would give.
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let seeder = resident(dir.path(), "seeder.sh");
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
[services.seeder]
command = ["{}"]
restart = "always"
profiles = ["dev"]
"#,
            api.display(),
            seeder.display()
        ),
    );

    let daemon = Daemon::start_with(&cfg, &["--profile", "dev"]);
    daemon.wait_for_status("both services to run", |s| {
        s.contains("api") && s.contains("seeder")
    });

    let (code, stdout, stderr) = cli(&["reload"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("no changes"), "{stdout}");

    let status = daemon.status();
    assert!(
        status.contains("seeder"),
        "the reload should not have dropped the profiled service:\n{status}"
    );
}

#[test]
fn a_daemon_without_the_profile_leaves_the_service_out() {
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
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
[services.seeder]
command = ["true"]
profiles = ["dev"]
"#,
            api.display()
        ),
    );

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let status = daemon.status();
    assert!(!status.contains("seeder"), "{status}");

    // And it is not a service the daemon will take commands about, because it
    // is not part of this stack.
    let (code, _, stderr) = cli(&["restart", "seeder"], &cfg);
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown service"), "{stderr}");
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
