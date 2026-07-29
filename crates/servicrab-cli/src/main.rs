//! `servicrab` — a lightweight cross-platform process supervisor.
//!
//! # Architecture notes
//!
//! The CLI is intentionally thin: it delegates all business logic to
//! `servicrab-core` and uses `tokio` only for the `run` subcommand where we
//! need async process I/O.
//!
//! ## Future phases (TODOs)
//!
//! - TODO(phase-2): Add `servicrab start` / `stop` / `restart` commands that
//!   talk to a background daemon over a Unix socket.
//! - TODO(phase-2): Add `servicrab status` to show a rich process table.
//! - TODO(phase-2): Add `servicrab logs <service>` to stream logs from the
//!   daemon.
//! - TODO(phase-3): Add `servicrab up` / `down` for whole-stack lifecycle.
//! - TODO(phase-3): Add `servicrab watch` to restart services on file changes.

use anyhow::Context;
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
    /// Generate an example servicrab.toml in the current directory.
    Init,

    /// Parse and validate a servicrab.toml configuration file.
    Check {
        /// Path to the configuration file.
        #[arg(default_value = "servicrab.toml")]
        config: std::path::PathBuf,
    },

    /// List services defined in a servicrab.toml configuration file.
    List {
        /// Path to the configuration file.
        #[arg(default_value = "servicrab.toml")]
        config: std::path::PathBuf,
    },

    /// Run a single service in the foreground, forwarding its stdout/stderr.
    Run {
        /// Name of the service to run (as defined in [services.<name>]).
        service: String,

        /// Path to the configuration file.
        #[arg(default_value = "servicrab.toml")]
        config: std::path::PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialise structured logging.  The log level can be overridden via the
    // RUST_LOG environment variable (e.g. `RUST_LOG=debug servicrab list`).
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init => commands::init::run().context("init failed"),
        Commands::Check { config } => commands::check::run(&config).context("check failed"),
        Commands::List { config } => commands::list::run(&config).context("list failed"),
        Commands::Run { service, config } => commands::run::run(&service, &config)
            .await
            .context("run failed"),
    }
}
