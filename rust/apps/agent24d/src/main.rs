//! agent24d — the Agent24 Rust daemon (SPEC-002).
//!
//! B2 scope: serve skeleton — `/api/v1/health`, bearer-token handshake via the
//! stdout ready line, dynamic port, CancellationToken-driven graceful shutdown.

mod approvals;
mod events;
mod routes;
mod runs;
mod schedules;
mod server;

use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "agent24d", version, about = "Agent24 daemon")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon (127.0.0.1 only; --port 0 picks a free port)
    Serve {
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Ephemeral instance (CLI standalone mode): skip the singleton lock
        /// and the discovery state file
        #[arg(long, default_value_t = false)]
        ephemeral: bool,
    },
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr) // stdout is reserved for the ready line
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Serve { port, ephemeral } => run_serve(port, ephemeral),
    }
}

fn run_serve(port: u16, ephemeral: bool) -> std::process::ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            tracing::error!("failed to start tokio runtime: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let cancel = CancellationToken::new();
    let result = runtime.block_on(server::serve(port, ephemeral, cancel));
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("serve failed: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
