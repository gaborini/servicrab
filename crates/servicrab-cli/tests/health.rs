//! Integration tests for health checks and readiness gating.
//!
//! Every fixture is generated into a [`TempDir`] at test time; all waits are
//! bounded so a hung supervisor fails the test instead of stalling CI.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Upper bound for any wait in this file.
const CEILING: Duration = Duration::from_secs(15);

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

/// Run a servicrab subcommand to completion: `(exit code, stdout, stderr)`.
fn servicrab(command: &str, config_path: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(binary())
        .arg(command)
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

fn up(config_path: &Path, args: &[&str]) -> (i32, String, String) {
    servicrab("up", config_path, args)
}

/// Wait until `path` exists and is non-empty.
fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + CEILING;
    while Instant::now() < deadline {
        if path.exists() && fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}

#[test]
fn a_dependent_waits_for_its_dependency_to_become_healthy() {
    let dir = TempDir::new().unwrap();
    let ready = dir.path().join("db.ready");
    let log = dir.path().join("order.txt");

    // The dependency only reports itself ready after a delay, so a dependent
    // that started on "process is up" alone would beat the marker.
    script(
        dir.path(),
        "db.sh",
        &format!("sleep 0.6\necho ready > {}\nsleep 5", ready.display()),
    );
    script(
        dir.path(),
        "probe.sh",
        &format!("test -f {}", ready.display()),
    );
    script(
        dir.path(),
        "api.sh",
        &format!(
            "if [ -f {} ]; then echo after >> {}; else echo before >> {}; fi",
            ready.display(),
            log.display(),
            log.display()
        ),
    );

    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{api}"]
depends_on = ["db"]

[services.db]
command = ["{db}"]

[services.db.health]
command = ["{probe}"]
interval = "100ms"
start_period = "5s"
"#,
            api = dir.path().join("api.sh").display(),
            db = dir.path().join("db.sh").display(),
            probe = dir.path().join("probe.sh").display(),
        ),
    );

    let (_code, _stdout, stderr) = up(&cfg, &["--abort-on-failure"]);
    let order = fs::read_to_string(&log).expect("order log");
    assert_eq!(order.trim(), "after", "stderr: {stderr}");
    assert!(stderr.contains("healthy"), "stderr: {stderr}");
}

#[test]
fn a_dependent_is_skipped_when_its_dependency_never_becomes_healthy() {
    let dir = TempDir::new().unwrap();
    let started = dir.path().join("api.started");

    script(dir.path(), "db.sh", "sleep 30");
    script(dir.path(), "probe.sh", "exit 1");
    script(
        dir.path(),
        "api.sh",
        &format!("echo started > {}", started.display()),
    );

    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{api}"]
depends_on = ["db"]

[services.db]
command = ["{db}"]
restart = "never"

[services.db.health]
command = ["{probe}"]
interval = "100ms"
retries = 2
"#,
            api = dir.path().join("api.sh").display(),
            db = dir.path().join("db.sh").display(),
            probe = dir.path().join("probe.sh").display(),
        ),
    );

    let (code, _stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(stderr.contains("unhealthy"), "stderr: {stderr}");
    assert!(
        stderr.contains("skipped") || stderr.contains("never became available"),
        "stderr: {stderr}"
    );
    assert!(
        !started.exists(),
        "the dependent must not start when its dependency never becomes healthy"
    );
}

/// The other half of the test above: `service_started` is the way to opt out of
/// health gating for one edge — a log shipper needs the database process to
/// exist, not to be serving queries — so the same never-healthy dependency does
/// not hold this dependent back.
#[test]
fn service_started_ignores_the_dependencys_health_check() {
    let dir = TempDir::new().unwrap();
    let started = dir.path().join("api.started");

    script(dir.path(), "db.sh", "sleep 30");
    script(dir.path(), "probe.sh", "exit 1");
    script(
        dir.path(),
        "api.sh",
        &format!("echo started > {}", started.display()),
    );

    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{api}"]
depends_on = {{ db = {{ condition = "service_started" }} }}

[services.db]
command = ["{db}"]
restart = "never"

[services.db.health]
command = ["{probe}"]
interval = "100ms"
retries = 2
"#,
            api = dir.path().join("api.sh").display(),
            db = dir.path().join("db.sh").display(),
            probe = dir.path().join("probe.sh").display(),
        ),
    );

    let (_code, _stdout, stderr) = up(&cfg, &[]);
    assert!(
        started.exists(),
        "the dependent should not wait for a health check it opted out of: {stderr}"
    );
}

#[test]
fn an_unhealthy_service_is_restarted() {
    let dir = TempDir::new().unwrap();
    let runs = dir.path().join("runs.txt");

    // The probe answers "unhealthy" for the first run of the service and
    // "healthy" from the second one on.  It keys off the service's own run
    // log rather than off its own invocation count, so the answer does not
    // depend on how many times it happens to be called per run.
    script(
        dir.path(),
        "svc.sh",
        &format!("echo run >> {}\nsleep 10", runs.display()),
    );
    script(
        dir.path(),
        "probe.sh",
        &format!(
            "[ \"$(wc -l < {runs} 2>/dev/null || echo 0)\" -ge 2 ]",
            runs = runs.display()
        ),
    );

    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.svc]
command = ["{svc}"]
restart = "on-failure"
restart_delay = "100ms"
max_restarts = 3
shutdown_timeout = "2s"

[services.svc.health]
command = ["{probe}"]
interval = "150ms"
retries = 1
"#,
            svc = dir.path().join("svc.sh").display(),
            probe = dir.path().join("probe.sh").display(),
        ),
    );

    let mut child = Command::new(binary())
        .arg("up")
        .arg("--config")
        .arg(&cfg)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn servicrab up");

    // Wait until the service has been started twice: once initially and once
    // after the health check stopped it.
    let deadline = Instant::now() + CEILING;
    loop {
        let runs_seen = fs::read_to_string(&runs)
            .map(|s| s.lines().count())
            .unwrap_or(0);
        if runs_seen >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the unhealthy service was not restarted (runs so far: {runs_seen})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn on_unhealthy_ignore_keeps_the_process_running() {
    let dir = TempDir::new().unwrap();
    let runs = dir.path().join("runs.txt");
    let done = dir.path().join("done.txt");

    script(
        dir.path(),
        "svc.sh",
        &format!(
            "echo run >> {}\nsleep 1.5\necho done > {}",
            runs.display(),
            done.display()
        ),
    );
    script(dir.path(), "probe.sh", "exit 1");

    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.svc]
command = ["{svc}"]
restart = "never"

[services.svc.health]
command = ["{probe}"]
interval = "100ms"
retries = 1
on_unhealthy = "ignore"
"#,
            svc = dir.path().join("svc.sh").display(),
            probe = dir.path().join("probe.sh").display(),
        ),
    );

    let (code, _stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    wait_for_file(&done);
    let run_count = fs::read_to_string(&runs).unwrap().lines().count();
    assert_eq!(run_count, 1, "the service must not have been restarted");
    assert!(stderr.contains("unhealthy"), "stderr: {stderr}");
}

#[test]
fn a_tcp_probe_reports_a_listening_service_as_healthy() {
    let dir = TempDir::new().unwrap();
    // Pick a port by binding and releasing it immediately.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    // A tiny listener: `nc` is not guaranteed to exist everywhere, so use the
    // same Rust binary that is already available — python3 is present on both
    // CI images and macOS/Linux dev machines.
    script(
        dir.path(),
        "listen.sh",
        &format!(
            "exec python3 -c \"import socket,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', {port})); s.listen(8); time.sleep(10)\""
        ),
    );

    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.listener]
command = ["{listen}"]
shutdown_timeout = "2s"

[services.listener.health]
tcp = "127.0.0.1:{port}"
interval = "100ms"
"#,
            listen = dir.path().join("listen.sh").display(),
        ),
    );

    let mut child = Command::new(binary())
        .arg("up")
        .arg("--config")
        .arg(&cfg)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn servicrab up");

    // Give the probe a moment to succeed, then stop the stack and check the
    // reported status.
    std::thread::sleep(Duration::from_millis(1500));
    let _ = child.kill();
    let output = child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("healthy"), "stderr: {stderr}");
}

#[test]
fn list_reports_the_configured_probe() {
    let dir = TempDir::new().unwrap();
    let cfg = config(
        dir.path(),
        r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["echo", "hi"]

[services.api.health]
http = "http://127.0.0.1:8080/healthz"
interval = "3s"
"#,
    );

    let (code, stdout, stderr) = servicrab("list", &cfg, &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("health: http http://127.0.0.1:8080/healthz every 3s"),
        "stdout: {stdout}"
    );

    let (code, stdout, _) = servicrab("list", &cfg, &["--json"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("\"health\": \"http http://127.0.0.1:8080/healthz\""),
        "stdout: {stdout}"
    );
}

#[test]
fn an_invalid_health_block_fails_check() {
    let dir = TempDir::new().unwrap();
    let cfg = config(
        dir.path(),
        r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["echo", "hi"]

[services.api.health]
interval = "3s"
"#,
    );

    let (code, _stdout, stderr) = servicrab("check", &cfg, &[]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("exactly one of `command`, `http` or `tcp`"),
        "stderr: {stderr}"
    );
}
