//! Integration tests for `servicrab generate`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

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

fn project(dir: &Path) -> PathBuf {
    let path = dir.join("servicrab.toml");
    fs::write(
        &path,
        r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["/usr/bin/api"]
"#,
    )
    .unwrap();
    path
}

#[test]
fn a_systemd_unit_is_written_to_stdout() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path());

    let (code, stdout, stderr) = cli(&["generate", "systemd"], &cfg);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("[Service]"), "{stdout}");
    assert!(stdout.contains("ExecStart="), "{stdout}");
    assert!(stdout.contains("daemon --config"), "{stdout}");
    // The install hints must not pollute the unit itself.
    assert!(!stdout.contains("Install with"), "{stdout}");
    assert!(stderr.contains("systemctl"), "{stderr}");
}

#[test]
fn a_launchd_plist_is_written_to_stdout() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path());

    let (code, stdout, stderr) = cli(&["generate", "launchd"], &cfg);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("<?xml"), "{stdout}");
    assert!(stdout.contains("com.servicrab.demo"), "{stdout}");
    assert!(stdout.trim_end().ends_with("</plist>"), "{stdout}");
    assert!(stderr.contains("launchctl"), "{stderr}");
}

#[test]
fn a_unit_carries_the_profiles_it_was_generated_with() {
    // Otherwise the init system would start a different stack than the operator
    // tried out with `servicrab start --profile`.
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path());

    let (code, stdout, stderr) = cli(
        &[
            "generate",
            "systemd",
            "--profile",
            "dev",
            "--profile",
            "obs",
        ],
        &cfg,
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("daemon --config") && stdout.contains("--profile dev --profile obs"),
        "{stdout}"
    );

    let (code, stdout, stderr) = cli(&["generate", "launchd", "--profile", "dev"], &cfg);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("<string>--profile</string>") && stdout.contains("<string>dev</string>"),
        "{stdout}"
    );
}

#[test]
fn the_unit_can_be_written_to_a_file() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path());
    let target = dir.path().join("demo.service");

    let (code, stdout, stderr) = cli(
        &["generate", "systemd", "-o", target.to_str().unwrap()],
        &cfg,
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrote"), "{stdout}");
    assert!(fs::read_to_string(&target).unwrap().contains("[Unit]"));
}

#[test]
fn a_directory_target_gets_the_conventional_file_name() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path());
    let out = dir.path().join("units");
    fs::create_dir(&out).unwrap();

    let (code, _, stderr) = cli(&["generate", "systemd", "-o", out.to_str().unwrap()], &cfg);
    assert_eq!(code, 0, "{stderr}");
    assert!(out.join("servicrab-demo.service").is_file());

    let (code, _, stderr) = cli(&["generate", "launchd", "-o", out.to_str().unwrap()], &cfg);
    assert_eq!(code, 0, "{stderr}");
    assert!(out.join("com.servicrab.demo.plist").is_file());
}

#[test]
fn the_scope_switches_the_install_target() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path());

    let (_, stdout, stderr) = cli(&["generate", "systemd", "--scope", "user"], &cfg);
    assert!(stdout.contains("WantedBy=default.target"), "{stdout}");
    assert!(stderr.contains("--user"), "{stderr}");

    let (_, stdout, _) = cli(&["generate", "systemd", "--scope", "system"], &cfg);
    assert!(stdout.contains("WantedBy=multi-user.target"), "{stdout}");
}

#[test]
fn a_system_unit_can_name_the_account_to_run_as() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path());

    let (code, stdout, stderr) = cli(&["generate", "systemd", "--user", "deploy"], &cfg);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("User=deploy"), "{stdout}");
}

#[test]
fn an_invalid_config_is_reported_instead_of_a_unit() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("servicrab.toml");
    fs::write(
        &cfg,
        r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["/usr/bin/api"]
depends_on = ["ghost"]
"#,
    )
    .unwrap();

    let (code, stdout, stderr) = cli(&["generate", "systemd"], &cfg);
    assert_ne!(code, 0);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("ghost"), "{stderr}");
}

#[test]
fn an_unknown_target_is_rejected() {
    let dir = TempDir::new().unwrap();
    let cfg = project(dir.path());

    let (code, _, stderr) = cli(&["generate", "upstart"], &cfg);
    assert_ne!(code, 0);
    assert!(stderr.contains("invalid value"), "{stderr}");
}
