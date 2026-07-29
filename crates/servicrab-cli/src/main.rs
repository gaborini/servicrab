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
//!
//! ## Future phases (TODOs)
//!
//! - TODO(phase-2): Add `start` / `stop` / `restart` commands that talk to
//!   a background daemon over a Unix socket.
//! - TODO(phase-2): Add `status` to show a rich process table.
//! - TODO(phase-2): Add `logs <service>` to stream logs from the daemon.
//! - TODO(phase-3): Add `down` for stopping a detached stack.

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod commands;
mod style;

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
}

fn main() {
    let cli = Cli::parse();

    // `run` has no other progress output, so its lifecycle transitions are
    // logged by default.  `up` renders the same information from its event
    // stream, so logging is quiet there to avoid printing everything twice.
    // Either way `RUST_LOG` wins (e.g. `RUST_LOG=debug servicrab up`).
    let default_filter = match cli.command {
        Commands::Run { .. } => "servicrab_core=info",
        Commands::Up { .. } => "error",
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
        } => commands::up::run(
            &services,
            config.as_deref(),
            commands::up::UpOptions {
                no_restart,
                no_prefix,
                timestamps,
                abort_on_failure,
            },
        ),
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
