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

/// A service that ignores SIGTERM, so its shutdown always runs to the
/// configured timeout and can be observed while it is still in progress.
fn stubborn(dir: &Path, name: &str, pids: &Path) -> PathBuf {
    script(
        dir,
        name,
        &format!(
            "echo $$ >> {}\ntrap '' TERM\nwhile true; do sleep 0.2; done",
            pids.display()
        ),
    )
}

fn is_alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

/// Every pid the stubborn fixture has recorded so far.
fn recorded_pids(path: &Path) -> Vec<i32> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// Poll until `predicate` holds for the recorded pids, or the ceiling elapses.
fn wait_for_pids(path: &Path, what: &str, predicate: impl Fn(&[i32]) -> bool) -> Vec<i32> {
    let deadline = Instant::now() + CEILING;
    loop {
        let pids = recorded_pids(path);
        if predicate(&pids) {
            return pids;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {what}; recorded pids: {pids:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_reload_that_re_adds_a_still_stopping_service_leaves_one_process() {
    // The dropped slot is still winding down when the second reload brings the
    // service back.  Replacing that slot would discard the `stop` channel and
    // the task handle of the process that is on its way out, so the freshly
    // started one would never be signalled and would outlive the daemon.
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let pids = dir.path().join("slow.pids");
    let slow = stubborn(dir.path(), "slow.sh", &pids);

    let with_slow = format!(
        r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
[services.slow]
command = ["{}"]
restart = "always"
shutdown_timeout = "4s"
"#,
        api.display(),
        slow.display()
    );
    let without_slow = format!(
        r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
        api.display()
    );

    let cfg = write_config(dir.path(), &with_slow);
    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("both services to run", |s| {
        s.matches("running").count() == 2
    });
    let first = wait_for_pids(&pids, "the first process to record itself", |p| {
        p.len() == 1
    })[0];

    // Drop the service.  It ignores SIGTERM, so it is still alive — and its
    // slot still retired — while the next reload is applied.
    fs::write(&cfg, &without_slow).unwrap();
    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("1 removed"), "{stdout}");
    assert!(
        is_alive(first),
        "the fixture should still be winding down at this point"
    );

    // …and put it straight back.
    fs::write(&cfg, &with_slow).unwrap();
    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("1 added"), "{stdout}");

    // The replacement runs only once its predecessor is gone: exactly one
    // process for this service is alive at any time.
    let all = wait_for_pids(&pids, "the replacement to start", |p| p.len() == 2);
    let alive: Vec<i32> = all.iter().copied().filter(|pid| is_alive(*pid)).collect();
    assert_eq!(
        alive.len(),
        1,
        "expected exactly one live process, recorded {all:?}, alive {alive:?}"
    );
    daemon.wait_for_status("the replacement to be reported running", |s| {
        s.matches("running").count() == 2
    });

    // And the supervisor can still reach it: nothing survives `down`.
    let (code, stdout, stderr) = cli(&["down"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");
    let deadline = Instant::now() + CEILING;
    loop {
        let survivors: Vec<i32> = all.iter().copied().filter(|pid| is_alive(*pid)).collect();
        if survivors.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("processes {survivors:?} outlived the daemon");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_retired_service_does_not_leave_its_process_group_behind() {
    // A service dropped by a reload is still winding down when the daemon is
    // told to stop, and it is no longer in the config, so `stop_all` treats it
    // specially.  Whichever way it ends — signalled, escalated or detached —
    // its whole process group has to go, not just the direct child.
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let grandchild_pid = dir.path().join("grandchild.pid");
    let deaf = script(
        dir.path(),
        "deaf.sh",
        &format!(
            "trap '' TERM\n\
             (trap '' TERM; echo $$ > {}; while true; do sleep 0.2; done) &\n\
             while true; do sleep 0.2; done",
            grandchild_pid.display()
        ),
    );

    let with_deaf = format!(
        r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
[services.deaf]
command = ["{}"]
restart = "always"
shutdown_timeout = "50s"
"#,
        api.display(),
        deaf.display()
    );
    let cfg = write_config(dir.path(), &with_deaf);

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("both services to run", |s| {
        s.matches("running").count() == 2
    });
    let deadline = Instant::now() + CEILING;
    while !grandchild_pid.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let grandchild: i32 = fs::read_to_string(&grandchild_pid)
        .expect("the fixture should have recorded its grandchild")
        .trim()
        .parse()
        .unwrap();
    assert!(is_alive(grandchild), "fixture grandchild should be running");

    // Retire the service.  It ignores SIGTERM, so it is still in its 50s
    // shutdown when the daemon is told to stop.
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

    let (code, stdout, stderr) = cli(&["down"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");

    let deadline = Instant::now() + CEILING;
    while is_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !is_alive(grandchild),
        "grandchild {grandchild} outlived the supervisor"
    );
}

#[test]
fn a_reload_that_adds_a_dependency_does_not_block_the_dependent() {
    // A service with no dependents has no readiness subscriber, so its status
    // is only recorded if the supervisor writes it unconditionally.  A reload
    // that gives it a dependent makes that dependent read the recorded value
    // first: a stale `pending` there blocks the dependent forever.
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let db = resident(dir.path(), "db.sh");
    let independent = format!(
        r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
[services.db]
command = ["{}"]
restart = "always"
"#,
        api.display(),
        db.display()
    );
    let cfg = write_config(dir.path(), &independent);

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("both services to run", |s| {
        s.matches("running").count() == 2
    });
    let api_pid = daemon.pid_of("api");

    // `db` has been up for a while by now, so the only record of its readiness
    // is the one the supervisor kept while nobody was watching.
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
depends_on = ["db"]
[services.db]
command = ["{}"]
restart = "always"
"#,
            api.display(),
            db.display()
        ),
    )
    .unwrap();

    let (code, stdout, stderr) = daemon.reload();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("1 changed"), "{stdout}");

    // The dependent has to come back up: `db` is running, so its dependency is
    // satisfied the moment it is consulted.
    let fresh = daemon.wait_for_pid_change("api", api_pid);
    assert_ne!(fresh, api_pid);
    daemon.wait_for_status("both services to run again", |s| {
        s.matches("running").count() == 2
    });
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
