//! `servicrab` — a lightweight cross-platform process supervisor.
//!
//! ## Subcommands
//!
//! - `servicrab init [--path PATH] [--force]` — create example config
//! - `servicrab check [--config PATH]` — validate config, print summary
//! - `servicrab list [--config PATH] [--json]` — list services
//! - `servicrab run <SERVICE> [--config PATH] [--no-restart]` — run one
//!   service in the foreground (Linux/macOS)
//! - `servicrab up [SERVICE...]` — run a whole stack in the foreground
//!   (Linux/macOS)
//! - `servicrab watch [SERVICE...]` — `up` with restart-on-file-change
//!   (Linux/macOS)
//!
//! - `servicrab logs [SERVICE...]` — read the captured log files
//! - `servicrab start` / `stop` / `restart` / `reload` / `status` / `down` /
//!   `daemon` — background daemon control (Linux/macOS)
//! - `servicrab events [SERVICE...]` — follow the daemon's event stream
//!   (Linux/macOS)
//! - `servicrab generate <systemd|launchd>` — write an init-system unit
//! - `servicrab completions <SHELL>` — print a shell completion script

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod commands;
mod daemon;
mod style;
mod wire;

#[derive(Parser, Debug)]
#[command(
    name = "servicrab",
    about = "A lightweight process supervisor for local stacks, homelabs, and small servers",
    version,
    author
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a documented example servicrab.toml in the current directory
    /// (or the path specified by --path).
    Init {
        /// Where to write the config file.
        #[arg(long, default_value = "servicrab.toml")]
        path: std::path::PathBuf,

        /// Overwrite the file if it already exists.
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Load and validate a servicrab.toml; print project name, service count,
    /// start order, and any warnings.
    Check {
        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,
    },

    /// List all services defined in a servicrab.toml.
    List {
        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Output in JSON format instead of the human-readable table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Run a single configured service in the foreground, applying its
    /// restart and shutdown policy.  Linux and macOS only.
    Run {
        /// Name of the service to run, as defined in [services.<name>].
        service: String,

        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Never restart the service, whatever the configured policy says.
        #[arg(long, default_value_t = false)]
        no_restart: bool,
    },

    /// Run a whole stack in the foreground: start every service in dependency
    /// order, interleave their output, and stop them in reverse order on
    /// Ctrl+C.  Linux and macOS only.
    Up {
        /// Services to start.  Their dependencies are always started too.
        /// With no names, every service with autostart = true is started.
        services: Vec<String>,

        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Never restart services, whatever their configured policy says.
        #[arg(long, default_value_t = false)]
        no_restart: bool,

        /// Do not prefix output lines with the service name.
        #[arg(long, default_value_t = false)]
        no_prefix: bool,

        /// Prefix output lines with a UTC timestamp.
        #[arg(long, default_value_t = false)]
        timestamps: bool,

        /// Stop the whole stack as soon as one service fails.
        #[arg(long, default_value_t = false)]
        abort_on_failure: bool,

        /// Print one JSON event per line on stdout instead of rendering for a
        /// terminal.
        #[arg(long)]
        json: bool,
    },

    /// Run a stack in the foreground and restart services when their watched
    /// files change.  Identical to `up`, except that it refuses to start when
    /// no selected service declares a [watch] block.  Linux and macOS only.
    Watch {
        /// Services to start.  Their dependencies are always started too.
        /// With no names, every service with autostart = true is started.
        services: Vec<String>,

        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Never restart services, whatever their configured policy says.
        #[arg(long, default_value_t = false)]
        no_restart: bool,

        /// Do not prefix output lines with the service name.
        #[arg(long, default_value_t = false)]
        no_prefix: bool,

        /// Prefix output lines with a UTC timestamp.
        #[arg(long, default_value_t = false)]
        timestamps: bool,

        /// Stop the whole stack as soon as one service fails.
        #[arg(long, default_value_t = false)]
        abort_on_failure: bool,

        /// Print one JSON event per line on stdout instead of rendering for a
        /// terminal.
        #[arg(long)]
        json: bool,
    },

    /// Show the captured log files of one or more services.  Requires a
    /// [project.logs] section in the configuration.
    Logs {
        /// Services to show.  With no names, every service that writes log
        /// files is shown.
        services: Vec<String>,

        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Keep printing new lines as they are written.
        #[arg(long, short = 'f', default_value_t = false)]
        follow: bool,

        /// Number of trailing lines to show per service.
        #[arg(long, short = 'n', default_value_t = 50)]
        lines: usize,

        /// Do not prefix output lines with the service name.
        #[arg(long, default_value_t = false)]
        no_prefix: bool,
    },

    /// Start the stack in the background and return immediately, or start
    /// individual services inside an already running daemon.
    /// Linux and macOS only.
    Start {
        /// Services to start inside a running daemon.  With no names, the
        /// daemon itself is started.
        services: Vec<String>,

        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Never restart services, whatever their configured policy says.
        #[arg(long, default_value_t = false)]
        no_restart: bool,
    },

    /// Stop individual services without stopping the daemon.
    /// Linux and macOS only.
    Stop {
        /// Services to stop.
        #[arg(required = true)]
        services: Vec<String>,

        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,
    },

    /// Restart individual services inside the running daemon.
    /// Linux and macOS only.
    Restart {
        /// Services to restart.
        #[arg(required = true)]
        services: Vec<String>,

        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,
    },

    /// Re-read the configuration and apply it to the running daemon.
    /// Linux and macOS only.
    Reload {
        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,
    },

    /// Follow the running daemon's live event stream.  Linux and macOS only.
    Events {
        /// Only follow these services.  Defaults to all of them.
        services: Vec<String>,

        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Print one JSON object per line instead of rendering for a terminal.
        #[arg(long)]
        json: bool,

        /// Do not prefix lines with the service name.
        #[arg(long)]
        no_prefix: bool,

        /// Prefix lines with a UTC timestamp.
        #[arg(long, short = 't')]
        timestamps: bool,

        /// Leave captured stdout/stderr out of the stream.
        #[arg(long)]
        no_logs: bool,
    },

    /// Show what the background daemon is doing.  Linux and macOS only.
    Status {
        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Print machine-readable JSON instead of a table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Stop the background daemon and every service it supervises.
    /// Linux and macOS only.
    Down {
        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,
    },

    /// Generate an init-system unit that runs the stack through
    /// `servicrab daemon`.
    Generate {
        /// Which init system to generate for.
        target: commands::generate::Target,

        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Whether the unit is system-wide or per-user.
        #[arg(long, value_enum, default_value_t = commands::generate::Scope::System)]
        scope: commands::generate::Scope,

        /// Write the unit to this file (or into this directory) instead of
        /// stdout.
        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,

        /// Account the daemon should run as (system scope only).
        #[arg(long)]
        user: Option<String>,
    },

    /// Print a shell completion script to stdout.
    Completions {
        /// Shell to generate completions for.
        shell: clap_complete::Shell,
    },

    /// Supervise the stack in the foreground while serving the daemon socket.
    /// This is what `start` runs in the background; use it directly under
    /// systemd, launchd or a container.  Linux and macOS only.
    Daemon {
        /// Path to the configuration file.  If omitted, discovers
        /// servicrab.toml by walking up from the current directory.
        #[arg(long, short = 'c')]
        config: Option<std::path::PathBuf>,

        /// Never restart services, whatever their configured policy says.
        #[arg(long, default_value_t = false)]
        no_restart: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    // `run` has no other progress output, so its lifecycle transitions are
    // logged by default.  `up` renders the same information from its event
    // stream, so logging is quiet there to avoid printing everything twice.
    // Either way `RUST_LOG` wins (e.g. `RUST_LOG=debug servicrab up`).
    let default_filter = match cli.command {
        Commands::Run { .. } => "servicrab_core=info",
        Commands::Up { .. } | Commands::Watch { .. } => "error",
        // The daemon's log file is the only trace it leaves, so it keeps the
        // full lifecycle history.
        Commands::Daemon { .. } => "servicrab=info,servicrab_core=info",
        _ => "warn",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        // Diagnostics belong on stderr; stdout is reserved for command output
        // and for the stdout of supervised services.
        .with_writer(std::io::stderr)
        .with_target(false)
        .compact()
        .init();

    // Commands return the process exit code to use; `0` means success.
    let result = match cli.command {
        Commands::Init { path, force } => commands::init::run(&path, force).map(|()| 0),
        Commands::Check { config } => commands::check::run(config.as_deref()).map(|()| 0),
        Commands::List { config, json } => commands::list::run(config.as_deref(), json).map(|()| 0),
        Commands::Run {
            service,
            config,
            no_restart,
        } => commands::run::run(&service, config.as_deref(), no_restart),
        Commands::Up {
            services,
            config,
            no_restart,
            no_prefix,
            timestamps,
            abort_on_failure,
            json,
        } => commands::up::run(
            &services,
            config.as_deref(),
            commands::up::UpOptions {
                no_restart,
                no_prefix,
                timestamps,
                abort_on_failure,
                require_watch: false,
                json,
            },
        ),
        Commands::Watch {
            services,
            config,
            no_restart,
            no_prefix,
            timestamps,
            abort_on_failure,
            json,
        } => commands::up::run(
            &services,
            config.as_deref(),
            commands::up::UpOptions {
                no_restart,
                no_prefix,
                timestamps,
                abort_on_failure,
                require_watch: true,
                json,
            },
        ),
        Commands::Start {
            services,
            config,
            no_restart,
        } => commands::daemon::start(config.as_deref(), &services, no_restart),
        Commands::Stop { services, config } => commands::daemon::stop(config.as_deref(), &services),
        Commands::Restart { services, config } => {
            commands::daemon::restart(config.as_deref(), &services)
        }
        Commands::Reload { config } => commands::daemon::reload(config.as_deref()),
        Commands::Events {
            services,
            config,
            json,
            no_prefix,
            timestamps,
            no_logs,
        } => commands::events::events(
            &services,
            config.as_deref(),
            commands::events::EventsOptions {
                json,
                no_prefix,
                timestamps,
                no_logs,
            },
        ),
        Commands::Status { config, json } => commands::daemon::status(config.as_deref(), json),
        Commands::Down { config } => commands::daemon::down(config.as_deref()),
        Commands::Daemon { config, no_restart } => {
            commands::daemon::daemon(config.as_deref(), no_restart)
        }
        Commands::Generate {
            target,
            config,
            scope,
            output,
            user,
        } => commands::generate::run(
            target,
            config.as_deref(),
            commands::generate::GenerateOptions {
                scope,
                output,
                user,
            },
        ),
        Commands::Completions { shell } => commands::completions::run::<Cli>(shell).map(|()| 0),
        Commands::Logs {
            services,
            config,
            follow,
            lines,
            no_prefix,
        } => commands::logs::run(
            &services,
            config.as_deref(),
            commands::logs::LogsOptions {
                follow,
                lines,
                no_prefix,
            },
        )
        .map(|()| 0),
    };

    match result {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
