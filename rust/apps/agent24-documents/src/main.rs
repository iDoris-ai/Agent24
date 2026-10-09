//! Entry point the kernel spawns (`spawn.command` in `domain-os.yml`).
//!
//! `connect()` takes over the kernel-bound listener (fd 3) and dials the
//! callback socket; `serve()` nests [`agent24_documents::router`] under
//! `/api/v1/documents`. Storage is opened in between. Logs go to stderr, which the supervisor captures.

use std::process::ExitCode;

use agent24_documents::state::AppState;
use agent24_os_sdk::Module;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let module = match Module::builder(agent24_documents::MANIFEST_YAML)
        .connect()
        .await
    {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "documents: failed to connect to the kernel");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(data_dir = %module.data_dir().display(), "documents: connected");

    // A storage failure is logged and reported by the routes, not fatal.
    let state = AppState::open(module.data_dir()).await;
    match module.serve(agent24_documents::router(state)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "documents: server stopped");
            ExitCode::FAILURE
        }
    }
}
