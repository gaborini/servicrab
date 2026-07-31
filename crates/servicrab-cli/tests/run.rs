//! Integration tests for `servicrab run <SERVICE>`.
//!
//! All fixtures are shell scripts generated into a [`TempDir`] at test time, so
//! nothing depends on repository-specific absolute paths.  Every test uses
//! short, bounded timeouts and asserts that no supervised process is left
//! behind.

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
const CEILING: Duration = Duration::from_secs(5);

// ── fixtures ───────────────────────────────────────────────────────────────

/// Write an executable `/bin/sh` fixture script into `dir`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Write a `servicrab.toml` into `dir` and return its path.
fn config(dir: &Path, toml: &str) -> PathBuf {
    let path = dir.join("servicrab.toml");
    fs::write(&path, toml).unwrap();
    path
}

fn binary() -> PathBuf {
    assert_cmd::cargo::cargo_bin("servicrab")
}

/// Run `servicrab run <service>` to completion and return
/// `(exit code, stdout, stderr)`.
fn run_service(config_path: &Path, service: &str, extra: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(binary());
    cmd.arg("run")
        .arg(service)
        .arg("--config")
        .arg(config_path)
        .args(extra)
        .env_remove("RUST_LOG");
    let output = cmd.output().expect("failed to run servicrab");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Spawn `servicrab run <service>` in the background, capturing stdout.
fn spawn_service(config_path: &Path, service: &str, stdout: Stdio) -> Child {
    spawn_service_with(config_path, service, stdout, Stdio::null())
}

/// Like [`spawn_service`] but with an explicit stderr redirection, so a test
/// can report what the supervisor said when an assertion fails.
fn spawn_service_with(config_path: &Path, service: &str, stdout: Stdio, stderr: Stdio) -> Child {
    Command::new(binary())
        .arg("run")
        .arg(service)
        .arg("--config")
        .arg(config_path)
        .env_remove("RUST_LOG")
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("failed to spawn servicrab")
}

/// Drain a child's captured stderr without blocking forever.
fn drain_stderr(child: &mut Child) -> String {
    use std::io::Read;
    let mut buf = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut buf);
    }
    buf
}

/// Block until `path` exists (or the ceiling elapses).
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

/// Wait for a child to exit, killing it and failing if it outlives the ceiling.
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

/// How many times a counting fixture ran.
fn run_count(counter: &Path) -> usize {
    fs::read_to_string(counter)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// Wait until the counting fixture has run at least `count` times.
fn wait_for_runs(counter: &Path, count: usize) {
    let deadline = Instant::now() + CEILING;
    while Instant::now() < deadline {
        if run_count(counter) >= count {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "timed out waiting for {count} runs; saw {}",
        run_count(counter)
    );
}

fn is_alive(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None).is_ok()
}

// ── command, cwd, environment, streams ─────────────────────────────────────

#[test]
fn arguments_are_passed_verbatim_without_a_shell() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("argv.txt");
    let argv_sh = script(
        dir.path(),
        "argv.sh",
        &format!("for a in \"$@\"; do echo \"$a\" >> {}; done", out.display()),
    );

    // The second argument contains a space and a glob character; passing it
    // through a shell would split or expand it.
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"argv\"\n\
             [services.argv]\ncommand = [\"{}\", \"one\", \"two three\", \"*\"]\n",
            argv_sh.display()
        ),
    );

    let (code, _, stderr) = run_service(&cfg, "argv", &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let recorded = fs::read_to_string(&out).unwrap();
    assert_eq!(recorded, "one\ntwo three\n*\n");
}

#[test]
fn configured_cwd_is_used() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("work")).unwrap();
    let pwd_sh = script(dir.path(), "pwd.sh", "pwd");

    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"cwd\"\n\
             [services.pwd]\ncommand = [\"{}\"]\ncwd = \"./work\"\n",
            pwd_sh.display()
        ),
    );

    let (code, stdout, stderr) = run_service(&cfg, "pwd", &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.trim().ends_with("work"),
        "expected the service to run in ./work, got {stdout:?}"
    );
}

#[test]
fn environment_override_order_is_preserved_at_runtime() {
    let dir = TempDir::new().unwrap();
    let env_sh = script(
        dir.path(),
        "env.sh",
        "echo \"P1=$P1\"; echo \"P2=$P2\"; echo \"P3=$P3\"",
    );

    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"envorder\"\n\
             [project.env]\nP2 = \"project\"\nP3 = \"project\"\n\
             [services.env]\ncommand = [\"{}\"]\n\
             [services.env.env]\nP3 = \"service\"\n",
            env_sh.display()
        ),
    );

    // P1 comes from the process environment, P2 from the project, P3 from the
    // service — later layers must win.
    let output = Command::new(binary())
        .arg("run")
        .arg("env")
        .arg("--config")
        .arg(&cfg)
        .env("P1", "process")
        .env("P2", "process")
        .env("P3", "process")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("P1=process"), "{stdout}");
    assert!(stdout.contains("P2=project"), "{stdout}");
    assert!(stdout.contains("P3=service"), "{stdout}");
}

#[test]
fn stdout_and_stderr_are_forwarded() {
    let dir = TempDir::new().unwrap();
    let sh = script(
        dir.path(),
        "streams.sh",
        "echo to-stdout; echo to-stderr >&2",
    );
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"streams\"\n\
             [services.streams]\ncommand = [\"{}\"]\n",
            sh.display()
        ),
    );

    let (code, stdout, stderr) = run_service(&cfg, "streams", &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("to-stdout"), "stdout was {stdout:?}");
    assert!(stderr.contains("to-stderr"), "stderr was {stderr:?}");
}

#[test]
fn zero_exit_code_is_propagated() {
    let dir = TempDir::new().unwrap();
    let sh = script(dir.path(), "ok.sh", "exit 0");
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"ok\"\n\
             [services.ok]\ncommand = [\"{}\"]\n",
            sh.display()
        ),
    );
    assert_eq!(run_service(&cfg, "ok", &[]).0, 0);
}

#[test]
fn non_zero_exit_code_is_propagated() {
    let dir = TempDir::new().unwrap();
    let sh = script(dir.path(), "fail.sh", "exit 42");
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"fail\"\n\
             [services.fail]\ncommand = [\"{}\"]\n",
            sh.display()
        ),
    );
    assert_eq!(run_service(&cfg, "fail", &[]).0, 42);
}

#[test]
fn unknown_service_is_reported_clearly() {
    let dir = TempDir::new().unwrap();
    let sh = script(dir.path(), "ok.sh", "exit 0");
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"ok\"\n\
             [services.ok]\ncommand = [\"{}\"]\n",
            sh.display()
        ),
    );

    let (code, _, stderr) = run_service(&cfg, "missing", &[]);
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown service"), "{stderr}");
    assert!(stderr.contains("ok"), "error should list known services");
}

// ── restart policies ───────────────────────────────────────────────────────

/// Build a config whose single service counts its runs into `counter`.
fn counting_config(dir: &Path, counter: &Path, body: &str, extra: &str) -> PathBuf {
    let sh = script(
        dir,
        "count.sh",
        &format!("echo run >> {}\n{body}", counter.display()),
    );
    config(
        dir,
        &format!(
            "version = 1\n\
             [project]\nname = \"counting\"\n\
             [services.svc]\ncommand = [\"{}\"]\n{extra}",
            sh.display()
        ),
    )
}

#[test]
fn never_policy_does_not_restart() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("count.txt");
    let cfg = counting_config(dir.path(), &counter, "exit 1", "restart = \"never\"\n");

    let (code, _, _) = run_service(&cfg, "svc", &[]);
    assert_eq!(code, 1);
    assert_eq!(run_count(&counter), 1, "never must not restart");
}

#[test]
fn on_failure_does_not_restart_successful_exit() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("count.txt");
    let cfg = counting_config(
        dir.path(),
        &counter,
        "exit 0",
        "restart = \"on-failure\"\nrestart_delay = \"100ms\"\nrestart_max_delay = \"100ms\"\nmax_restarts = 2\n",
    );

    let (code, _, _) = run_service(&cfg, "svc", &[]);
    assert_eq!(code, 0);
    assert_eq!(run_count(&counter), 1, "a clean exit must not restart");
}

#[test]
fn on_failure_restarts_failures_until_the_limit() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("count.txt");
    let cfg = counting_config(
        dir.path(),
        &counter,
        "exit 1",
        "restart = \"on-failure\"\nrestart_delay = \"100ms\"\nrestart_max_delay = \"100ms\"\nmax_restarts = 2\n",
    );

    let (code, _, stderr) = run_service(&cfg, "svc", &[]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("giving up after 2 restart attempt"),
        "restart limit should be reported: {stderr}"
    );
    assert_eq!(run_count(&counter), 3, "1 initial run + 2 restarts");
}

#[test]
fn always_restarts_successful_exits() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("count.txt");
    let cfg = counting_config(
        dir.path(),
        &counter,
        "exit 0",
        "restart = \"always\"\nrestart_delay = \"100ms\"\nrestart_max_delay = \"100ms\"\nmax_restarts = 2\n",
    );

    let (code, _, _) = run_service(&cfg, "svc", &[]);
    assert_eq!(code, 1, "the restart limit is eventually exhausted");
    assert_eq!(run_count(&counter), 3, "always restarts a clean exit too");
}

#[test]
fn no_restart_flag_overrides_the_configured_policy() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("count.txt");
    let cfg = counting_config(
        dir.path(),
        &counter,
        "exit 1",
        "restart = \"always\"\nrestart_delay = \"100ms\"\nrestart_max_delay = \"100ms\"\nmax_restarts = 5\n",
    );

    let (code, _, _) = run_service(&cfg, "svc", &["--no-restart"]);
    assert_eq!(code, 1);
    assert_eq!(run_count(&counter), 1, "--no-restart must win");
}

#[test]
fn zero_max_restarts_means_unlimited_restarts() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("count.txt");
    let cfg = counting_config(
        dir.path(),
        &counter,
        "exit 1",
        "restart = \"on-failure\"\nrestart_delay = \"100ms\"\nrestart_max_delay = \"100ms\"\nmax_restarts = 0\n",
    );

    let mut child = spawn_service_with(&cfg, "svc", Stdio::null(), Stdio::piped());

    // More runs than the default budget of 10 would ever allow, so the count
    // rules out both a finite limit and the old "give up on the first failure"
    // reading of `max_restarts = 0`.
    wait_for_runs(&counter, 12);
    assert!(
        is_alive(child.id() as i32),
        "the supervisor must not have given up"
    );

    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    let code = wait_bounded(&mut child);
    let stderr = drain_stderr(&mut child);
    assert_eq!(code, 130, "supervisor said: {stderr}");
    assert!(
        !stderr.contains("giving up"),
        "the restart limit must never be reached: {stderr}"
    );
}

// ── shutdown ───────────────────────────────────────────────────────────────

#[test]
fn interrupt_triggers_graceful_shutdown() {
    let dir = TempDir::new().unwrap();
    let marker = dir.path().join("started.txt");
    let sh = script(
        dir.path(),
        "sleeper.sh",
        &format!(
            "echo started > {}\nwhile true; do sleep 0.1; done",
            marker.display()
        ),
    );
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"sleeper\"\n\
             [services.sleeper]\ncommand = [\"{}\"]\n\
             restart = \"always\"\nshutdown_timeout = \"2s\"\n",
            sh.display()
        ),
    );

    let mut child = spawn_service_with(&cfg, "sleeper", Stdio::null(), Stdio::piped());
    wait_for_file(&marker);

    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    let code = wait_bounded(&mut child);
    let stderr = drain_stderr(&mut child);

    // 130 == 128 + SIGINT; `restart = "always"` must not resurrect the service
    // after an explicit user shutdown.
    assert_eq!(code, 130, "supervisor said: {stderr}");
}

#[test]
fn shutdown_timeout_escalates_to_sigkill() {
    let dir = TempDir::new().unwrap();
    let marker = dir.path().join("started.txt");
    let sh = script(
        dir.path(),
        "stubborn.sh",
        &format!(
            "trap '' TERM\necho started > {}\nwhile true; do sleep 0.1; done",
            marker.display()
        ),
    );
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"stubborn\"\n\
             [services.stubborn]\ncommand = [\"{}\"]\n\
             shutdown_signal = \"term\"\nshutdown_timeout = \"500ms\"\n",
            sh.display()
        ),
    );

    let mut child = spawn_service_with(&cfg, "stubborn", Stdio::null(), Stdio::piped());
    wait_for_file(&marker);

    let started = Instant::now();
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let code = wait_bounded(&mut child);
    let elapsed = started.elapsed();
    let stderr = drain_stderr(&mut child);

    // 143 == 128 + SIGTERM.
    assert_eq!(code, 143, "supervisor said: {stderr}");
    assert!(
        elapsed >= Duration::from_millis(400),
        "the supervisor should have waited out the shutdown timeout, took {elapsed:?}"
    );
    assert!(
        elapsed < CEILING,
        "the escalation to SIGKILL took too long: {elapsed:?}"
    );
}

#[test]
fn descendants_do_not_survive_shutdown() {
    let dir = TempDir::new().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    // The direct child is /bin/sh; it spawns a long sleep as a grandchild.
    // Signalling only the direct child would leave the sleep running.
    let sh = script(
        dir.path(),
        "parent.sh",
        &format!("sleep 30 &\necho $! > {}\nwait", pidfile.display()),
    );
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"tree\"\n\
             [services.tree]\ncommand = [\"{}\"]\nshutdown_timeout = \"1s\"\n",
            sh.display()
        ),
    );

    let mut child = spawn_service(&cfg, "tree", Stdio::null());
    wait_for_file(&pidfile);
    let grandchild: i32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(is_alive(grandchild), "fixture grandchild should be running");

    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    wait_bounded(&mut child);

    // Give the kernel a moment to reap the process group.
    let deadline = Instant::now() + Duration::from_secs(2);
    while is_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !is_alive(grandchild),
        "grandchild {grandchild} outlived the supervisor"
    );
}

#[test]
fn no_orphans_remain_after_a_normal_run() {
    let dir = TempDir::new().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    let sh = script(
        dir.path(),
        "spawner.sh",
        &format!("sleep 30 &\necho $! > {}\nexit 0", pidfile.display()),
    );
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"orphans\"\n\
             [services.spawner]\ncommand = [\"{}\"]\n",
            sh.display()
        ),
    );

    let (code, _, _) = run_service(&cfg, "spawner", &[]);
    assert_eq!(code, 0);

    let grandchild: i32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while is_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !is_alive(grandchild),
        "background descendant {grandchild} was left running"
    );
}

#[test]
fn missing_executable_reports_a_spawn_failure() {
    let dir = TempDir::new().unwrap();
    let cfg = config(
        dir.path(),
        &format!(
            "version = 1\n\
             [project]\nname = \"missing\"\n\
             [services.ghost]\ncommand = [\"{}\"]\n",
            dir.path().join("definitely-not-here").display()
        ),
    );

    let (code, _, stderr) = run_service(&cfg, "ghost", &[]);
    assert_eq!(code, 1);
    assert!(stderr.contains("failed to spawn"), "{stderr}");
}
