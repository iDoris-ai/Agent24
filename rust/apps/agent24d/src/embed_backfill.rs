//! VR-4: after the ready line, attach the loopback embedder and keep the
//! owner's vectors caught up. VR-6 attaches the near-miss rewriter in the
//! same place when the router already has a local chat model. Nothing here
//! runs on the startup path.

use std::sync::Arc;

use agent24_agent::{EmbedBackfill, RouterRewriter};
use agent24_memory::embedder::LocalEmbedder;
use agent24_memory::hybrid::QueryRewriter;
use agent24_memory::vector::Embedder;
use agent24_models::EmbeddingClient;
use agent24_models::router::ModelRouter;
use agent24_protocol::Model;
use tokio_util::sync::CancellationToken;

/// How long to wait before trying to open the embedder again.
const OPEN_RETRY: std::time::Duration = std::time::Duration::from_secs(15);

/// Probe, attach, reindex, then wait for remembers. Returns when `cancel` fires.
/// Until the embedder opens, keyword search serves recall.
pub async fn drive(link: EmbedBackfill, router: Arc<ModelRouter>, cancel: CancellationToken) {
    if cancel.is_cancelled() {
        return;
    }
    // iDoris is set up in the background after the daemon starts and may
    // restart later, so an empty slot is not final: keep trying until the
    // embedder opens. Keyword search serves recall meanwhile.
    let mut logged = None;
    let embedder = loop {
        let attempt = match EmbeddingClient::from_router(Arc::clone(&router)) {
            Ok(client) => match client.check_available() {
                Err(err) => Err(log_reason(&err)),
                Ok(()) => tokio::select! {
                () = cancel.cancelled() => return,
                opened = LocalEmbedder::open(link.kv(), client.clone()) => opened.map_err(|err| {
                    match client.check_available() {
                        Err(unavailable) => log_reason(&unavailable),
                        Ok(()) => log_reason(&err),
                    }
                }),
                },
            },
            Err(err) => Err(log_reason(&err)),
        };
        match attempt {
            Ok(embedder) => break Arc::new(embedder),
            Err(err) => {
                if logged.as_ref() != Some(&err) {
                    tracing::info!(
                        reason = %err,
                        "vector recall waiting for iDoris; keyword search stays"
                    );
                    logged = Some(err);
                }
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(OPEN_RETRY) => {}
                }
            }
        }
    };
    let rewriter = local_rewriter(&router, &cancel).await;
    link.install_retriever(Arc::clone(&embedder), rewriter);
    let model = embedder.model_id().to_owned();
    backfill_once(&link, &embedder, &router, &cancel).await;
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
    let availability = EmbeddingClient::from_router(Arc::clone(&router));
    let Ok(availability) = availability else {
        return;
    };
    let mut waiting = false;
    loop {
        let notified = tokio::select! {
            () = cancel.cancelled() => break,
            () = link.notified() => true,
            () = tokio::time::sleep(OPEN_RETRY) => false,
        };
        if let Err(err) = availability.check_available() {
            if !waiting {
                tracing::info!(reason = %log_reason(&err), "vector recall waiting for iDoris; keyword search stays");
            }
            waiting = true;
            continue;
        }
        if !notified && !waiting {
            continue;
        }
        waiting = false;
        if cancel.is_cancelled() {
            break;
        }
        backfill_once(&link, &embedder, &router, &cancel).await;
    }
}

async fn backfill_once(
    link: &EmbedBackfill,
    embedder: &LocalEmbedder,
    router: &Arc<ModelRouter>,
    cancel: &CancellationToken,
) {
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
        if EmbeddingClient::from_router(Arc::clone(router))
            .and_then(|client| client.check_available())
            .is_err()
        {
            return;
        }
        // The reason is the client/store error. It does not include the fact
        // text or an embeddings response body.
        tracing::warn!(reason = %log_reason(&err), "embedding backfill failed");
    }
}

/// The router's default model, when a local provider is actually serving it
/// and it is not the embedding model. Anything else (OCR, speech, image)
/// stays off: recall then behaves as it did before VR-6.
async fn local_rewriter(
    router: &Arc<ModelRouter>,
    cancel: &CancellationToken,
) -> Option<Arc<dyn QueryRewriter>> {
    let Some(preferred) = router.default_model() else {
        tracing::info!("query rewrite off; no local chat model");
        return None;
    };
    if !is_chat_model(&preferred) {
        tracing::info!("query rewrite off; no local chat model");
        return None;
    }
    let models = router.models(cancel).await;
    let served = models
        .iter()
        .any(|model| model.id == preferred && is_local(model));
    if !served {
        tracing::info!("query rewrite off; no local chat model");
        return None;
    }
    tracing::info!(model = %preferred, "query rewrite on");
    Some(Arc::new(RouterRewriter::new(
        Arc::clone(router),
        cancel.clone(),
        preferred,
    )))
}

fn is_local(model: &Model) -> bool {
    model.tier.eq_ignore_ascii_case("local") || model.tier.eq_ignore_ascii_case("lora")
}

fn is_chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id != agent24_models::DEFAULT_EMBED_MODEL.to_ascii_lowercase()
        && !id.contains("embed")
        && !id.contains("bge")
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn backfill_without_embed_binding_does_not_send_requests() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = agent24_models::IdorisGateway::from_credentials(
            &format!("http://{}", listener.local_addr().unwrap()),
            zeroize::Zeroizing::new("idk_test".into()),
        )
        .unwrap();
        gateway.set_embedding_enabled(false);
        let router = Arc::new(ModelRouter::with_defaults(vec![]));
        router.replace_idoris(gateway);
        let cancel = CancellationToken::new();
        let summarizer = Arc::new(agent24_agent::RouterSummarizer::new(
            Arc::clone(&router),
            cancel.clone(),
        ));
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        let memory = agent24_agent::SessionMemory::new(kv, summarizer).with_owner("owner".into());
        tokio::time::pause();
        let task = tokio::spawn(drive(
            memory.embed_backfill(),
            Arc::clone(&router),
            cancel.clone(),
        ));
        assert!(
            tokio::time::timeout(OPEN_RETRY * 3, listener.accept())
                .await
                .is_err()
        );
        let gateway = agent24_models::IdorisGateway::from_credentials(
            &format!("http://{}", listener.local_addr().unwrap()),
            zeroize::Zeroizing::new("idk_reconnected".into()),
        )
        .unwrap();
        router.replace_idoris(gateway);
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let sent = sent.clone();
                async move {
                    sent.send(body).unwrap();
                    let mut vector = vec![0.0; agent24_models::EMBED_DIMS];
                    vector[0] = 1.0;
                    (
                        [("x-idoris-served-locality", "loopback")],
                        axum::Json(serde_json::json!({"data":[{"index":0,"embedding":vector}]})),
                    )
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::advance(OPEN_RETRY).await;
        tokio::time::resume();
        let request = tokio::time::timeout(OPEN_RETRY * 2, received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request["model"], "idoris/embed");
        cancel.cancel();
        task.await.unwrap();
        server.abort();
    }
}
