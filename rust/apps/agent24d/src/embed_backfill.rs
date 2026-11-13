//! VR-4: after the ready line, attach the loopback embedder and keep the
//! owner's vectors caught up. Nothing here runs on the startup path.

use std::sync::Arc;

use agent24_agent::EmbedBackfill;
use agent24_memory::embedder::LocalEmbedder;
use agent24_memory::vector::Embedder;
use agent24_models::EmbeddingClient;
use tokio_util::sync::CancellationToken;

/// Probe, attach, reindex, then wait for remembers. Returns when `cancel` fires
/// or the embedder cannot be built (keyword search stays on).
pub async fn drive(link: EmbedBackfill, cancel: CancellationToken) {
    if cancel.is_cancelled() {
        return;
    }
    let client = match EmbeddingClient::from_env() {
        Ok(client) => client,
        Err(err) => {
            tracing::info!(
                reason = %log_reason(&err),
                "vector recall off; keyword search stays"
            );
            return;
        }
    };
    let opened = tokio::select! {
        () = cancel.cancelled() => return,
        opened = LocalEmbedder::open(link.kv(), client) => opened,
    };
    let embedder = match opened {
        Ok(embedder) => Arc::new(embedder),
        Err(err) => {
            tracing::info!(
                reason = %log_reason(&err),
                "vector recall off; keyword search stays"
            );
            return;
        }
    };
    link.install(Arc::clone(&embedder));
    let model = embedder.model_id().to_owned();
    backfill_once(&link, &embedder, &cancel).await;
    match link
        .kv()
        .vector_retriever((*embedder).clone())
        .coverage(link.owner())
        .await
    {
        Ok((embedded, total)) => {
            tracing::info!(model, embedded, total, "vector index ready");
        }
        Err(err) => {
            tracing::info!(
                model,
                reason = %log_reason(&err),
                "vector index ready"
            );
        }
    }
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = link.notified() => {}
        }
        if cancel.is_cancelled() {
            break;
        }
        backfill_once(&link, &embedder, &cancel).await;
    }
}

async fn backfill_once(link: &EmbedBackfill, embedder: &LocalEmbedder, cancel: &CancellationToken) {
    if cancel.is_cancelled() {
        return;
    }
    // Paused personal memory: do not embed. Recall already returns before any
    // query embedding; this is the same switch for the backfill.
    match link.kv().memory_enabled(link.owner()).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(err) => {
            tracing::warn!(
                reason = %log_reason(&err),
                "embedding backfill skipped"
            );
            return;
        }
    }
    let owner = link.owner().to_owned();
    let retriever = link.kv().vector_retriever(embedder.clone());
    let indexed = tokio::select! {
        () = cancel.cancelled() => return,
        indexed = retriever.reindex(&owner) => indexed,
    };
    if let Err(err) = indexed {
        // The reason is the client/store error. It does not include the fact
        // text or an embeddings response body.
        tracing::warn!(reason = %log_reason(&err), "embedding backfill failed");
    }
}

/// One short line. Embedding failures already omit the response body (VR-1);
/// this also drops anything past the first line and caps the length so a
/// future error string cannot drag a fact or a body into the log.
fn log_reason(err: &impl std::fmt::Display) -> String {
    let mut line = err.to_string();
    if let Some(end) = line.find('\n') {
        line.truncate(end);
    }
    const MAX: usize = 180;
    if line.chars().count() > MAX {
        line = line.chars().take(MAX).collect();
    }
    line
}
