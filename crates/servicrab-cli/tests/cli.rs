//! Integration tests for the `servicrab` CLI binary.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

// ── helpers ────────────────────────────────────────────────────────────────

/// Create a temp dir with a minimal valid `servicrab.toml`.
fn temp_dir_with_config(toml: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(&path, toml).unwrap();
    (dir, path)
}

fn minimal_config() -> &'static str {
    "version = 1\n[project]\nname = \"test-project\"\n[services.web]\ncommand = [\"echo\", \"hello\"]\n"
}

fn cmd() -> Command {
    Command::cargo_bin("servicrab").unwrap()
}

// ── init ───────────────────────────────────────────────────────────────────

#[test]
fn init_creates_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");

    cmd()
        .arg("init")
        .arg("--path")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("Created"));

    assert!(path.exists(), "config file should have been created");
    let contents = fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("version = 1"),
        "file should contain version"
    );
}

#[test]
fn init_does_not_overwrite_without_force() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(&path, "existing content").unwrap();

    cmd()
        .arg("init")
        .arg("--path")
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("already exists"));

    // File should be untouched.
    assert_eq!(fs::read_to_string(&path).unwrap(), "existing content");
}

#[test]
fn init_overwrites_with_force() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(&path, "old content").unwrap();

    cmd()
        .arg("init")
        .arg("--path")
        .arg(&path)
        .arg("--force")
        .assert()
        .success();

    let contents = fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("version = 1"),
        "file should be overwritten"
    );
}

// ── check ──────────────────────────────────────────────────────────────────

#[test]
fn check_success() {
    let (_dir, path) = temp_dir_with_config(minimal_config());

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("test-project"));
}

#[test]
fn check_failure_invalid_config() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(&path, "version = 1\n[project]\nname = \"p\"\n").unwrap();

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("error"));
}

#[test]
fn check_failure_missing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("does-not-exist.toml");

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .failure();
}

#[test]
fn check_shows_start_order() {
    let toml = r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["echo", "api"]
depends_on = ["db"]
[services.db]
command = ["echo", "db"]
"#;
    let (_dir, path) = temp_dir_with_config(toml);

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("db"))
        .stdout(contains("api"));
}

// ── list ───────────────────────────────────────────────────────────────────

#[test]
fn list_human_output() {
    let (_dir, path) = temp_dir_with_config(minimal_config());

    cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("test-project"))
        .stdout(contains("web"));
}

#[test]
fn list_json_output() {
    let (_dir, path) = temp_dir_with_config(minimal_config());

    let output = cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");

    // Should be an array of service objects.
    let arr = json.as_array().expect("JSON should be an array");
    assert!(!arr.is_empty());

    let first = &arr[0];
    assert_eq!(first["name"].as_str().unwrap(), "web");
    assert!(first["command"].is_array());
    assert!(first["autostart"].as_bool().unwrap());
    assert_eq!(first["restart"].as_str().unwrap(), "never");
}

#[test]
fn list_failure_on_invalid_config() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    // Missing services section
    fs::write(&path, "version = 1\n[project]\nname = \"p\"\n").unwrap();

    cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .assert()
        .failure();
}

// ── completions ────────────────────────────────────────────────────────────

#[test]
fn completions_are_generated_for_every_supported_shell() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = cmd()
            .arg("completions")
            .arg(shell)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let text = String::from_utf8(output).unwrap();
        assert!(
            text.contains("servicrab"),
            "{shell} completions should mention the binary name"
        );
        assert!(
            text.len() > 200,
            "{shell} completions look suspiciously short"
        );
    }
}

#[test]
fn completions_mention_the_subcommands() {
    let output = cmd()
        .arg("completions")
        .arg("bash")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    for sub in ["up", "down", "logs", "status", "restart"] {
        assert!(text.contains(sub), "bash completions should list {sub}");
    }
}

#[test]
fn completions_reject_an_unknown_shell() {
    cmd().arg("completions").arg("tcsh").assert().failure();
}

// ── man ────────────────────────────────────────────────────────────────────

#[test]
fn the_man_page_documents_every_subcommand() {
    let output = cmd()
        .arg("man")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();

    assert!(text.starts_with(".ie"), "should start with roff, not text");
    assert!(text.contains(".TH servicrab 1"), "missing the man title");

    // The page is generated from the clap definitions, so a new subcommand
    // shows up here without anyone remembering to add it.
    for sub in [
        "init",
        "check",
        "list",
        "run",
        "up",
        "watch",
        "logs",
        "start",
        "stop",
        "restart",
        "reload",
        "events",
        "status",
        "down",
        "generate",
        "completions",
        "man",
        "daemon",
    ] {
        assert!(text.contains(sub), "the man page should mention {sub}");
    }
}

#[test]
fn the_man_page_has_the_hand_written_sections() {
    let output = cmd()
        .arg("man")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();

    for section in [".SH FILES", ".SH ENVIRONMENT", ".SH EXIT STATUS"] {
        assert!(text.contains(section), "the man page should have {section}");
    }
    assert!(text.contains("daemon.sock"), "FILES should list the socket");
    assert!(
        text.contains("RUST_LOG"),
        "ENVIRONMENT should list RUST_LOG"
    );
}

// ── env_file ───────────────────────────────────────────────────────────────

#[test]
fn env_file_values_reach_the_service() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".env"), "GREETING=from-file\n").unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(
        &path,
        "version = 1\n[project]\nname = \"p\"\n[services.web]\ncommand = [\"sh\", \"-c\", \"echo $GREETING\"]\nenv_file = \".env\"\n",
    )
    .unwrap();

    cmd()
        .arg("run")
        .arg("web")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("from-file"));
}

#[test]
fn a_missing_env_file_fails_the_config() {
    let (_dir, path) = temp_dir_with_config(
        "version = 1\n[project]\nname = \"p\"\n[services.web]\ncommand = [\"echo\"]\nenv_file = \"nope.env\"\n",
    );

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("env_file"));
}
