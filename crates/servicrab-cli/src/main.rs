//! `servicrab` — a lightweight cross-platform process supervisor.
//!
//! ## Subcommands
//!
//! - `servicrab init [--path PATH] [--force]` — create example config
//! - `servicrab check [--config PATH]` — validate config, print summary
//! - `servicrab list [--config PATH] [--json]` — list services
//!
//! ## Future phases (TODOs)
//!
//! - TODO(phase-2): Add `start` / `stop` / `restart` commands that talk to
//!   a background daemon over a Unix socket.
//! - TODO(phase-2): Add `status` to show a rich process table.
//! - TODO(phase-2): Add `logs <service>` to stream logs from the daemon.
//! - TODO(phase-3): Add `up` / `down` for whole-stack lifecycle.

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod commands;

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
}

fn main() {
    // Initialise structured logging.  The log level can be overridden via the
    // RUST_LOG environment variable (e.g. `RUST_LOG=debug servicrab list`).
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Init { path, force } => commands::init::run(&path, force),
        Commands::Check { config } => commands::check::run(config.as_deref()),
        Commands::List { config, json } => commands::list::run(config.as_deref(), json),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
