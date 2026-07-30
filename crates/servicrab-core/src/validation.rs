//! Validation pipeline: raw TOML model → validated runtime [`Config`].
//!
//! The main entry point is [`validate_raw`].  Callers should use
//! [`crate::load::load`] rather than calling this directly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{
    Config, Dependency, DependencyCondition, HealthCheck, HealthProbe, LogSettings, Project,
    ProjectName, RestartPolicy, Service, ServiceName, ShutdownSignal, UnhealthyAction,
    WatchSettings,
};
use crate::error::{ConfigError, ConfigWarning};
use crate::graph::topological_sort;
use crate::raw::{
    RawConfig, RawDependsOn, RawEnvFile, RawHealthCheck, RawRestartPolicy, RawService, RawWatch,
};

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

    // ── 4a. Project env files ──────────────────────────────────────────────
    let (project_env_files, project_file_env) = load_env_files(
        raw.project.env_file.as_ref(),
        &source_dir,
        "project",
        &mut errors,
    );

    // ── 4b. Project log settings ───────────────────────────────────────────
    let logs = raw
        .project
        .logs
        .as_ref()
        .and_then(|raw_logs| validate_logs(raw_logs, &source_dir, &mut errors));

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

        // Service env files
        let (svc_env_files, svc_file_env) = load_env_files(
            raw_svc.env_file.as_ref(),
            &source_dir,
            &format!("service {raw_name:?}"),
            &mut errors,
        );

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

        let health = raw_svc
            .health
            .as_ref()
            .and_then(|raw_health| validate_health(raw_health, raw_name, &mut errors));

        let log_to_file = raw_svc.logs.as_ref().is_none_or(|logs| logs.enabled);

        let watch = raw_svc
            .watch
            .as_ref()
            .and_then(|raw_watch| validate_watch(raw_watch, &cwd, raw_name, &mut errors));

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

        // Merge environment, later layers override earlier ones:
        //   process → project env_file → project env → service env_file → service env
        let mut merged_env: BTreeMap<String, String> = std::env::vars().collect();
        merged_env.extend(project_file_env.clone());
        merged_env.extend(project_env.clone());
        merged_env.extend(svc_file_env);
        merged_env.extend(svc_env);

        // Collect depends_on (cross-service validation deferred to step 7).
        // An unparseable condition is dropped rather than reported here: step 7
        // walks the raw services, so it also sees the ones this loop skipped
        // over an invalid command, and it is the single place that reports
        // dependency problems.
        let depends_on: Vec<Dependency> = dependency_entries(raw_svc)
            .into_iter()
            .map(|(dep, condition)| Dependency {
                service: ServiceName(dep.to_string()),
                condition: condition.and_then(parse_dependency_condition),
            })
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
                env_files: svc_env_files,
                depends_on,
                autostart: raw_svc.autostart,
                restart,
                restart_delay,
                restart_max_delay,
                max_restarts,
                stable_after,
                shutdown_signal,
                shutdown_timeout,
                health,
                log_to_file,
                watch,
            },
        );
    }

    // ── 7. Cross-service dependency validation ────────────────────────────
    for (raw_name, raw_svc) in &raw.services {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for (dep, condition) in dependency_entries(raw_svc) {
            if dep == raw_name.as_str() {
                errors.push(ConfigError::SelfDependency {
                    service: raw_name.clone(),
                });
                continue;
            }
            let Some(target) = raw.services.get(dep) else {
                errors.push(ConfigError::UnknownDependency {
                    service: raw_name.clone(),
                    dep: dep.to_string(),
                });
                continue;
            };
            if !seen.insert(dep) {
                errors.push(ConfigError::DuplicateDependency {
                    service: raw_name.clone(),
                    dep: dep.to_string(),
                });
                continue;
            }

            // A condition the dependency can never satisfy is a deadlock
            // waiting to happen — the dependent would sit there until someone
            // interrupts the stack — so it is rejected at load time.
            let Some(condition) = condition else { continue };
            match parse_dependency_condition(condition) {
                None => errors.push(ConfigError::InvalidDependencyCondition {
                    service: raw_name.clone(),
                    dep: dep.to_string(),
                    value: condition.to_string(),
                }),
                Some(DependencyCondition::ServiceHealthy) if target.health.is_none() => {
                    errors.push(ConfigError::DependencyNotHealthChecked {
                        service: raw_name.clone(),
                        dep: dep.to_string(),
                    });
                }
                Some(DependencyCondition::ServiceCompletedSuccessfully)
                    if target.restart == RawRestartPolicy::Always =>
                {
                    errors.push(ConfigError::DependencyNeverCompletes {
                        service: raw_name.clone(),
                        dep: dep.to_string(),
                    });
                }
                Some(_) => {}
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
        .map(|(name, svc)| {
            let deps = svc.depends_on.iter().map(|d| d.service.clone()).collect();
            (name.clone(), deps)
        })
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
            env_files: project_env_files,
            logs,
        },
        services,
        start_order,
    };

    Ok((config, warnings))
}

// ── Dependencies ───────────────────────────────────────────────────────────

/// The `depends_on` entries of one raw service, whichever form it used.
fn dependency_entries(service: &RawService) -> Vec<(&str, Option<&str>)> {
    service
        .depends_on
        .as_ref()
        .map(RawDependsOn::entries)
        .unwrap_or_default()
}

/// Parse a `depends_on` condition token, or `None` if it is not one.
fn parse_dependency_condition(value: &str) -> Option<DependencyCondition> {
    match value {
        "service_started" => Some(DependencyCondition::ServiceStarted),
        "service_healthy" => Some(DependencyCondition::ServiceHealthy),
        "service_completed_successfully" => Some(DependencyCondition::ServiceCompletedSuccessfully),
        _ => None,
    }
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

/// Resolve and read the declared `env_file` entries.
///
/// Returns the resolved absolute paths (in declaration order) and the merged
/// key/value pairs, where a later file overrides an earlier one.  Every failure
/// is pushed onto `errors`; the caller keeps validating so that a run reports
/// all problems at once.
fn load_env_files(
    declared: Option<&RawEnvFile>,
    source_dir: &Path,
    scope: &str,
    errors: &mut Vec<ConfigError>,
) -> (Vec<PathBuf>, BTreeMap<String, String>) {
    let mut paths = Vec::new();
    let mut merged = BTreeMap::new();

    let Some(declared) = declared else {
        return (paths, merged);
    };

    for candidate in declared.paths() {
        let raw = Path::new(candidate);
        let path = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            source_dir.join(raw)
        };

        match crate::envfile::load(&path) {
            Ok(vars) => {
                for (key, value) in vars {
                    if !is_valid_env_key(&key) || value.contains('\0') {
                        errors.push(ConfigError::InvalidEnvFile {
                            scope: scope.to_string(),
                            path: path.clone(),
                            reason: format!("invalid entry for key {key:?}"),
                        });
                        continue;
                    }
                    merged.insert(key, value);
                }
            }
            Err(e) => errors.push(ConfigError::InvalidEnvFile {
                scope: scope.to_string(),
                path: path.clone(),
                reason: e.to_string(),
            }),
        }

        paths.push(path);
    }

    (paths, merged)
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

// ── Watch settings validation ──────────────────────────────────────────────

/// Ignore entries that are always applied, on top of what the config asks for.
const ALWAYS_IGNORED: [&str; 2] = [".git", ".servicrab"];

/// Validate a `[services.<name>.watch]` table.
fn validate_watch(
    raw: &RawWatch,
    cwd: &Path,
    service: &str,
    errors: &mut Vec<ConfigError>,
) -> Option<WatchSettings> {
    let mut paths = Vec::new();

    if raw.paths.is_empty() {
        errors.push(ConfigError::InvalidWatch {
            service: service.to_string(),
            reason: "paths must list at least one file or directory".to_string(),
        });
    }

    for entry in &raw.paths {
        if entry.is_empty() {
            errors.push(ConfigError::InvalidWatch {
                service: service.to_string(),
                reason: "paths must not contain an empty entry".to_string(),
            });
            continue;
        }

        let candidate = PathBuf::from(entry);
        let resolved = if candidate.is_absolute() {
            candidate
        } else {
            cwd.join(candidate)
        };

        match resolved.canonicalize() {
            Ok(path) => paths.push(path),
            Err(e) => errors.push(ConfigError::InvalidWatch {
                service: service.to_string(),
                reason: format!("path {} cannot be watched: {e}", resolved.display()),
            }),
        }
    }

    let interval = parse_duration_field(
        raw.interval.as_deref(),
        "watch.interval",
        Duration::from_secs(1),
        service,
        DUR_100MS,
        DUR_1H,
        errors,
    );
    let debounce = parse_duration_field(
        raw.debounce.as_deref(),
        "watch.debounce",
        Duration::from_millis(300),
        service,
        Duration::from_millis(50),
        DUR_1H,
        errors,
    );

    let mut ignore: Vec<String> = ALWAYS_IGNORED.iter().map(|s| (*s).to_string()).collect();
    for entry in &raw.ignore {
        if entry.is_empty() {
            errors.push(ConfigError::InvalidWatch {
                service: service.to_string(),
                reason: "ignore must not contain an empty entry".to_string(),
            });
            continue;
        }
        if !ignore.iter().any(|existing| existing == entry) {
            ignore.push(entry.clone());
        }
    }

    if paths.is_empty() {
        return None;
    }

    Some(WatchSettings {
        paths,
        ignore,
        interval,
        debounce,
    })
}

// ── Log settings validation ────────────────────────────────────────────────

/// Default log directory, relative to the config file.
const DEFAULT_LOG_DIR: &str = ".servicrab/logs";
/// Default rotation threshold.
const DEFAULT_MAX_SIZE: u64 = 10 * 1024 * 1024;
/// Smallest rotation threshold that still makes sense.
const MIN_MAX_SIZE: u64 = 1024;
/// Largest accepted rotation threshold (1 TiB).
const MAX_MAX_SIZE: u64 = 1024 * 1024 * 1024 * 1024;

/// Validate a `[project.logs]` table.
fn validate_logs(
    raw: &crate::raw::RawLogs,
    source_dir: &Path,
    errors: &mut Vec<ConfigError>,
) -> Option<LogSettings> {
    let dir = raw.dir.as_deref().unwrap_or(DEFAULT_LOG_DIR);
    let dir = {
        let candidate = PathBuf::from(dir);
        if candidate.is_absolute() {
            candidate
        } else {
            source_dir.join(candidate)
        }
    };

    // Catching a file where a directory belongs here turns a confusing
    // runtime write failure into a config error.
    if dir.exists() && !dir.is_dir() {
        errors.push(ConfigError::InvalidLogDir {
            dir: dir.clone(),
            reason: "exists but is not a directory".to_string(),
        });
        return None;
    }

    let max_size = match raw.max_size.as_deref() {
        None => DEFAULT_MAX_SIZE,
        Some(value) => match parse_size(value) {
            Ok(size) if (MIN_MAX_SIZE..=MAX_MAX_SIZE).contains(&size) => size,
            Ok(size) => {
                errors.push(ConfigError::InvalidSize {
                    field: "max_size",
                    value: value.to_string(),
                    reason: format!(
                        "{size} bytes is outside the supported range ({MIN_MAX_SIZE} bytes to 1TB)"
                    ),
                });
                DEFAULT_MAX_SIZE
            }
            Err(reason) => {
                errors.push(ConfigError::InvalidSize {
                    field: "max_size",
                    value: value.to_string(),
                    reason,
                });
                DEFAULT_MAX_SIZE
            }
        },
    };

    let max_files = raw.max_files.unwrap_or(3);
    if max_files > 100 {
        errors.push(ConfigError::InvalidMaxFiles { value: max_files });
        return None;
    }

    Some(LogSettings {
        dir,
        max_size,
        max_files,
    })
}

/// Parse a byte size such as `"512"`, `"64KB"`, `"10 MB"` or `"1GiB"`.
///
/// Both SI-style (`KB`, `MB`, `GB`) and binary (`KiB`, `MiB`, `GiB`) suffixes
/// are accepted and treated as powers of 1024, which is what people mean when
/// they size a log file.
pub(crate) fn parse_size(value: &str) -> Result<u64, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("the size must not be empty".to_string());
    }

    let split = trimmed
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(trimmed.len());
    let (number, suffix) = trimmed.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("{number:?} is not a number"))?;
    if number < 0.0 {
        return Err("the size must not be negative".to_string());
    }

    let multiplier = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1u64,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        "t" | "tb" | "tib" => 1024u64 * 1024 * 1024 * 1024,
        other => return Err(format!("unknown unit {other:?}; use B, KB, MB, GB or TB")),
    };

    let bytes = number * multiplier as f64;
    if !bytes.is_finite() || bytes > u64::MAX as f64 {
        return Err("the size is too large".to_string());
    }
    Ok(bytes as u64)
}

// ── Health-check validation ────────────────────────────────────────────────

/// Validate a `[services.<name>.health]` table.
///
/// Returns `None` (after pushing errors) when the table is unusable.
fn validate_health(
    raw: &RawHealthCheck,
    service: &str,
    errors: &mut Vec<ConfigError>,
) -> Option<HealthCheck> {
    let declared: Vec<&'static str> = [
        raw.command.is_some().then_some("command"),
        raw.http.is_some().then_some("http"),
        raw.tcp.is_some().then_some("tcp"),
    ]
    .into_iter()
    .flatten()
    .collect();

    let probe = match declared.as_slice() {
        [] => {
            errors.push(ConfigError::MissingHealthProbe {
                service: service.to_string(),
            });
            None
        }
        [_] => build_probe(raw, service, errors),
        _ => {
            errors.push(ConfigError::ConflictingHealthProbes {
                service: service.to_string(),
                probes: declared.join(", "),
            });
            None
        }
    };

    let interval = parse_duration_field(
        raw.interval.as_deref(),
        "health.interval",
        Duration::from_secs(2),
        service,
        DUR_100MS,
        DUR_1H,
        errors,
    );
    let timeout = parse_duration_field(
        raw.timeout.as_deref(),
        "health.timeout",
        Duration::from_secs(5),
        service,
        DUR_100MS,
        DUR_1H,
        errors,
    );
    let start_period = parse_duration_field(
        raw.start_period.as_deref(),
        "health.start_period",
        Duration::ZERO,
        service,
        Duration::ZERO,
        DUR_24H,
        errors,
    );

    let on_unhealthy = match raw.on_unhealthy.as_deref() {
        None | Some("restart") => UnhealthyAction::Restart,
        Some("ignore") => UnhealthyAction::Ignore,
        Some(other) => {
            errors.push(ConfigError::InvalidUnhealthyAction {
                service: service.to_string(),
                value: other.to_string(),
            });
            UnhealthyAction::Restart
        }
    };

    // `retries` is the number of consecutive failures needed to declare the
    // service unhealthy, so it is always at least one probe.
    let retries = raw.retries.unwrap_or(3).max(1);

    Some(HealthCheck {
        probe: probe?,
        interval,
        timeout,
        retries,
        start_period,
        on_unhealthy,
    })
}

/// Build the probe described by the (single) probe field that was set.
fn build_probe(
    raw: &RawHealthCheck,
    service: &str,
    errors: &mut Vec<ConfigError>,
) -> Option<HealthProbe> {
    let invalid =
        |field: &'static str, value: &str, reason: &str| ConfigError::InvalidHealthProbe {
            service: service.to_string(),
            field,
            value: value.to_string(),
            reason: reason.to_string(),
        };

    if let Some(command) = &raw.command {
        let Some(executable) = command.first() else {
            errors.push(invalid("command", "", "the command must not be empty"));
            return None;
        };
        if executable.is_empty() || command.iter().any(|part| part.contains('\0')) {
            errors.push(invalid(
                "command",
                executable,
                "the executable must not be empty and no argument may contain a NUL byte",
            ));
            return None;
        }
        return Some(HealthProbe::Command {
            executable: executable.clone(),
            args: command[1..].to_vec(),
        });
    }

    if let Some(url) = &raw.http {
        return match parse_http_url(url) {
            Ok(probe) => Some(probe),
            Err(reason) => {
                errors.push(invalid("http", url, &reason));
                None
            }
        };
    }

    if let Some(addr) = &raw.tcp {
        return match parse_host_port(addr) {
            Ok((host, port)) => Some(HealthProbe::Tcp { host, port }),
            Err(reason) => {
                errors.push(invalid("tcp", addr, &reason));
                None
            }
        };
    }

    None
}

/// Parse an `http://host[:port][/path]` URL into an [`HealthProbe::Http`].
///
/// Only plaintext HTTP is supported; use a `command` probe (e.g. `curl`) for
/// anything that needs TLS, redirects or authentication.
fn parse_http_url(url: &str) -> Result<HealthProbe, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| match url.split_once("://") {
            Some(("https", _)) => {
                "https is not supported; use a `command` probe such as `curl -fsS <url>`"
                    .to_string()
            }
            _ => "the URL must start with `http://`".to_string(),
        })?;

    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    if authority.contains('@') {
        return Err("credentials in the URL are not supported".to_string());
    }

    let (host, port) = match parse_host_port(authority) {
        Ok(hp) => hp,
        Err(_) if !authority.contains(':') => {
            if authority.is_empty() {
                return Err("the URL has no host".to_string());
            }
            (authority.to_string(), 80)
        }
        Err(reason) => return Err(reason),
    };

    Ok(HealthProbe::Http {
        url: url.to_string(),
        host,
        port,
        path: path.to_string(),
    })
}

/// Parse a `host:port` pair, accepting `[::1]:port` for IPv6 literals.
fn parse_host_port(addr: &str) -> Result<(String, u16), String> {
    let (host, port) = if let Some(rest) = addr.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .ok_or_else(|| "unterminated IPv6 literal".to_string())?;
        let port = rest
            .strip_prefix(':')
            .ok_or_else(|| "expected `[host]:port`".to_string())?;
        (host, port)
    } else {
        addr.rsplit_once(':')
            .ok_or_else(|| "expected `host:port`".to_string())?
    };

    if host.is_empty() {
        return Err("the host must not be empty".to_string());
    }
    let port: u16 = port
        .parse()
        .map_err(|_| format!("{port:?} is not a valid port number"))?;
    if port == 0 {
        return Err("the port must not be zero".to_string());
    }
    Ok((host.to_string(), port))
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
    fn expect_one_error(toml: &str) -> ConfigError {
        let errs = load_from_str(toml).unwrap_err();
        assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
        errs.into_iter().next().unwrap()
    }

    // ── Log settings ───────────────────────────────────────────────────────

    /// A config whose project carries the given `[project.logs]` body.
    fn with_logs(body: &str, service_extra: &str) -> String {
        format!(
            r#"
version = 1
[project]
name = "p"
[project.logs]
{body}
[services.api]
command = ["echo", "hi"]
{service_extra}
"#
        )
    }

    /// The single service every log test declares.
    fn api(cfg: &Config) -> &Service {
        cfg.services.values().next().expect("one service")
    }

    #[test]
    fn log_settings_default_to_a_project_local_directory() {
        let (cfg, _) = load_from_str(&with_logs("", "")).unwrap();
        let logs = cfg.project.logs.expect("logs enabled");

        assert!(logs.dir.ends_with(".servicrab/logs"), "{:?}", logs.dir);
        assert!(logs.dir.is_absolute());
        assert_eq!(logs.max_size, 10 * 1024 * 1024);
        assert_eq!(logs.max_files, 3);
    }

    #[test]
    fn no_logs_table_means_no_file_logging() {
        let (cfg, _) = load_from_str(
            r#"
version = 1
[project]
name = "p"
[services.api]
command = ["echo", "hi"]
"#,
        )
        .unwrap();
        assert!(cfg.project.logs.is_none());
    }

    #[test]
    fn log_sizes_accept_si_and_binary_suffixes() {
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("1B").unwrap(), 1);
        assert_eq!(parse_size("64KiB").unwrap(), 64 * 1024);
        assert_eq!(parse_size("10 MB").unwrap(), 10 * 1024 * 1024);
        assert_eq!(parse_size("1.5mb").unwrap(), 1024 * 1024 * 3 / 2);
        assert_eq!(parse_size("2G").unwrap(), 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn log_sizes_reject_nonsense() {
        assert!(parse_size("").is_err());
        assert!(parse_size("MB").is_err());
        assert!(parse_size("10 parsecs").is_err());
    }

    #[test]
    fn an_unknown_size_unit_is_a_config_error() {
        let err = expect_one_error(&with_logs(r#"max_size = "10 parsecs""#, ""));
        assert!(
            matches!(&err, ConfigError::InvalidSize { field, .. } if *field == "max_size"),
            "{err:?}"
        );
    }

    #[test]
    fn a_tiny_max_size_is_rejected() {
        let err = expect_one_error(&with_logs(r#"max_size = "10""#, ""));
        assert!(matches!(err, ConfigError::InvalidSize { .. }), "{err:?}");
    }

    #[test]
    fn too_many_rotated_files_are_rejected() {
        let err = expect_one_error(&with_logs("max_files = 101", ""));
        assert!(
            matches!(err, ConfigError::InvalidMaxFiles { value: 101 }),
            "{err:?}"
        );
    }

    #[test]
    fn keeping_no_rotated_files_is_allowed() {
        let (cfg, _) = load_from_str(&with_logs("max_files = 0", "")).unwrap();
        assert_eq!(cfg.project.logs.unwrap().max_files, 0);
    }

    #[test]
    fn a_service_can_opt_out_of_file_logging() {
        let (cfg, _) =
            load_from_str(&with_logs("", "[services.api.logs]\nenabled = false")).unwrap();
        assert!(!api(&cfg).log_to_file);
    }

    #[test]
    fn services_log_to_file_by_default() {
        let (cfg, _) = load_from_str(&with_logs("", "")).unwrap();
        assert!(api(&cfg).log_to_file);
    }

    #[test]
    fn an_absolute_log_dir_is_used_as_is() {
        let (cfg, _) =
            load_from_str(&with_logs(r#"dir = "/tmp/servicrab-test-logs""#, "")).unwrap();
        assert_eq!(
            cfg.project.logs.unwrap().dir,
            PathBuf::from("/tmp/servicrab-test-logs")
        );
    }

    #[test]
    fn a_log_file_where_a_directory_belongs_is_a_config_error() {
        let dir = TempDir::new().unwrap();
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, "hi").unwrap();

        let toml = with_logs(&format!(r#"dir = "{}""#, blocker.display()), "");
        let err = expect_one_error(&toml);
        assert!(matches!(err, ConfigError::InvalidLogDir { .. }), "{err:?}");
    }

    #[test]
    fn log_files_live_next_to_each_other_per_service() {
        let (cfg, _) = load_from_str(&with_logs("", "")).unwrap();
        let name = api(&cfg).name.clone();
        let logs = cfg.project.logs.unwrap();

        assert_eq!(logs.file_for(&name).file_name().unwrap(), "api.log");
        assert_eq!(
            logs.rotated_file_for(&name, 2).file_name().unwrap(),
            "api.log.2"
        );
    }

    // ── Health checks ──────────────────────────────────────────────────────

    /// A config with a single service carrying the given `[health]` body.
    fn with_health(body: &str) -> String {
        format!(
            r#"
version = 1
[project]
name = "p"
[services.api]
command = ["echo", "hi"]
[services.api.health]
{body}
"#
        )
    }

    #[test]
    fn a_health_check_defaults_are_applied() {
        let (cfg, _) = load_from_str(&with_health(r#"tcp = "127.0.0.1:5432""#)).unwrap();
        let health = cfg
            .services
            .values()
            .next()
            .unwrap()
            .health
            .clone()
            .unwrap();
        assert_eq!(
            health.probe,
            HealthProbe::Tcp {
                host: "127.0.0.1".to_string(),
                port: 5432
            }
        );
        assert_eq!(health.interval, Duration::from_secs(2));
        assert_eq!(health.timeout, Duration::from_secs(5));
        assert_eq!(health.retries, 3);
        assert_eq!(health.start_period, Duration::ZERO);
        assert_eq!(health.on_unhealthy, UnhealthyAction::Restart);
    }

    #[test]
    fn a_health_check_can_override_every_field() {
        let toml = with_health(
            r#"command = ["curl", "-fsS", "http://localhost/health"]
interval = "500ms"
timeout = "1s"
retries = 7
start_period = "10s"
on_unhealthy = "ignore""#,
        );
        let (cfg, _) = load_from_str(&toml).unwrap();
        let health = cfg
            .services
            .values()
            .next()
            .unwrap()
            .health
            .clone()
            .unwrap();
        assert_eq!(
            health.probe,
            HealthProbe::Command {
                executable: "curl".to_string(),
                args: vec!["-fsS".to_string(), "http://localhost/health".to_string()],
            }
        );
        assert_eq!(health.interval, Duration::from_millis(500));
        assert_eq!(health.timeout, Duration::from_secs(1));
        assert_eq!(health.retries, 7);
        assert_eq!(health.start_period, Duration::from_secs(10));
        assert_eq!(health.on_unhealthy, UnhealthyAction::Ignore);
    }

    #[test]
    fn an_http_probe_is_split_into_host_port_and_path() {
        let (cfg, _) = load_from_str(&with_health(
            r#"http = "http://127.0.0.1:8080/healthz?full=1""#,
        ))
        .unwrap();
        let health = cfg
            .services
            .values()
            .next()
            .unwrap()
            .health
            .clone()
            .unwrap();
        assert_eq!(
            health.probe,
            HealthProbe::Http {
                url: "http://127.0.0.1:8080/healthz?full=1".to_string(),
                host: "127.0.0.1".to_string(),
                port: 8080,
                path: "/healthz?full=1".to_string(),
            }
        );
    }

    #[test]
    fn an_http_probe_defaults_to_port_80_and_root() {
        let (cfg, _) = load_from_str(&with_health(r#"http = "http://example.test""#)).unwrap();
        let health = cfg
            .services
            .values()
            .next()
            .unwrap()
            .health
            .clone()
            .unwrap();
        assert_eq!(
            health.probe,
            HealthProbe::Http {
                url: "http://example.test".to_string(),
                host: "example.test".to_string(),
                port: 80,
                path: "/".to_string(),
            }
        );
    }

    #[test]
    fn https_health_urls_are_rejected_with_a_hint() {
        let err = expect_one_error(&with_health(r#"http = "https://example.test/health""#));
        let msg = err.to_string();
        assert!(msg.contains("https is not supported"), "{msg}");
    }

    #[test]
    fn a_health_table_without_a_probe_is_rejected() {
        let err = expect_one_error(&with_health(r#"interval = "1s""#));
        assert!(
            matches!(err, ConfigError::MissingHealthProbe { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_health_table_with_two_probes_is_rejected() {
        let err = expect_one_error(&with_health(
            r#"tcp = "127.0.0.1:5432"
http = "http://127.0.0.1:8080/""#,
        ));
        let msg = err.to_string();
        assert!(msg.contains("http, tcp"), "{msg}");
    }

    #[test]
    fn a_malformed_tcp_probe_is_rejected() {
        for (addr, needle) in [
            (r#"tcp = "127.0.0.1""#, "expected `host:port`"),
            (r#"tcp = "127.0.0.1:0""#, "must not be zero"),
            (r#"tcp = "127.0.0.1:https""#, "not a valid port"),
            (r#"tcp = ":5432""#, "host must not be empty"),
        ] {
            let err = expect_one_error(&with_health(addr));
            let msg = err.to_string();
            assert!(msg.contains(needle), "{addr}: {msg}");
        }
    }

    #[test]
    fn an_ipv6_tcp_probe_is_accepted() {
        let (cfg, _) = load_from_str(&with_health(r#"tcp = "[::1]:6379""#)).unwrap();
        let health = cfg
            .services
            .values()
            .next()
            .unwrap()
            .health
            .clone()
            .unwrap();
        assert_eq!(
            health.probe,
            HealthProbe::Tcp {
                host: "::1".to_string(),
                port: 6379
            }
        );
    }

    #[test]
    fn an_empty_probe_command_is_rejected() {
        let err = expect_one_error(&with_health("command = []"));
        assert!(
            matches!(err, ConfigError::InvalidHealthProbe { .. }),
            "{err}"
        );
    }

    #[test]
    fn an_unknown_on_unhealthy_action_is_rejected() {
        let err = expect_one_error(&with_health(
            r#"tcp = "127.0.0.1:5432"
on_unhealthy = "explode""#,
        ));
        assert!(
            matches!(err, ConfigError::InvalidUnhealthyAction { .. }),
            "{err}"
        );
    }

    #[test]
    fn an_out_of_range_health_interval_is_rejected() {
        let err = expect_one_error(&with_health(
            r#"tcp = "127.0.0.1:5432"
interval = "10ms""#,
        ));
        let msg = err.to_string();
        assert!(msg.contains("health.interval"), "{msg}");
    }

    #[test]
    fn zero_retries_are_normalised_to_one() {
        let (cfg, _) = load_from_str(&with_health(
            r#"tcp = "127.0.0.1:5432"
retries = 0"#,
        ))
        .unwrap();
        let health = cfg
            .services
            .values()
            .next()
            .unwrap()
            .health
            .clone()
            .unwrap();
        assert_eq!(health.retries, 1);
    }

    #[test]
    fn an_unknown_health_field_is_rejected() {
        let toml = with_health(
            r#"tcp = "127.0.0.1:5432"
retires = 3"#,
        );
        let raw: Result<RawConfig, _> = toml::from_str(&toml);
        assert!(raw.is_err(), "unknown health fields must not be accepted");
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
                logs: None,
                name: "p".into(),
                env: Default::default(),
                env_file: None,
            },
            services: {
                let mut m = std::collections::BTreeMap::new();
                m.insert(
                    "s".to_string(),
                    crate::raw::RawService {
                        command: vec!["exe\0cutable".to_string()],
                        cwd: None,
                        env: Default::default(),
                        env_file: None,
                        depends_on: None,
                        autostart: true,
                        restart: Default::default(),
                        restart_delay: None,
                        restart_max_delay: None,
                        max_restarts: None,
                        stable_after: None,
                        shutdown_signal: None,
                        shutdown_timeout: None,
                        health: None,
                        logs: None,
                        watch: None,
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

    // ── env_file ───────────────────────────────────────────────────────────

    /// Result of validating a config written into a temp dir.
    type Validated = Result<(Config, Vec<ConfigWarning>), Vec<ConfigError>>;

    /// Write `servicrab.toml` plus a set of sibling files, then validate.
    /// The `TempDir` is returned so the caller keeps the tree alive.
    fn load_with_files(toml: &str, files: &[(&str, &str)]) -> (TempDir, Validated) {
        let dir = TempDir::new().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, toml).unwrap();
        let raw: RawConfig = toml::from_str(toml).expect("raw parse failed in test");
        let result = validate_raw(raw, &path);
        (dir, result)
    }

    #[test]
    fn a_service_env_file_is_loaded() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nenv_file=\".env\"\n";
        let (_dir, result) = load_with_files(toml, &[(".env", "FROM_FILE=yes\n")]);
        let (cfg, _) = result.unwrap();
        let svc = &cfg.services[&ServiceName("s".into())];
        assert_eq!(svc.env.get("FROM_FILE").map(String::as_str), Some("yes"));
        assert_eq!(svc.env_files.len(), 1);
    }

    #[test]
    fn a_project_env_file_reaches_every_service() {
        let toml = "version=1\n[project]\nname=\"p\"\nenv_file=[\".env\"]\n[services.s]\ncommand=[\"echo\"]\n[services.t]\ncommand=[\"echo\"]\n";
        let (_dir, result) = load_with_files(toml, &[(".env", "SHARED=1\n")]);
        let (cfg, _) = result.unwrap();
        for name in ["s", "t"] {
            let svc = &cfg.services[&ServiceName(name.into())];
            assert_eq!(svc.env.get("SHARED").map(String::as_str), Some("1"));
        }
        assert_eq!(cfg.project.env_files.len(), 1);
    }

    #[test]
    fn env_files_are_layered_in_declaration_order() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nenv_file=[\".env\", \".env.local\"]\n";
        let (_dir, result) =
            load_with_files(toml, &[(".env", "K=base\n"), (".env.local", "K=local\n")]);
        let (cfg, _) = result.unwrap();
        let svc = &cfg.services[&ServiceName("s".into())];
        assert_eq!(svc.env.get("K").map(String::as_str), Some("local"));
    }

    #[test]
    fn an_explicit_env_entry_beats_the_env_file() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nenv_file=\".env\"\n[services.s.env]\nK=\"inline\"\n";
        let (_dir, result) = load_with_files(toml, &[(".env", "K=file\n")]);
        let (cfg, _) = result.unwrap();
        let svc = &cfg.services[&ServiceName("s".into())];
        assert_eq!(svc.env.get("K").map(String::as_str), Some("inline"));
    }

    #[test]
    fn a_service_env_file_beats_the_project_env_file() {
        let toml = "version=1\n[project]\nname=\"p\"\nenv_file=\".env\"\n[services.s]\ncommand=[\"echo\"]\nenv_file=\".env.svc\"\n";
        let (_dir, result) = load_with_files(
            toml,
            &[(".env", "K=project\n"), (".env.svc", "K=service\n")],
        );
        let (cfg, _) = result.unwrap();
        let svc = &cfg.services[&ServiceName("s".into())];
        assert_eq!(svc.env.get("K").map(String::as_str), Some("service"));
    }

    // ── watch ──────────────────────────────────────────────────────────────

    #[test]
    fn a_watch_block_resolves_paths_against_the_service_cwd() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\n[services.s.watch]\npaths=[\"src\"]\n";
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, toml).unwrap();
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let (cfg, _) = validate_raw(raw, &path).unwrap();

        let watch = cfg.services[&ServiceName("s".into())]
            .watch
            .as_ref()
            .expect("watch settings");
        assert_eq!(watch.paths.len(), 1);
        assert!(watch.paths[0].ends_with("src"));
        assert_eq!(watch.interval, Duration::from_secs(1));
        assert_eq!(watch.debounce, Duration::from_millis(300));
    }

    #[test]
    fn watch_always_ignores_git_and_servicrab_state() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\n[services.s.watch]\npaths=[\"src\"]\nignore=[\"target\"]\n";
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, toml).unwrap();
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let (cfg, _) = validate_raw(raw, &path).unwrap();

        let watch = cfg.services[&ServiceName("s".into())]
            .watch
            .as_ref()
            .expect("watch settings");
        for entry in [".git", ".servicrab", "target"] {
            assert!(
                watch.ignore.iter().any(|i| i == entry),
                "{entry} should be ignored, got {:?}",
                watch.ignore
            );
        }
    }

    #[test]
    fn a_watch_interval_below_the_minimum_is_a_config_error() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\n[services.s.watch]\npaths=[\"src\"]\ninterval=\"1ms\"\n";
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, toml).unwrap();
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let errs = validate_raw(raw, &path).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::DurationOutOfRange { field, .. } if *field == "watch.interval")));
    }

    #[test]
    fn a_missing_env_file_is_a_config_error() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nenv_file=\"nope.env\"\n";
        let (_dir, result) = load_with_files(toml, &[]);
        let errs = result.unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidEnvFile { .. })));
    }

    #[test]
    fn a_malformed_env_file_is_a_config_error() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\nenv_file=\".env\"\n";
        let (_dir, result) = load_with_files(toml, &[(".env", "not an assignment\n")]);
        let errs = result.unwrap_err();
        let message = errs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(message.contains("env_file"), "got: {message}");
        assert!(message.contains("line 1"), "got: {message}");
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

    // ── Dependency conditions ──────────────────────────────────────────────

    /// Two services where `b` depends on `a` under `condition`, and `a` carries
    /// whatever `a_extra` adds.
    fn with_condition(condition: &str, a_extra: &str) -> String {
        format!(
            r#"
version = 1
[project]
name = "p"
[services.a]
command = ["echo"]
{a_extra}
[services.b]
command = ["echo"]
depends_on = {{ a = {{ condition = "{condition}" }} }}
"#
        )
    }

    #[test]
    fn a_declared_condition_survives_validation() {
        let (cfg, _) =
            load_from_str(&with_condition("service_completed_successfully", "")).unwrap();
        let b = &cfg.services[&ServiceName("b".to_string())];

        assert_eq!(b.depends_on.len(), 1);
        assert_eq!(b.depends_on[0].service.as_str(), "a");
        assert_eq!(
            b.depends_on[0].condition,
            Some(DependencyCondition::ServiceCompletedSuccessfully)
        );
    }

    #[test]
    fn the_short_form_leaves_the_condition_to_the_dependency() {
        let toml = r#"
version = 1
[project]
name = "p"
[services.a]
command = ["echo"]
[services.plain]
command = ["echo"]
depends_on = ["a"]
[services.checked]
command = ["echo"]
depends_on = ["a"]
[services.checked.health]
tcp = "127.0.0.1:1"
"#;
        let (cfg, _) = load_from_str(toml).unwrap();
        let plain = &cfg.services[&ServiceName("plain".to_string())];
        let checked = &cfg.services[&ServiceName("checked".to_string())];

        // Nothing is resolved at load time, so that adding a health check to a
        // dependency does not count as a change to its dependents.
        assert_eq!(plain.depends_on[0].condition, None);
        assert_eq!(
            plain.depends_on[0].condition_for(&cfg.services[&ServiceName("a".to_string())]),
            DependencyCondition::ServiceStarted
        );
        assert_eq!(
            plain.depends_on[0].condition_for(checked),
            DependencyCondition::ServiceHealthy
        );
    }

    #[test]
    fn an_unknown_condition_errors() {
        let err = expect_one_error(&with_condition("service_started_maybe", ""));
        assert!(
            matches!(&err, ConfigError::InvalidDependencyCondition { dep, value, .. }
                if dep == "a" && value == "service_started_maybe"),
            "{err:?}"
        );
    }

    #[test]
    fn service_healthy_needs_the_dependency_to_have_a_health_check() {
        let err = expect_one_error(&with_condition("service_healthy", ""));
        assert!(
            matches!(&err, ConfigError::DependencyNotHealthChecked { service, dep }
                if service == "b" && dep == "a"),
            "{err:?}"
        );

        load_from_str(&with_condition(
            "service_healthy",
            "[services.a.health]\ntcp = \"127.0.0.1:1\"",
        ))
        .expect("a health-checked dependency should pass");
    }

    #[test]
    fn service_completed_successfully_rejects_a_dependency_that_always_restarts() {
        let err = expect_one_error(&with_condition(
            "service_completed_successfully",
            r#"restart = "always""#,
        ));
        assert!(
            matches!(&err, ConfigError::DependencyNeverCompletes { service, dep }
                if service == "b" && dep == "a"),
            "{err:?}"
        );

        // `on-failure` is fine: it retries until the run succeeds, which is
        // exactly what waiting for a successful completion wants.
        load_from_str(&with_condition(
            "service_completed_successfully",
            r#"restart = "on-failure""#,
        ))
        .expect("a retried dependency should pass");
    }

    #[test]
    fn the_table_form_is_checked_like_the_list_form() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\ndepends_on={ ghost = {} }\n";
        let err = expect_one_error(toml);
        assert!(
            matches!(&err, ConfigError::UnknownDependency { dep, .. } if dep == "ghost"),
            "{err:?}"
        );
    }

    #[test]
    fn a_typo_inside_the_table_form_names_the_field() {
        let toml = "version=1\n[project]\nname=\"p\"\n[services.a]\ncommand=[\"echo\"]\n[services.b]\ncommand=[\"echo\"]\ndepends_on={ a = { conditon = \"service_started\" } }\n";
        let err = toml::from_str::<RawConfig>(toml).expect_err("unknown field must be rejected");
        assert!(err.to_string().contains("conditon"), "{err}");
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
                env_file: None,
                depends_on: None,
                autostart: true,
                restart: Default::default(),
                restart_delay: None,
                restart_max_delay: None,
                max_restarts: None,
                stable_after: None,
                shutdown_signal: None,
                shutdown_timeout: None,
                health: None,
                logs: None,
                watch: None,
            },
        );
        RawConfig {
            version: 1,
            project: crate::raw::RawProject {
                logs: None,
                name: "p".into(),
                env: Default::default(),
                env_file: None,
            },
            services,
        }
    }
}
