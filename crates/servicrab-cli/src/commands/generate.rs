//! `servicrab generate <systemd|launchd>` — emit an init-system unit that
//! supervises the whole project through `servicrab daemon`.
//!
//! The generated unit never contains service definitions: it starts the
//! servicrab daemon, which reads `servicrab.toml` at boot. That keeps the unit
//! stable — adding a service to the stack means editing the config and running
//! `servicrab reload`, not regenerating and reinstalling anything.

use std::path::{Path, PathBuf};
use std::time::Duration;

use servicrab_core::{load, resolve_config_path, Config};

/// Which init system to generate for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum Target {
    /// A systemd unit file (Linux).
    Systemd,
    /// A launchd property list (macOS).
    Launchd,
}

/// Whether the unit belongs to the machine or to the current user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum Scope {
    /// A system-wide unit, started at boot (`/etc/systemd/system`,
    /// `/Library/LaunchDaemons`).
    #[default]
    System,
    /// A per-user unit, started at login (`systemctl --user`,
    /// `~/Library/LaunchAgents`).
    User,
}

/// Options for `servicrab generate`.
#[derive(Debug, Clone, Default)]
pub struct GenerateOptions {
    /// System-wide or per-user unit.
    pub scope: Scope,
    /// Where to write the unit; stdout when omitted. A directory gets the
    /// conventional file name for the target.
    pub output: Option<PathBuf>,
    /// Account the daemon should run as (system scope only).
    pub user: Option<String>,
    /// Profiles the generated unit should start with.
    pub profiles: Vec<String>,
}

/// Everything a unit template needs that does not come from the config.
#[derive(Debug, Clone)]
struct Context {
    /// Absolute path of the `servicrab` executable.
    program: PathBuf,
    /// Absolute path of the config file.
    config: PathBuf,
    /// Directory the daemon should run in (the config's directory).
    working_dir: PathBuf,
    scope: Scope,
    user: Option<String>,
    /// How long the init system should wait for a clean stop.
    stop_timeout: Duration,
    /// Profiles to put on the daemon's command line.
    profiles: Vec<String>,
}

impl Context {
    /// The `--profile` arguments the unit has to pass on, as one string ready
    /// to append to a command line.
    fn profile_flags(&self) -> String {
        self.profiles
            .iter()
            .map(|profile| format!(" --profile {profile}"))
            .collect()
    }
}

/// Extra time on top of the slowest service's shutdown timeout, so the init
/// system never kills the daemon while it is still stopping the stack.
const STOP_MARGIN: Duration = Duration::from_secs(15);
/// Lower bound for the stop timeout, matching systemd's usual default.
const MIN_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Run the `generate` subcommand.
pub fn run(target: Target, config: Option<&Path>, options: GenerateOptions) -> Result<i32, String> {
    let path = resolve_config_path(config).map_err(|e| format!("could not find config: {e}"))?;
    let (cfg, warnings) = load(&path).map_err(|errors| {
        let msgs: Vec<String> = errors.iter().map(|e| format!("  • {e}")).collect();
        format!(
            "✗ {} has {} error(s):\n{}",
            path.display(),
            errors.len(),
            msgs.join("\n")
        )
    })?;
    for warning in &warnings {
        eprintln!("⚠  {warning}");
    }

    let program = std::env::current_exe()
        .map_err(|e| format!("could not find the servicrab executable: {e}"))?;
    let context = Context {
        program,
        working_dir: path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
        config: path,
        scope: options.scope,
        user: options.user.clone(),
        stop_timeout: stop_timeout(&cfg),
        profiles: options.profiles.clone(),
    };

    let unit = match target {
        Target::Systemd => systemd_unit(&cfg, &context),
        Target::Launchd => launchd_plist(&cfg, &context),
    };
    let file_name = unit_file_name(target, &cfg);

    match options.output.as_deref() {
        None => {
            print!("{unit}");
            eprintln!("{}", install_hint(target, options.scope, &file_name));
        }
        Some(output) => {
            let destination = if output.is_dir() {
                output.join(&file_name)
            } else {
                output.to_path_buf()
            };
            std::fs::write(&destination, &unit)
                .map_err(|e| format!("could not write {}: {e}", destination.display()))?;
            println!("✓ wrote {}", destination.display());
            println!("{}", install_hint(target, options.scope, &file_name));
        }
    }
    Ok(0)
}

/// The conventional file name for a generated unit.
fn unit_file_name(target: Target, cfg: &Config) -> String {
    match target {
        Target::Systemd => format!("servicrab-{}.service", cfg.project.name),
        Target::Launchd => format!("{}.plist", launchd_label(cfg)),
    }
}

/// The reverse-DNS label launchd identifies the job by.
fn launchd_label(cfg: &Config) -> String {
    format!("com.servicrab.{}", cfg.project.name)
}

/// How long the init system should allow for a clean stop.
///
/// The daemon stops its services in reverse dependency order, so the slowest
/// service's shutdown timeout is the floor for the whole stack.
fn stop_timeout(cfg: &Config) -> Duration {
    let slowest = cfg
        .services
        .values()
        .map(|service| service.shutdown_timeout)
        .max()
        .unwrap_or_default();
    (slowest + STOP_MARGIN).max(MIN_STOP_TIMEOUT)
}

/// Render a systemd unit for the project.
fn systemd_unit(cfg: &Config, context: &Context) -> String {
    let program = quote_systemd(&context.program);
    let config = quote_systemd(&context.config);
    let mut unit = String::new();

    unit.push_str(&format!(
        "# systemd unit for the servicrab project \"{}\".\n\
         # Generated by servicrab {}; edit it freely, it is never regenerated.\n\n",
        cfg.project.name,
        env!("CARGO_PKG_VERSION")
    ));

    unit.push_str("[Unit]\n");
    unit.push_str(&format!(
        "Description=servicrab stack \"{}\"\n",
        cfg.project.name
    ));
    unit.push_str("Documentation=https://github.com/gaborini/servicrab\n");
    if context.scope == Scope::System {
        unit.push_str("Wants=network-online.target\n");
        unit.push_str("After=network-online.target\n");
    }
    unit.push('\n');

    unit.push_str("[Service]\n");
    unit.push_str("Type=simple\n");
    unit.push_str(&format!(
        "WorkingDirectory={}\n",
        quote_systemd(&context.working_dir)
    ));
    unit.push_str(&format!(
        "ExecStart={program} daemon --config {config}{}\n",
        context.profile_flags()
    ));
    // `systemctl reload` picks up config changes without stopping services
    // that did not change.
    unit.push_str(&format!("ExecReload={program} reload --config {config}\n"));
    unit.push_str("Restart=on-failure\n");
    unit.push_str("RestartSec=5\n");
    unit.push_str("KillSignal=SIGTERM\n");
    // The daemon stops its own children, but `mixed` guarantees nothing is
    // left behind if it dies unexpectedly.
    unit.push_str("KillMode=mixed\n");
    unit.push_str(&format!(
        "TimeoutStopSec={}\n",
        context.stop_timeout.as_secs()
    ));
    if context.scope == Scope::System {
        if let Some(user) = &context.user {
            unit.push_str(&format!("User={user}\n"));
            unit.push_str(&format!("Group={user}\n"));
        }
    }
    unit.push('\n');

    unit.push_str("[Install]\n");
    unit.push_str(match context.scope {
        Scope::System => "WantedBy=multi-user.target\n",
        Scope::User => "WantedBy=default.target\n",
    });
    unit
}

/// Render a launchd property list for the project.
fn launchd_plist(cfg: &Config, context: &Context) -> String {
    let label = launchd_label(cfg);
    let logs = context.working_dir.join(".servicrab");
    let mut plist = String::new();

    plist.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    plist.push_str(
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    );
    plist.push_str(&format!(
        "<!-- launchd job for the servicrab project \"{}\", generated by servicrab {}. -->\n",
        escape_xml(cfg.project.name.as_str()),
        env!("CARGO_PKG_VERSION")
    ));
    plist.push_str("<plist version=\"1.0\">\n<dict>\n");

    plist.push_str(&format!(
        "\t<key>Label</key>\n\t<string>{}</string>\n",
        escape_xml(&label)
    ));
    plist.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    let arguments = [
        context.program.display().to_string(),
        "daemon".to_string(),
        "--config".to_string(),
        context.config.display().to_string(),
    ]
    .into_iter()
    .chain(
        context
            .profiles
            .iter()
            .flat_map(|profile| ["--profile".to_string(), profile.clone()]),
    );
    for argument in arguments {
        plist.push_str(&format!("\t\t<string>{}</string>\n", escape_xml(&argument)));
    }
    plist.push_str("\t</array>\n");

    plist.push_str(&format!(
        "\t<key>WorkingDirectory</key>\n\t<string>{}</string>\n",
        escape_xml(&context.working_dir.display().to_string())
    ));
    plist.push_str("\t<key>RunAtLoad</key>\n\t<true/>\n");
    // Restart the daemon when it dies, but respect a clean `servicrab down`.
    plist.push_str(
        "\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n",
    );
    plist.push_str(&format!(
        "\t<key>StandardOutPath</key>\n\t<string>{}</string>\n",
        escape_xml(&logs.join("launchd.out.log").display().to_string())
    ));
    plist.push_str(&format!(
        "\t<key>StandardErrorPath</key>\n\t<string>{}</string>\n",
        escape_xml(&logs.join("launchd.err.log").display().to_string())
    ));
    plist.push_str(&format!(
        "\t<key>ExitTimeOut</key>\n\t<integer>{}</integer>\n",
        context.stop_timeout.as_secs()
    ));
    plist.push_str("\t<key>ProcessType</key>\n\t<string>Background</string>\n");
    if context.scope == Scope::System {
        if let Some(user) = &context.user {
            plist.push_str(&format!(
                "\t<key>UserName</key>\n\t<string>{}</string>\n",
                escape_xml(user)
            ));
        }
    }

    plist.push_str("</dict>\n</plist>\n");
    plist
}

/// How to install the generated unit, printed next to it rather than into it.
fn install_hint(target: Target, scope: Scope, file_name: &str) -> String {
    match (target, scope) {
        (Target::Systemd, Scope::System) => format!(
            "\nInstall with:\n  \
             sudo cp {file_name} /etc/systemd/system/\n  \
             sudo systemctl daemon-reload\n  \
             sudo systemctl enable --now {file_name}\n\n\
             Then `sudo systemctl reload {file_name}` applies config changes."
        ),
        (Target::Systemd, Scope::User) => format!(
            "\nInstall with:\n  \
             mkdir -p ~/.config/systemd/user\n  \
             cp {file_name} ~/.config/systemd/user/\n  \
             systemctl --user daemon-reload\n  \
             systemctl --user enable --now {file_name}\n\n\
             Then `systemctl --user reload {file_name}` applies config changes."
        ),
        (Target::Launchd, Scope::System) => format!(
            "\nInstall with:\n  \
             sudo cp {file_name} /Library/LaunchDaemons/\n  \
             sudo launchctl bootstrap system /Library/LaunchDaemons/{file_name}\n\n\
             Then `servicrab reload` applies config changes."
        ),
        (Target::Launchd, Scope::User) => format!(
            "\nInstall with:\n  \
             cp {file_name} ~/Library/LaunchAgents/\n  \
             launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{file_name}\n\n\
             Then `servicrab reload` applies config changes."
        ),
    }
}

/// Quote a path for a systemd directive when it contains whitespace.
fn quote_systemd(path: &Path) -> String {
    let text = path.display().to_string();
    if text.contains(char::is_whitespace) {
        format!("\"{}\"", text.replace('"', "\\\""))
    } else {
        text
    }
}

/// Escape the five XML entities so odd paths cannot break the plist.
fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;

    use tempfile::TempDir;

    /// Load a config from TOML the way the command does.
    fn config(toml: &str) -> (TempDir, Config) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("servicrab.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(toml.as_bytes()).unwrap();
        let (cfg, _) = load(&path).expect("valid test config");
        (dir, cfg)
    }

    const BASE: &str = r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["/usr/bin/api"]
"#;

    fn context() -> Context {
        Context {
            program: PathBuf::from("/usr/local/bin/servicrab"),
            config: PathBuf::from("/srv/demo/servicrab.toml"),
            working_dir: PathBuf::from("/srv/demo"),
            scope: Scope::System,
            user: None,
            profiles: Vec::new(),
            stop_timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn a_systemd_unit_starts_the_daemon_and_reloads_the_config() {
        let (_dir, cfg) = config(BASE);
        let unit = systemd_unit(&cfg, &context());

        assert!(
            unit.contains("Description=servicrab stack \"demo\""),
            "{unit}"
        );
        assert!(
            unit.contains(
                "ExecStart=/usr/local/bin/servicrab daemon --config /srv/demo/servicrab.toml"
            ),
            "{unit}"
        );
        assert!(
            unit.contains(
                "ExecReload=/usr/local/bin/servicrab reload --config /srv/demo/servicrab.toml"
            ),
            "{unit}"
        );
        assert!(unit.contains("WorkingDirectory=/srv/demo"), "{unit}");
        assert!(unit.contains("WantedBy=multi-user.target"), "{unit}");
        assert!(unit.contains("KillMode=mixed"), "{unit}");
    }

    #[test]
    fn a_user_scoped_systemd_unit_drops_system_only_directives() {
        let (_dir, cfg) = config(BASE);
        let unit = systemd_unit(
            &cfg,
            &Context {
                scope: Scope::User,
                user: Some("deploy".to_string()),
                ..context()
            },
        );

        assert!(unit.contains("WantedBy=default.target"), "{unit}");
        assert!(!unit.contains("network-online.target"), "{unit}");
        // A user unit already runs as the user.
        assert!(!unit.contains("User="), "{unit}");
    }

    #[test]
    fn a_system_unit_can_run_as_another_account() {
        let (_dir, cfg) = config(BASE);
        let unit = systemd_unit(
            &cfg,
            &Context {
                user: Some("deploy".to_string()),
                ..context()
            },
        );

        assert!(unit.contains("User=deploy"), "{unit}");
        assert!(unit.contains("Group=deploy"), "{unit}");
    }

    #[test]
    fn paths_with_spaces_are_quoted_for_systemd() {
        let (_dir, cfg) = config(BASE);
        let unit = systemd_unit(
            &cfg,
            &Context {
                config: PathBuf::from("/srv/my demo/servicrab.toml"),
                working_dir: PathBuf::from("/srv/my demo"),
                ..context()
            },
        );

        assert!(
            unit.contains("--config \"/srv/my demo/servicrab.toml\""),
            "{unit}"
        );
        assert!(unit.contains("WorkingDirectory=\"/srv/my demo\""), "{unit}");
    }

    #[test]
    fn the_stop_timeout_follows_the_slowest_service() {
        let (_dir, cfg) = config(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["/usr/bin/api"]
shutdown_timeout = "60s"
[services.worker]
command = ["/usr/bin/worker"]
shutdown_timeout = "5s"
"#,
        );

        assert_eq!(stop_timeout(&cfg), Duration::from_secs(75));
    }

    #[test]
    fn the_stop_timeout_never_drops_below_the_floor() {
        let (_dir, cfg) = config(BASE);
        assert_eq!(stop_timeout(&cfg), MIN_STOP_TIMEOUT);
    }

    #[test]
    fn a_launchd_plist_is_well_formed() {
        let (_dir, cfg) = config(BASE);
        let plist = launchd_plist(&cfg, &context());

        assert!(plist.starts_with("<?xml version=\"1.0\""), "{plist}");
        assert!(plist.trim_end().ends_with("</plist>"), "{plist}");
        assert!(
            plist.contains("<key>Label</key>\n\t<string>com.servicrab.demo</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/usr/local/bin/servicrab</string>"),
            "{plist}"
        );
        assert!(plist.contains("<string>--config</string>"), "{plist}");
        assert!(
            plist.contains("<string>/srv/demo/.servicrab/launchd.out.log</string>"),
            "{plist}"
        );
        assert!(plist.contains("<integer>30</integer>"), "{plist}");
        assert_eq!(
            plist.matches("<dict>").count(),
            plist.matches("</dict>").count()
        );
    }

    #[test]
    fn a_system_plist_can_run_as_another_account() {
        let (_dir, cfg) = config(BASE);
        let plist = launchd_plist(
            &cfg,
            &Context {
                user: Some("deploy".to_string()),
                ..context()
            },
        );
        assert!(plist.contains("<key>UserName</key>"), "{plist}");

        let agent = launchd_plist(
            &cfg,
            &Context {
                scope: Scope::User,
                user: Some("deploy".to_string()),
                ..context()
            },
        );
        assert!(!agent.contains("UserName"), "{agent}");
    }

    #[test]
    fn xml_special_characters_are_escaped() {
        assert_eq!(
            escape_xml("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );

        let (_dir, cfg) = config(BASE);
        let plist = launchd_plist(
            &cfg,
            &Context {
                working_dir: PathBuf::from("/srv/a&b"),
                ..context()
            },
        );
        assert!(plist.contains("<string>/srv/a&amp;b</string>"), "{plist}");
        assert!(!plist.contains("/srv/a&b<"), "{plist}");
    }

    #[test]
    fn unit_file_names_follow_each_platforms_convention() {
        let (_dir, cfg) = config(BASE);
        assert_eq!(
            unit_file_name(Target::Systemd, &cfg),
            "servicrab-demo.service"
        );
        assert_eq!(
            unit_file_name(Target::Launchd, &cfg),
            "com.servicrab.demo.plist"
        );
    }

    #[test]
    fn install_hints_match_the_scope() {
        assert!(install_hint(Target::Systemd, Scope::System, "u.service")
            .contains("/etc/systemd/system/"));
        assert!(
            install_hint(Target::Systemd, Scope::User, "u.service").contains("systemctl --user")
        );
        assert!(install_hint(Target::Launchd, Scope::System, "u.plist")
            .contains("/Library/LaunchDaemons/"));
        assert!(install_hint(Target::Launchd, Scope::User, "u.plist").contains("LaunchAgents"));
    }
}
