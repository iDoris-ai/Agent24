//! agent24d — the Agent24 Rust daemon (SPEC-002).
//!
//! B2 scope: serve skeleton — `/api/v1/health`, bearer-token handshake via a
//! ready pipe, dynamic port, CancellationToken-driven graceful shutdown.

mod approval_callback;
mod approvals;
// Capability authority is staged ahead of its route policy. Keep the allowance
// confined to that deferred module rather than weakening the daemon's lints.
#[allow(
    dead_code,
    unused_imports,
    reason = "capability route policy has not landed"
)]
mod capabilities;
mod domain;
mod events;
mod events_emit;
#[cfg(unix)]
mod host_bootstrap;
#[cfg(not(unix))]
mod host_bootstrap {
    #![allow(dead_code)] // Staged until the capability startup layer consumes it.

    use std::io;

    pub struct ReadyWriter(std::convert::Infallible);
    pub struct ParentLiveness(std::convert::Infallible);

    /// Capability bootstrap is unavailable without Unix descriptor validation.
    pub fn open_stdio() -> io::Result<(ReadyWriter, ParentLiveness)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "capability bootstrap requires Unix IPC descriptor validation",
        ))
    }

    impl ReadyWriter {
        pub async fn send(&mut self, _: &serde_json::Value) -> io::Result<()> {
            match self.0 {}
        }
    }

    impl ParentLiveness {
        pub async fn closed(self) {
            match self.0 {}
        }
    }
}
mod lifecycle;
mod mcp;
mod module_approval_broker;
mod module_approvals;
mod os_config;
mod os_memory;
mod os_memory_page;
mod os_routes;
mod overrides;
mod routes;
mod runs;
mod schedules;
mod server;

use clap::{Parser, Subcommand, ValueEnum};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AuthModeArg {
    LegacySingleToken,
    Capabilities,
}

impl From<AuthModeArg> for agent24_protocol::state_file::AuthMode {
    fn from(value: AuthModeArg) -> Self {
        match value {
            AuthModeArg::LegacySingleToken => Self::LegacySingleToken,
            AuthModeArg::Capabilities => Self::Capabilities,
        }
    }
}

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
        /// Authentication authority. Capability mode keeps host authority out
        /// of daemon.json and emits it only on the trusted ready pipe.
        #[arg(long, value_enum, default_value_t = AuthModeArg::LegacySingleToken)]
        auth_mode: AuthModeArg,
        /// Confirm that stdin/stdout are private pipes owned by the host.
        /// Required for capability mode and hidden from ordinary CLI help.
        #[arg(long, hide = true, default_value_t = false)]
        host_bootstrap_stdio: bool,
    },
}

fn main() -> std::process::ExitCode {
    // Before anything else — logging, the runtime — can start a thread: when
    // this process is a module's trampoline it becomes the module here, and
    // flagging its fds close-on-exec is race-free only while it has one thread.
    // An ordinary daemon start returns at once.
    agent24_os_proto::launch::run_as_trampoline_if_asked();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr) // stdout is reserved for the private ready line
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            port,
            ephemeral,
            auth_mode,
            host_bootstrap_stdio,
        } => run_serve(port, ephemeral, auth_mode.into(), host_bootstrap_stdio),
    }
}

fn run_serve(
    port: u16,
    ephemeral: bool,
    auth_mode: agent24_protocol::state_file::AuthMode,
    host_bootstrap_stdio: bool,
) -> std::process::ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            tracing::error!("failed to start tokio runtime: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let cancel = CancellationToken::new();
    let result = runtime.block_on(server::serve(
        port,
        ephemeral,
        auth_mode,
        host_bootstrap_stdio,
        cancel,
    ));
    // Bounded, not the default `Drop`, which waits for every blocking task:
    // a supervisor walking a large package tree in `spawn_blocking` when the
    // shutdown came would otherwise hold the exit past `serve`'s own bound
    // (TASKS B2; review of SUP-4, round 1).
    runtime.shutdown_timeout(lifecycle::RUNTIME_TEARDOWN);
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("serve failed: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
