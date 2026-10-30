//! agent24d — the Agent24 Rust daemon (SPEC-002).
//!
//! B2 scope: serve skeleton — `/api/v1/health`, bearer-token handshake via a
//! ready pipe, dynamic port, CancellationToken-driven graceful shutdown.

mod agentear_timings;
mod approval_callback;
mod approvals;
mod attach_commands;
mod attach_listener;
mod attach_registry;
mod attached;
mod attached_routes;
mod authz;
// Remaining public capability primitives are staged for the next endpoints.
#[allow(
    dead_code,
    unused_imports,
    reason = "capability API is intentionally staged beyond route policy"
)]
mod capabilities;
mod comm_routes;
mod consent_routes;
mod decide_routes;
mod domain;
#[allow(dead_code, reason = "frozen CORE-2 slice seam; wired by S1/S2")]
mod enroll_api;
mod enroll_endpoint;
// The feature trusts unsigned binaries and auto-answers presence: it must
// never reach a release build (#1446 F2). The e2e builds it in debug.
#[cfg(all(feature = "test-enrollment", not(debug_assertions)))]
compile_error!("the `test-enrollment` feature is for debug test builds only");
#[cfg(feature = "test-enrollment")]
#[path = "../tests/support/enroll.rs"]
mod enroll_test_support;
mod events;
mod events_emit;
mod grant_expiry;
mod hardware;
#[cfg(unix)]
mod host_bootstrap;
mod hyphae_install;
mod idoris_routes;
mod knowledge_host;
#[allow(dead_code)] // Wired at start (KB-C1-04 6c-4) and by the private interface (6d-2).
mod knowledge_store;
#[cfg(not(unix))]
mod host_bootstrap {
    #![allow(
        dead_code,
        reason = "capability bootstrap is unavailable without Unix descriptor validation"
    )]

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
mod memory_callback;
mod memory_maintenance;
mod memory_routes;
#[allow(
    dead_code,
    reason = "CORE-5b coordinator stays unwired; production startup still uses Store::open"
)]
mod migration_coordinator;
#[allow(dead_code, reason = "CORE-5c building blocks stay unwired until 5d-5f")]
mod migration_guard;
mod model_callback;
mod module_approval_broker;
mod module_approvals;
mod module_tool_runtime;
mod os_config;
mod os_memory;
mod os_memory_page;
mod os_routes;
mod overrides;
mod peer_runs;
mod presence_desktop;
mod routes;
mod runs;
mod scheduler_callback;
mod scheduler_deliver;
mod schedules;
mod server;

const _: () = assert!(!migration_coordinator::ENABLED);
mod timing_recorder;
mod usage_recorder;

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
        /// Service mode: capability daemon with no parent pipes that opens
        /// the enrolment endpoint `<data_dir>/enroll.sock`. Exclusive with
        /// --host-bootstrap-stdio (which opens the endpoint by itself).
        #[arg(long, default_value_t = false)]
        enroll_endpoint: bool,
        /// Leave the bearer token out of the legacy `ready` line (it still goes
        /// to the 0600 `daemon.json`). Passed by the LaunchAgent plist, whose
        /// `StandardOutPath` would otherwise persist the token in a log file.
        #[arg(long, default_value_t = false)]
        ready_omit_token: bool,
    },
}

fn main() -> std::process::ExitCode {
    // Before anything else — logging, the runtime — can start a thread: when
    // this process is a module's trampoline it becomes the module here, and
    // flagging its fds close-on-exec is race-free only while it has one thread.
    // An ordinary daemon start returns at once.
    if std::env::args().nth(1).as_deref() == Some("__idoris-install-child") {
        return agent24_idoris::install::run_install_child();
    }
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
            enroll_endpoint,
            ready_omit_token,
        } => run_serve(
            port,
            ephemeral,
            auth_mode.into(),
            host_bootstrap_stdio,
            enroll_endpoint,
            ready_omit_token,
        ),
    }
}

fn run_serve(
    port: u16,
    ephemeral: bool,
    auth_mode: agent24_protocol::state_file::AuthMode,
    host_bootstrap_stdio: bool,
    enroll_endpoint: bool,
    ready_omit_token: bool,
) -> std::process::ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            let error = format!("failed to start tokio runtime: {err}");
            tracing::error!("{error}");
            say_start_failed("start_failed", &error);
            return std::process::ExitCode::FAILURE;
        }
    };

    let cancel = CancellationToken::new();
    let result = runtime.block_on(server::serve(
        port,
        ephemeral,
        auth_mode,
        host_bootstrap_stdio,
        enroll_endpoint,
        ready_omit_token,
        cancel,
        None,
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
            let already_running = err
                .get_ref()
                .is_some_and(|inner| inner.is::<server::AlreadyRunning>());
            let code = if already_running {
                "already_running"
            } else {
                "start_failed"
            };
            say_start_failed(code, &err.to_string());
            std::process::ExitCode::FAILURE
        }
    }
}

/// SPEC-002 §4: a start that fails before its ready line says why on stdout
/// too, `{"type":"failed","code":…,"error":…}`, so a launcher that does not
/// keep stderr can still tell the user. Never after the ready line; a write
/// error is ignored, since the reader may be gone.
fn say_start_failed(code: &str, error: &str) {
    use std::io::Write;
    let ready_sent = server::READY_SENT.load(std::sync::atomic::Ordering::SeqCst);
    let Some(line) = start_failed_line(ready_sent, code, error) else {
        return;
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// The `failed` line, or none once the ready line has gone out.
fn start_failed_line(ready_sent: bool, code: &str, error: &str) -> Option<String> {
    (!ready_sent)
        .then(|| serde_json::json!({ "type": "failed", "code": code, "error": error }).to_string())
}

#[cfg(test)]
mod start_failed_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::start_failed_line;

    #[test]
    fn nothing_follows_the_ready_line() {
        assert_eq!(start_failed_line(true, "start_failed", "x"), None);
        let line = start_failed_line(false, "already_running", "held").unwrap();
        let line: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            line,
            serde_json::json!({ "type": "failed", "code": "already_running", "error": "held" })
        );
    }
}
