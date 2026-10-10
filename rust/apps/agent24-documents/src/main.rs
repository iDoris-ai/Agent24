//! Entry point the kernel spawns (`spawn.command` in `domain-os.yml`).
//!
//! `connect()` takes over the kernel-bound listener (fd 3) and dials the
//! callback socket; `serve()` nests [`agent24_documents::router`] under
//! `/api/v1/documents`. Storage is opened in between. Logs go to stderr, which the supervisor captures.

use std::process::ExitCode;

use std::sync::Arc;

use agent24_documents::engine::pdfkit::PdfKit;
use agent24_documents::engine::{Engine, Layers};
use agent24_documents::events::Events;
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

    // Taken before `serve`, which consumes the module. Without the grant
    // (it is in domain-os.yml) jobs are simply not announced.
    let events = module.events().map_or_else(Events::default, |client| {
        Events::start(Arc::new(move |kind, payload| {
            let client = client.clone();
            Box::pin(async move {
                client
                    .emit(kind, payload, None)
                    .await
                    .map_err(|e| e.to_string())
            })
        }))
    });
    // The read engine ships next to this binary on macOS (§3.1); without it
    // read operations report engine_unavailable.
    let engine = PdfKit::find().map(|e| Arc::new(e) as Arc<dyn Engine>);
    tracing::info!(engine = engine.is_some(), "documents: read engine");
    // A storage failure is logged and reported by the routes, not fatal.
    let state = AppState::open_serving(module.data_dir(), events, Layers::new(engine)).await;
    match module.serve(agent24_documents::router(state)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "documents: server stopped");
            ExitCode::FAILURE
        }
    }
}
