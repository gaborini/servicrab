//! Validation pipeline: raw TOML model → validated runtime [`Config`].
//!
//! The main entry point is [`validate_raw`].  Callers should use
//! [`crate::load::load`] rather than calling this directly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{
    Config, Project, ProjectName, RestartPolicy, Service, ServiceName, ShutdownSignal,
};
use crate::error::{ConfigError, ConfigWarning};
use crate::graph::topological_sort;
use crate::raw::{RawConfig, RawRestartPolicy};

/// The only supported schema version.
const SUPPORTED_VERSION: u32 = 1;

// Duration range constants
const DUR_100MS: Duration = Duration::from_millis(100);
const DUR_1S: Duration = Duration::from_secs(1);
const DUR_1H: Duration = Duration::from_secs(3600);
const DUR_24H: Duration = Duration::from_secs(86400);

/// Convert a raw config and its source path into the validated [`Config`],
/// collecting all errors before returning.
///
/// Returns `Err(errors)` if any validation errors were found, otherwise
/// `Ok((config, warnings))`.
pub fn validate_raw(
    raw: RawConfig,
    source_path: &Path,
) -> Result<(Config, Vec<ConfigWarning>), Vec<ConfigError>> {
    let mut errors: Vec<ConfigError> = Vec::new();
    let mut warnings: Vec<ConfigWarning> = Vec::new();

    // ── 1. Schema version ─────────────────────────────────────────────────
    if raw.version != SUPPORTED_VERSION {
        errors.push(ConfigError::UnsupportedVersion {
            version: raw.version,
        });
    }

    // ── 2. Source paths ────────────────────────────────────────────────────
    let source_path = source_path.to_path_buf();
    let source_dir = resolve_source_dir(&source_path);

    // ── 3. Project name ────────────────────────────────────────────────────
    let project_name = match validate_project_name(&raw.project.name) {
        Ok(n) => Some(n),
        Err(e) => {
            errors.push(e);
            None
        }
    };

    // ── 4. Project env ─────────────────────────────────────────────────────
    let project_env = validate_project_env(&raw.project.env, &mut errors);

    // ── 5. At least one service ────────────────────────────────────────────
    if raw.services.is_empty() {
        errors.push(ConfigError::NoServices);
    }

    // ── 6. Per-service validation ──────────────────────────────────────────
    let mut services: BTreeMap<ServiceName, Service> = BTreeMap::new();

    for (raw_name, raw_svc) in &raw.services {
        // Service name
        let svc_name = match validate_service_name(raw_name) {
            Ok(n) => n,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };

        // Command: must be non-empty, executable non-empty and NUL-free.
        if raw_svc.command.is_empty() {
            errors.push(ConfigError::EmptyCommand {
                service: raw_name.clone(),
            });
            continue;
        }

        let executable = &raw_svc.command[0];
        if executable.is_empty() || executable.contains('\0') {
            errors.push(ConfigError::InvalidExecutable {
                service: raw_name.clone(),
            });
            continue;
        }

        let mut arg_ok = true;
        for arg in &raw_svc.command[1..] {
            if arg.contains('\0') {
                errors.push(ConfigError::NulInCommandArg {
                    service: raw_name.clone(),
                });
                arg_ok = false;
                break;
            }
        }
        if !arg_ok {
            continue;
        }

        // cwd
        let cwd = resolve_cwd(raw_svc.cwd.as_deref(), &source_dir, raw_name, &mut errors);

        // Service env
        let svc_env = validate_service_env(&raw_svc.env, raw_name, &mut errors);

        // Restart policy
        let restart = match raw_svc.restart {
            RawRestartPolicy::Never => RestartPolicy::Never,
            RawRestartPolicy::OnFailure => RestartPolicy::OnFailure,
            RawRestartPolicy::Always => RestartPolicy::Always,
        };

        // Durations
        let restart_delay = parse_duration_field(
            raw_svc.restart_delay.as_deref(),
            "restart_delay",
            Duration::from_secs(1),
            raw_name,
            DUR_100MS,
            DUR_1H,
            &mut errors,
        );

        let restart_max_delay = parse_duration_field(
            raw_svc.restart_max_delay.as_deref(),
            "restart_max_delay",
            Duration::from_secs(30),
            raw_name,
            DUR_100MS,
            DUR_24H,
            &mut errors,
        );

        let max_restarts = raw_svc.max_restarts.unwrap_or(10);

        let stable_after = parse_duration_field(
            raw_svc.stable_after.as_deref(),
            "stable_after",
            Duration::from_secs(60),
            raw_name,
            DUR_1S,
            DUR_24H,
            &mut errors,
        );

        let shutdown_signal =
            parse_shutdown_signal(raw_svc.shutdown_signal.as_deref(), raw_name, &mut errors);

        let shutdown_timeout = parse_duration_field(
            raw_svc.shutdown_timeout.as_deref(),
            "shutdown_timeout",
            Duration::from_secs(10),
            raw_name,
            DUR_100MS,
            DUR_1H,
            &mut errors,
        );

        // restart_max_delay >= restart_delay
        if restart_max_delay < restart_delay {
            errors.push(ConfigError::RestartMaxDelayTooSmall {
                service: raw_name.clone(),
                delay: restart_delay,
                max_delay: restart_max_delay,
            });
        }

        // Warnings: restart settings that have no effect when restart="never"
        if restart == RestartPolicy::Never {
            for (field, is_explicit) in [
                ("restart_delay", raw_svc.restart_delay.is_some()),
                ("restart_max_delay", raw_svc.restart_max_delay.is_some()),
                ("max_restarts", raw_svc.max_restarts.is_some()),
                ("stable_after", raw_svc.stable_after.is_some()),
            ] {
                if is_explicit {
                    warnings.push(ConfigWarning::RestartSettingsIgnored {
                        service: raw_name.clone(),
                        field,
                    });
                }
            }
        }

        // Warning: executable not in PATH
        check_executable_in_path(executable, raw_name, &mut warnings);

        // Merge environment: process → project → service  (later overrides earlier)
        let mut merged_env: BTreeMap<String, String> = std::env::vars().collect();
        merged_env.extend(project_env.clone());
        merged_env.extend(svc_env);

        // Collect depends_on as ServiceNames (cross-service validation deferred)
        let depends_on: Vec<ServiceName> = raw_svc
            .depends_on
            .iter()
            .map(|d| ServiceName(d.clone()))
            .collect();

        let args = raw_svc.command[1..].to_vec();

        services.insert(
            svc_name.clone(),
            Service {
                name: svc_name,
                executable: executable.clone(),
                args,
                cwd,
                env: merged_env,
                depends_on,
                autostart: raw_svc.autostart,
                restart,
                restart_delay,
                restart_max_delay,
                max_restarts,
                stable_after,
                shutdown_signal,
                shutdown_timeout,
            },
        );
    }

    // ── 7. Cross-service dependency validation ────────────────────────────
    for (raw_name, raw_svc) in &raw.services {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for dep in &raw_svc.depends_on {
            if dep == raw_name {
                errors.push(ConfigError::SelfDependency {
                    service: raw_name.clone(),
                });
            } else if !raw.services.contains_key(dep.as_str()) {
                errors.push(ConfigError::UnknownDependency {
                    service: raw_name.clone(),
                    dep: dep.clone(),
                });
            } else if !seen.insert(dep.as_str()) {
                errors.push(ConfigError::DuplicateDependency {
                    service: raw_name.clone(),
                    dep: dep.clone(),
                });
            }
        }
    }

    // ── 8. Return early if any errors accumulated ─────────────────────────
    if !errors.is_empty() {
        return Err(errors);
    }

    // ── 9. Topological sort (may also emit DependencyCycle) ───────────────
    let dep_graph: BTreeMap<ServiceName, Vec<ServiceName>> = services
        .iter()
        .map(|(name, svc)| (name.clone(), svc.depends_on.clone()))
        .collect();

    let start_order = match topological_sort(&dep_graph) {
        Ok(order) => order,
        Err(e) => return Err(vec![e]),
    };

    let config = Config {
        source_path,
        source_dir,
        project: Project {
            name: project_name.expect("project_name is Some when no errors"),
            env: project_env,
        },
        services,
        start_order,
    };

    Ok((config, warnings))
}

// ── Name validation ────────────────────────────────────────────────────────

/// Validate and wrap a project name.
pub(crate) fn validate_project_name(name: &str) -> Result<ProjectName, ConfigError> {
    validate_name(name, 64)
        .map(|()| ProjectName(name.to_string()))
        .map_err(|reason| ConfigError::InvalidProjectName {
            name: name.to_string(),
            reason,
        })
}

/// Validate and wrap a service name.
pub(crate) fn validate_service_name(name: &str) -> Result<ServiceName, ConfigError> {
    validate_name(name, 48)
        .map(|()| ServiceName(name.to_string()))
        .map_err(|reason| ConfigError::InvalidServiceName {
            name: name.to_string(),
            reason,
        })
}

/// Core name validation (shared between project and service names).
fn validate_name(name: &str, max_len: usize) -> Result<(), String> {
    if name.is_empty() {
        return Err("must not be empty".to_string());
    }
    if !name.is_ascii() {
        return Err("must contain only ASCII characters".to_string());
    }
    if name.len() > max_len {
        return Err(format!(
            "must be at most {max_len} bytes, got {}",
            name.len()
        ));
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(format!(
            "must begin with an ASCII alphanumeric character, got {first:?}"
        ));
    }
    for ch in name.chars() {
        if !matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-') {
            return Err(format!(
                "character {ch:?} is not allowed; only ASCII alphanumerics, '.', '_', and '-' are permitted"
            ));
        }
    }
    Ok(())
}

// ── Environment validation ─────────────────────────────────────────────────

fn validate_project_env(
    env: &BTreeMap<String, String>,
    errors: &mut Vec<ConfigError>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in env {
        if !is_valid_env_key(k) {
            errors.push(ConfigError::InvalidProjectEnvKey { key: k.clone() });
            continue;
        }
        if v.contains('\0') {
            errors.push(ConfigError::NulInProjectEnvValue { key: k.clone() });
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    out
}

fn validate_service_env(
    env: &BTreeMap<String, String>,
    service: &str,
    errors: &mut Vec<ConfigError>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in env {
        if !is_valid_env_key(k) {
            errors.push(ConfigError::InvalidEnvKey {
                service: service.to_string(),
                key: k.clone(),
            });
            continue;
        }
        if v.contains('\0') {
            errors.push(ConfigError::NulInEnvValue {
                service: service.to_string(),
                key: k.clone(),
            });
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    out
}

fn is_valid_env_key(key: &str) -> bool {
    !key.is_empty() && !key.contains('=') && !key.contains('\0')
}

// ── cwd resolution ─────────────────────────────────────────────────────────

fn resolve_source_dir(source_path: &Path) -> PathBuf {
    let parent = source_path
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                Path::new(".")
            } else {
                p
            }
        })
        .unwrap_or_else(|| Path::new("."));
    parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf())
}

fn resolve_cwd(
    raw_cwd: Option<&str>,
    source_dir: &Path,
    service: &str,
    errors: &mut Vec<ConfigError>,
) -> PathBuf {
    let candidate = match raw_cwd {
        None => source_dir.to_path_buf(),
        Some(cwd) => {
            let p = Path::new(cwd);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                source_dir.join(p)
            }
        }
    };

    match candidate.canonicalize() {
        Ok(abs) if abs.is_dir() => abs,
        Ok(abs) => {
            errors.push(ConfigError::InvalidCwd {
                service: service.to_string(),
                cwd: abs,
            });
            source_dir.to_path_buf()
        }
        Err(_) => {
            errors.push(ConfigError::InvalidCwd {
                service: service.to_string(),
                cwd: candidate,
            });
            source_dir.to_path_buf()
        }
    }
}

// ── Duration parsing ───────────────────────────────────────────────────────

fn parse_duration_field(
    raw: Option<&str>,
    field: &'static str,
    default: Duration,
    service: &str,
    min: Duration,
    max: Duration,
    errors: &mut Vec<ConfigError>,
) -> Duration {
    let s = match raw {
        None => return default,
        Some(s) => s,
    };

    let dur = match humantime::parse_duration(s) {
        Ok(d) => d,
        Err(e) => {
            errors.push(ConfigError::InvalidDuration {
                service: service.to_string(),
                field,
                value: s.to_string(),
                reason: e.to_string(),
            });
            return default;
        }
    };

    if dur < min {
        errors.push(ConfigError::DurationOutOfRange {
            service: service.to_string(),
            field,
            reason: format!("must be at least {min:?}, got {dur:?}"),
        });
        return default;
    }
    if dur > max {
        errors.push(ConfigError::DurationOutOfRange {
            service: service.to_string(),
            field,
            reason: format!("must be at most {max:?}, got {dur:?}"),
        });
        return default;
    }

    dur
}

// ── Shutdown signal parsing ────────────────────────────────────────────────

fn parse_shutdown_signal(
    raw: Option<&str>,
    service: &str,
    errors: &mut Vec<ConfigError>,
) -> ShutdownSignal {
    match raw.unwrap_or("term") {
        "term" => ShutdownSignal::Term,
        "int" => ShutdownSignal::Int,
        "quit" => ShutdownSignal::Quit,
        "hup" => ShutdownSignal::Hup,
        other => {
            errors.push(ConfigError::InvalidShutdownSignal {
                service: service.to_string(),
                value: other.to_string(),
            });
            ShutdownSignal::Term
        }
    }
}

// ── PATH check ────────────────────────────────────────────────────────────

fn check_executable_in_path(executable: &str, service: &str, warnings: &mut Vec<ConfigWarning>) {
    // Only check bare names (no path separator); absolute/relative paths are
    // left to the OS at spawn time.
    if executable.contains(std::path::MAIN_SEPARATOR) {
        return;
    }
    // Best-effort: skip on platforms where `which`-style lookup is unavailable.
    if which_in_path(executable).is_none() {
        warnings.push(ConfigWarning::ExecutableNotInPath {
            service: service.to_string(),
            executable: executable.to_string(),
        });
    }
}

/// Minimal "which" implementation using PATH.
fn which_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    /// Write a `servicrab.toml` to a temp dir and load it via `validate_raw`.
    fn load_from_str(toml: &str) -> Result<(Config, Vec<ConfigWarning>), Vec<ConfigError>> {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, toml).unwrap();

        let raw: RawConfig = toml::from_str(toml).expect("raw parse failed in test");
        validate_raw(raw, &path)
    }

    /// Like `load_from_str` but expects exactly one error.
    #[allow(dead_code)]
    fn expect_one_error(toml: &str) -> ConfigError {
        let errs = load_from_str(toml).unwrap_err();
        assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
        errs.into_iter().next().unwrap()
    }

    // ── Minimal valid config ───────────────────────────────────────────────

    #[test]
    fn minimal_valid_config() {
        let toml = r#"
version = 1
[project]
name = "my-project"
[services.web]
command = ["python", "-m", "http.server"]
"#;
        let (cfg, _) = load_from_str(toml).expect("should be valid");
        assert_eq!(cfg.project.name.as_str(), "my-project");
        assert!(cfg.services.contains_key(&ServiceName("web".into())));
    }

    // ── Default values ─────────────────────────────────────────────────────

    #[test]
    fn service_defaults() {
        let toml = r#"
version = 1
[project]
name = "p"
[services.s]
command = ["echo"]
"#;
        let (cfg, _) = load_from_str(toml).unwrap();
        let svc = &cfg.services[&ServiceName("s".into())];
        assert!(svc.autostart);
        assert_eq!(svc.restart, RestartPolicy::Never);
        assert_eq!(svc.restart_delay, Duration::from_secs(1));
        assert_eq!(svc.restart_max_delay, Duration::from_secs(30));
        assert_eq!(svc.max_restarts, 10);
        assert_eq!(svc.stable_after, Duration::from_secs(60));
        assert_eq!(svc.shutdown_signal, ShutdownSignal::Term);
        assert_eq!(svc.shutdown_timeout, Duration::from_secs(10));
    }

    // ── Unknown-field rejection ────────────────────────────────────────────

    #[test]
    fn unknown_field_in_service_is_rejected() {
        let toml = r#"
version = 1
[project]
name = "p"
[services.s]
command = ["echo"]
unknown_field = "oops"
"#;
        // Strict rejection happens at TOML parse time, before validate_raw.
        let result: Result<RawConfig, _> = toml::from_str(toml);
        assert!(result.is_err(), "expected parse error for unknown field");
    }

    #[test]
    fn unknown_field_in_project_is_rejected() {
        let toml = r#"
version = 1
[project]
name = "p"
bad = true
[services.s]
command = ["echo"]
"#;
        let result: Result<RawConfig, _> = toml::from_str(toml);
        assert!(result.is_err());
    }

    // ── Schema version ─────────────────────────────────────────────────────

    #[test]
    fn unsupported_version_errors() {
        let toml = r#"
version = 2
[project]
name = "p"
[services.s]
command = ["echo"]
"#;
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::UnsupportedVersion { version: 2 })));
    }

    // ── Project name validation ────────────────────────────────────────────

    #[test]
    fn project_name_too_long() {
        let name = "a".repeat(65);
        let toml = format!(
            "version = 1\n[project]\nname = \"{name}\"\n[services.s]\ncommand = [\"echo\"]\n"
        );
        let errs = load_from_str(&toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidProjectName { .. })));
    }

    #[test]
    fn project_name_at_max_length() {
        let name = "a".repeat(64);
        let toml = format!(
            "version = 1\n[project]\nname = \"{name}\"\n[services.s]\ncommand = [\"echo\"]\n"
        );
        load_from_str(&toml).expect("64-char name should be valid");
    }

    #[test]
    fn project_name_starts_with_dash_invalid() {
        let toml = "version = 1\n[project]\nname = \"-bad\"\n[services.s]\ncommand = [\"echo\"]\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidProjectName { .. })));
    }

    #[test]
    fn project_name_with_valid_special_chars() {
        let toml =
            "version = 1\n[project]\nname = \"my.project_1-ok\"\n[services.s]\ncommand = [\"echo\"]\n";
        load_from_str(toml).expect("valid name with '.', '_', '-'");
    }

    // ── Service name validation ────────────────────────────────────────────

    #[test]
    fn service_name_at_max_length() {
        let name = "a".repeat(48);
        let toml = format!(
            "version = 1\n[project]\nname = \"p\"\n[services.{name}]\ncommand = [\"echo\"]\n"
        );
        load_from_str(&toml).expect("48-char service name valid");
    }

    #[test]
    fn service_name_too_long() {
        let name = "a".repeat(49);
        let toml = format!(
            "version = 1\n[project]\nname = \"p\"\n[services.{name}]\ncommand = [\"echo\"]\n"
        );
        let errs = load_from_str(&toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidServiceName { .. })));
    }

    // ── Empty command ──────────────────────────────────────────────────────

    #[test]
    fn empty_command_vec_errors() {
        let toml = "version = 1\n[project]\nname = \"p\"\n[services.s]\ncommand = []\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::EmptyCommand { .. })));
    }

    #[test]
    fn empty_executable_errors() {
        let toml =
            "version = 1\n[project]\nname = \"p\"\n[services.s]\ncommand = [\"\", \"arg\"]\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidExecutable { .. })));
    }

    // ── NUL rejection ─────────────────────────────────────────────────────

    #[test]
    fn nul_in_executable_errors() {
        // NUL bytes can't be in a TOML string literal, but we can test the
        // validation function directly.
        let raw = crate::raw::RawConfig {
            version: 1,
            project: crate::raw::RawProject {
                name: "p".into(),
                env: Default::default(),
            },
            services: {
                let mut m = std::collections::BTreeMap::new();
                m.insert(
                    "s".to_string(),
                    crate::raw::RawService {
                        command: vec!["exe\0cutable".to_string()],
                        cwd: None,
                        env: Default::default(),
                        depends_on: vec![],
                        autostart: true,
                        restart: Default::default(),
                        restart_delay: None,
                        restart_max_delay: None,
                        max_restarts: None,
                        stable_after: None,
                        shutdown_signal: None,
                        shutdown_timeout: None,
                    },
                );
                m
            },
        };
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, "").unwrap();
        let errs = validate_raw(raw, &path).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidExecutable { .. })));
    }

    // ── cwd resolution ─────────────────────────────────────────────────────

    #[test]
    fn relative_cwd_resolved_against_config_dir() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();

        let toml =
            "version = 1\n[project]\nname = \"p\"\n[services.s]\ncommand = [\"echo\"]\ncwd = \"sub\"\n";
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, toml).unwrap();

        let raw: RawConfig = toml::from_str(toml).unwrap();
        let (cfg, _) = validate_raw(raw, &path).unwrap();
        let svc = &cfg.services[&ServiceName("s".into())];
        assert_eq!(svc.cwd, sub.canonicalize().unwrap());
    }

    #[test]
    fn missing_cwd_errors() {
        let dir = TempDir::new().unwrap();
        let toml = "version = 1\n[project]\nname = \"p\"\n[services.s]\ncommand = [\"echo\"]\ncwd = \"nonexistent\"\n";
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, toml).unwrap();

        let raw: RawConfig = toml::from_str(toml).unwrap();
        let errs = validate_raw(raw, &path).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidCwd { .. })));
    }

    #[test]
    fn cwd_points_to_file_errors() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("afile.txt");
        std::fs::write(&file_path, "content").unwrap();

        let toml =
            "version = 1\n[project]\nname = \"p\"\n[services.s]\ncommand = [\"echo\"]\ncwd = \"afile.txt\"\n";
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, toml).unwrap();

        let raw: RawConfig = toml::from_str(toml).unwrap();
        let errs = validate_raw(raw, &path).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidCwd { .. })));
    }

    // ── Environment validation ─────────────────────────────────────────────

    #[test]
    fn env_key_with_equals_rejected() {
        let toml = "version = 1\n[project]\nname = \"p\"\n[services.s]\ncommand = [\"echo\"]\n[services.s.env]\n\"KEY=BAD\" = \"v\"\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidEnvKey { .. })));
    }

    #[test]
    fn env_key_empty_rejected() {
        // Empty keys are not representable in TOML, so we test via raw struct.
        let raw = make_raw_with_service_env(vec![("".to_string(), "v".to_string())]);
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, "").unwrap();
        let errs = validate_raw(raw, &path).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidEnvKey { .. })));
    }

    #[test]
    fn env_override_order() {
        // Set a known env var at the process level, override in project, then
        // service.
        std::env::set_var("_SVCRAB_TEST_KEY", "process");

        let toml = r#"
version = 1
[project]
name = "p"
[project.env]
_SVCRAB_TEST_KEY = "project"
[services.s]
command = ["echo"]
[services.s.env]
_SVCRAB_TEST_KEY = "service"
"#;
        let (cfg, _) = load_from_str(toml).unwrap();
        let svc = &cfg.services[&ServiceName("s".into())];
        assert_eq!(
            svc.env.get("_SVCRAB_TEST_KEY").map(String::as_str),
            Some("service")
        );
        std::env::remove_var("_SVCRAB_TEST_KEY");
    }

    // ── Restart policies ───────────────────────────────────────────────────

    #[test]
    fn restart_policy_never() {
        let (cfg, _) = load_from_str(
            "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nrestart=\"never\"\n",
        )
        .unwrap();
        assert_eq!(
            cfg.services[&ServiceName("s".into())].restart,
            RestartPolicy::Never
        );
    }

    #[test]
    fn restart_policy_on_failure() {
        let (cfg, _) = load_from_str(
            "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nrestart=\"on-failure\"\n",
        )
        .unwrap();
        assert_eq!(
            cfg.services[&ServiceName("s".into())].restart,
            RestartPolicy::OnFailure
        );
    }

    #[test]
    fn restart_policy_always() {
        let (cfg, _) = load_from_str(
            "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nrestart=\"always\"\n",
        )
        .unwrap();
        assert_eq!(
            cfg.services[&ServiceName("s".into())].restart,
            RestartPolicy::Always
        );
    }

    // ── Duration parsing ───────────────────────────────────────────────────

    #[test]
    fn duration_parsed_correctly() {
        let toml = r#"
version = 1
[project]
name = "p"
[services.s]
command = ["echo"]
restart = "always"
restart_delay = "500ms"
restart_max_delay = "2m"
shutdown_timeout = "30s"
"#;
        let (cfg, _) = load_from_str(toml).unwrap();
        let svc = &cfg.services[&ServiceName("s".into())];
        assert_eq!(svc.restart_delay, Duration::from_millis(500));
        assert_eq!(svc.restart_max_delay, Duration::from_secs(120));
        assert_eq!(svc.shutdown_timeout, Duration::from_secs(30));
    }

    #[test]
    fn restart_delay_below_minimum_errors() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nrestart=\"always\"\nrestart_delay=\"50ms\"\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ConfigError::DurationOutOfRange {
                field: "restart_delay",
                ..
            }
        )));
    }

    #[test]
    fn restart_delay_at_minimum_valid() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nrestart=\"always\"\nrestart_delay=\"100ms\"\n";
        load_from_str(toml).expect("100ms is the minimum, should be valid");
    }

    #[test]
    fn restart_max_delay_less_than_restart_delay_errors() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nrestart=\"always\"\nrestart_delay=\"10s\"\nrestart_max_delay=\"5s\"\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::RestartMaxDelayTooSmall { .. })));
    }

    #[test]
    fn invalid_duration_string_errors() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nrestart=\"always\"\nrestart_delay=\"not-a-duration\"\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidDuration { .. })));
    }

    // ── Dependency validation ──────────────────────────────────────────────

    #[test]
    fn unknown_dependency_errors() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\ndepends_on=[\"ghost\"]\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::UnknownDependency { dep, .. } if dep == "ghost")));
    }

    #[test]
    fn self_dependency_errors() {
        let toml =
            "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\ndepends_on=[\"s\"]\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::SelfDependency { service } if service == "s")));
    }

    #[test]
    fn duplicate_dependency_errors() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.a]\ncommand=[\"echo\"]\n[services.b]\ncommand=[\"echo\"]\ndepends_on=[\"a\",\"a\"]\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::DuplicateDependency { dep, .. } if dep == "a")));
    }

    #[test]
    fn dependency_cycle_readable_path() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.a]\ncommand=[\"echo\"]\ndepends_on=[\"b\"]\n[services.b]\ncommand=[\"echo\"]\ndepends_on=[\"a\"]\n";
        let errs = load_from_str(toml).unwrap_err();
        let cycle = errs.iter().find_map(|e| match e {
            ConfigError::DependencyCycle { cycle } => Some(cycle.as_str()),
            _ => None,
        });
        let cycle = cycle.expect("DependencyCycle error");
        assert!(cycle.contains('a'), "cycle={cycle}");
        assert!(cycle.contains('b'), "cycle={cycle}");
        assert!(cycle.contains("->"), "cycle should show arrow: {cycle}");
    }

    #[test]
    fn valid_dependency_passes() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.a]\ncommand=[\"echo\"]\n[services.b]\ncommand=[\"echo\"]\ndepends_on=[\"a\"]\n";
        load_from_str(toml).expect("valid dependency should pass");
    }

    // ── Topological order ──────────────────────────────────────────────────

    #[test]
    fn deterministic_start_order() {
        let toml = r#"
version = 1
[project]
name = "p"
[services.c]
command = ["echo"]
depends_on = ["a", "b"]
[services.b]
command = ["echo"]
depends_on = ["a"]
[services.a]
command = ["echo"]
"#;
        let (cfg, _) = load_from_str(toml).unwrap();
        let order: Vec<&str> = cfg.start_order.iter().map(|n| n.as_str()).collect();
        let pos_a = order.iter().position(|&n| n == "a").unwrap();
        let pos_b = order.iter().position(|&n| n == "b").unwrap();
        let pos_c = order.iter().position(|&n| n == "c").unwrap();
        assert!(pos_a < pos_b, "a must come before b");
        assert!(pos_a < pos_c, "a must come before c");
        assert!(pos_b < pos_c, "b must come before c");
    }

    // ── autostart ─────────────────────────────────────────────────────────

    #[test]
    fn autostart_false_parsed() {
        let toml =
            "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nautostart=false\n";
        let (cfg, _) = load_from_str(toml).unwrap();
        assert!(!cfg.services[&ServiceName("s".into())].autostart);
    }

    // ── Warnings ──────────────────────────────────────────────────────────

    #[test]
    fn restart_settings_warn_when_restart_never() {
        let toml = r#"
version = 1
[project]
name = "p"
[services.s]
command = ["echo"]
restart = "never"
restart_delay = "2s"
"#;
        let (_, warnings) = load_from_str(toml).unwrap();
        assert!(
            warnings.iter().any(|w| matches!(
                w,
                ConfigWarning::RestartSettingsIgnored {
                    field: "restart_delay",
                    ..
                }
            )),
            "expected restart_delay warning"
        );
    }

    // ── no services ───────────────────────────────────────────────────────

    #[test]
    fn no_services_errors() {
        let toml = "version=1\n[project]\nname=\"p\"\n";
        let errs = load_from_str(toml).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, ConfigError::NoServices)));
    }

    // ── helpers ────────────────────────────────────────────────────────────

    fn make_raw_with_service_env(env: Vec<(String, String)>) -> RawConfig {
        use crate::raw::RawService;
        let mut services = BTreeMap::new();
        services.insert(
            "s".to_string(),
            RawService {
                command: vec!["echo".to_string()],
                cwd: None,
                env: env.into_iter().collect(),
                depends_on: vec![],
                autostart: true,
                restart: Default::default(),
                restart_delay: None,
                restart_max_delay: None,
                max_restarts: None,
                stable_after: None,
                shutdown_signal: None,
                shutdown_timeout: None,
            },
        );
        RawConfig {
            version: 1,
            project: crate::raw::RawProject {
                name: "p".into(),
                env: Default::default(),
            },
            services,
        }
    }
}
