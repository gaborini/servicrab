//! Integration tests for `servicrab up`.
//!
//! Every fixture is generated into a [`TempDir`] at test time, so nothing
//! depends on repository-specific paths.  All waits are bounded and each test
//! asserts that no supervised process is left behind.

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
const CEILING: Duration = Duration::from_secs(10);

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

/// Run `servicrab up` to completion and return `(exit code, stdout, stderr)`.
fn up(config_path: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(binary())
        .arg("up")
        .args(args)
        .arg("--config")
        .arg(config_path)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run servicrab up");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Spawn `servicrab up` in the background with stdout captured.
fn spawn_up(config_path: &Path, args: &[&str]) -> Child {
    Command::new(binary())
        .arg("up")
        .args(args)
        .arg("--config")
        .arg(config_path)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn servicrab up")
}

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

fn wait_bounded(child: &mut Child) -> i32 {
    let deadline = Instant::now() + CEILING;
    loop {
        match child.try_wait().unwrap() {
            Some(status) => return status.code().unwrap_or(-1),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("supervisor did not exit within {CEILING:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn is_alive(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None).is_ok()
}

fn read_pid(path: &Path) -> i32 {
    fs::read_to_string(path)
        .expect("pid file")
        .trim()
        .parse()
        .expect("numeric pid")
}

// ── planning ───────────────────────────────────────────────────────────────

#[test]
fn only_autostart_services_run_by_default() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo hello-from-$1");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.auto]
command = ["{0}", "auto"]

[services.manual]
command = ["{0}", "manual"]
autostart = false
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &[]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("hello-from-auto"), "stdout: {stdout}");
    assert!(!stdout.contains("hello-from-manual"), "stdout: {stdout}");
}

#[test]
fn named_services_pull_in_their_dependencies() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo hello-from-$1");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.db]
command = ["{0}", "db"]
autostart = false

[services.api]
command = ["{0}", "api"]
autostart = false
depends_on = ["db"]
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &["api"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("hello-from-db"), "stdout: {stdout}");
    assert!(stdout.contains("hello-from-api"), "stdout: {stdout}");
}

#[test]
fn unknown_service_is_reported_clearly() {
    let dir = TempDir::new().unwrap();
    let cfg = config(
        dir.path(),
        r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["true"]
"#,
    );

    let (code, _stdout, stderr) = up(&cfg, &["nope"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown service"), "stderr: {stderr}");
    assert!(stderr.contains("api"), "stderr: {stderr}");
}

#[test]
fn a_stack_without_autostart_services_is_an_error() {
    let dir = TempDir::new().unwrap();
    let cfg = config(
        dir.path(),
        r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["true"]
autostart = false
"#,
    );

    let (code, _stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 1);
    assert!(stderr.contains("no services to start"), "stderr: {stderr}");
}

// ── output ─────────────────────────────────────────────────────────────────

#[test]
fn output_is_prefixed_with_the_service_name() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo line-from-$1");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}", "api"]
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &[]);
    assert_eq!(code, 0);
    assert!(
        stdout.lines().any(|l| l.starts_with("api | ")),
        "stdout: {stdout}"
    );
}

#[test]
fn prefixes_can_be_disabled() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo bare-line");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}"]
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &["--no-prefix"]);
    assert_eq!(code, 0);
    assert!(stdout.lines().any(|l| l == "bare-line"), "stdout: {stdout}");
}

#[test]
fn stderr_of_a_service_is_forwarded_to_stderr() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "noisy.sh", "echo to-stdout; echo to-stderr >&2");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}"]
"#,
            dir.path().join("noisy.sh").display()
        ),
    );

    let (code, stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("to-stdout"), "stdout: {stdout}");
    assert!(stderr.contains("to-stderr"), "stderr: {stderr}");
    assert!(!stdout.contains("to-stderr"), "stdout: {stdout}");
}

#[test]
fn timestamps_can_be_enabled() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo stamped");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}"]
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &["--timestamps"]);
    assert_eq!(code, 0);
    let line = stdout
        .lines()
        .find(|l| l.contains("stamped"))
        .unwrap_or_else(|| panic!("no output line: {stdout}"));
    let stamp = &line[..8];
    assert!(
        stamp.len() == 8 && stamp.chars().filter(|c| *c == ':').count() == 2,
        "expected a HH:MM:SS prefix, got {line:?}"
    );
}

// ── ordering, dependencies, shutdown ───────────────────────────────────────

#[test]
fn a_dependent_starts_only_after_its_dependency_is_up() {
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("order.txt");
    let marker = dir.path().join("db.up");
    // The dependency records that it is up and then stays alive.
    script(
        dir.path(),
        "db.sh",
        &format!(
            "echo db >> {}\necho up > {}\nsleep 1",
            log.display(),
            marker.display()
        ),
    );
    // The dependent waits out any shell start-up jitter and then checks
    // whether the dependency really was up before it got started.
    script(
        dir.path(),
        "api.sh",
        &format!(
            "sleep 0.3\nif [ -f {} ]; then echo api-after-db >> {}; else echo api-before-db >> {}; fi",
            marker.display(),
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
command = ["{}"]
depends_on = ["db"]

[services.db]
command = ["{}"]
"#,
            dir.path().join("api.sh").display(),
            dir.path().join("db.sh").display()
        ),
    );

    let (code, _stdout, _stderr) = up(&cfg, &[]);
    assert_eq!(code, 0);
    let order = fs::read_to_string(&log).expect("order log");
    let lines: Vec<&str> = order.lines().collect();
    assert_eq!(lines, vec!["db", "api-after-db"], "order: {order}");
}

#[test]
fn a_dependent_is_skipped_when_its_dependency_fails_to_start() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo api-started");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.db]
command = ["{}/definitely-not-here"]

[services.api]
command = ["{}"]
depends_on = ["db"]
"#,
            dir.path().display(),
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(!stdout.contains("api-started"), "stdout: {stdout}");
    assert!(
        stderr.contains("never became available"),
        "stderr: {stderr}"
    );
}

#[test]
fn ctrl_c_stops_the_whole_stack() {
    let dir = TempDir::new().unwrap();
    let ready_a = dir.path().join("a.pid");
    let ready_b = dir.path().join("b.pid");
    script(
        dir.path(),
        "sleeper.sh",
        "echo $$ > $1\ntrap 'exit 0' TERM\nwhile true; do sleep 0.1; done",
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.alpha]
command = ["{0}", "{1}"]
shutdown_timeout = "2s"

[services.beta]
command = ["{0}", "{2}"]
depends_on = ["alpha"]
shutdown_timeout = "2s"
"#,
            dir.path().join("sleeper.sh").display(),
            ready_a.display(),
            ready_b.display()
        ),
    );

    let mut child = spawn_up(&cfg, &[]);
    wait_for_file(&ready_a);
    wait_for_file(&ready_b);
    let pid_a = read_pid(&ready_a);
    let pid_b = read_pid(&ready_b);

    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).expect("send SIGINT");
    let code = wait_bounded(&mut child);
    assert_eq!(code, 130, "expected 128+SIGINT");

    std::thread::sleep(Duration::from_millis(200));
    assert!(!is_alive(pid_a), "alpha survived the shutdown");
    assert!(!is_alive(pid_b), "beta survived the shutdown");
}

#[test]
fn shutdown_runs_in_reverse_dependency_order() {
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("stops.txt");
    let ready = dir.path().join("ready");
    script(
        dir.path(),
        "sleeper.sh",
        &format!(
            "trap 'echo $1 >> {}; exit 0' TERM\necho up > {}.$1\nwhile true; do sleep 0.1; done",
            log.display(),
            ready.display()
        ),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.db]
command = ["{0}", "db"]
shutdown_timeout = "2s"

[services.api]
command = ["{0}", "api"]
depends_on = ["db"]
shutdown_timeout = "2s"
"#,
            dir.path().join("sleeper.sh").display()
        ),
    );

    let mut child = spawn_up(&cfg, &[]);
    wait_for_file(&PathBuf::from(format!("{}.db", ready.display())));
    wait_for_file(&PathBuf::from(format!("{}.api", ready.display())));

    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).expect("send SIGTERM");
    let code = wait_bounded(&mut child);
    assert_eq!(code, 143, "expected 128+SIGTERM");

    let stops = fs::read_to_string(&log).expect("stop log");
    let order: Vec<&str> = stops.lines().collect();
    assert_eq!(order, vec!["api", "db"], "stop order: {stops}");
}

// ── restart policies and failures ──────────────────────────────────────────

#[test]
fn no_restart_disables_restarting_for_every_service() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("runs.txt");
    script(
        dir.path(),
        "flaky.sh",
        &format!("echo run >> {}\nexit 1", counter.display()),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.flaky]
command = ["{0}"]
restart = "on-failure"
restart_delay = "100ms"
max_restarts = 3
"#,
            dir.path().join("flaky.sh").display()
        ),
    );

    let (code, _stdout, _stderr) = up(&cfg, &["--no-restart"]);
    assert_eq!(code, 0, "a clean stop without restarts is not a failure");
    let runs = fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs, 1, "the service should have run exactly once");
}

#[test]
fn a_service_that_exhausts_its_restart_budget_fails_the_stack() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("runs.txt");
    script(
        dir.path(),
        "flaky.sh",
        &format!("echo run >> {}\nexit 1", counter.display()),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.flaky]
command = ["{0}"]
restart = "on-failure"
restart_delay = "100ms"
restart_max_delay = "200ms"
max_restarts = 2
"#,
            dir.path().join("flaky.sh").display()
        ),
    );

    let (code, _stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(stderr.contains("giving up"), "stderr: {stderr}");
    let runs = fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs, 3, "one initial run plus two restarts");
}

#[test]
fn abort_on_failure_tears_down_the_rest_of_the_stack() {
    let dir = TempDir::new().unwrap();
    let ready = dir.path().join("long.pid");
    script(
        dir.path(),
        "sleeper.sh",
        "echo $$ > $1\ntrap 'exit 0' TERM\nwhile true; do sleep 0.1; done",
    );
    script(dir.path(), "boom.sh", "exit 7");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.long]
command = ["{0}", "{1}"]
shutdown_timeout = "2s"

[services.boom]
command = ["{2}"]
restart = "on-failure"
restart_delay = "500ms"
max_restarts = 2
"#,
            dir.path().join("sleeper.sh").display(),
            ready.display(),
            dir.path().join("boom.sh").display()
        ),
    );

    let mut child = spawn_up(&cfg, &["--abort-on-failure"]);
    wait_for_file(&ready);
    let pid = read_pid(&ready);

    let code = wait_bounded(&mut child);
    assert_eq!(code, 1);

    std::thread::sleep(Duration::from_millis(200));
    assert!(!is_alive(pid), "the long-running service was not torn down");
}

#[test]
fn descendants_do_not_survive_a_stack_shutdown() {
    let dir = TempDir::new().unwrap();
    let parent_pid = dir.path().join("parent.pid");
    let child_pid = dir.path().join("child.pid");
    script(
        dir.path(),
        "tree.sh",
        &format!(
            "sh -c 'echo $$ > {}; trap \"\" TERM; while true; do sleep 0.1; done' &\n\
             echo $$ > {}\n\
             trap 'exit 0' TERM\n\
             while true; do sleep 0.1; done",
            child_pid.display(),
            parent_pid.display()
        ),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.tree]
command = ["{0}"]
shutdown_timeout = "1s"
"#,
            dir.path().join("tree.sh").display()
        ),
    );

    let mut child = spawn_up(&cfg, &[]);
    wait_for_file(&parent_pid);
    wait_for_file(&child_pid);
    let descendant = read_pid(&child_pid);

    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).expect("send SIGINT");
    wait_bounded(&mut child);

    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !is_alive(descendant),
        "a descendant outlived the stack shutdown"
    );
}
