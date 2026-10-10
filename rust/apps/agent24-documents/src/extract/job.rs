//! The extract job's work (ADR-DOC-02 §3.2, §7), run by [`runner`]: the
//! revision's pinned text layer, every field asked of the local model, and
//! the extraction written with the job's success in one transaction.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::Field;
use super::ground::Source;
use super::run::{Extracted, Failure, Job, run};
use crate::engine::LayerFailure;
use crate::extractions::{MAX_VALUE_BYTES, NewExtraction, insert};
use crate::id::{IdKind, new_id};
use crate::idem::request_sha256;
use crate::jobs::runner::{self, Claim};
use crate::read::load;
use crate::state::{AppState, Storage};
use crate::text_layer::ParseStatus;

/// How long the job waits for its text layer to be built: a request waits
/// 8 s (§3.1); a job in the background can wait for a long document.
const LAYER_WAIT: Duration = Duration::from_secs(10 * 60);
/// How often a job waiting for its layer asks whether it is cancelled.
const LAYER_POLL: Duration = Duration::from_secs(1);

/// What an extract job works on, as written when it was created.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub document_id: String,
    pub revision: i64,
    pub schema_sha256: String,
    pub fields: Vec<Field>,
}

/// The schema's hash, as the business key holds it: the JCS of
/// `{"fields": …}` as requested (§5.4).
///
/// # Errors
/// When the fields hold a number JCS cannot write (they hold none).
pub fn schema_sha256(fields: &[Field]) -> Result<String, String> {
    request_sha256(&json!({ "fields": fields })).map_err(|e| e.to_string())
}

/// Runs attempt `attempt` of extract job `job_id` in the background.
pub fn spawn(state: AppState, storage: Arc<Storage>, job_id: String, attempt: i64) {
    runner::spawn(storage, job_id, attempt, move |storage, claim| async move {
        extract(&state, &storage, &claim).await
    });
}

/// The job as `run` sees it: going on while its attempt is running, and
/// its progress written to it. A status it cannot read stops the work too,
/// and the error is kept for the runner to settle: an unreadable claim is
/// never taken for a running one.
struct Running<'a> {
    storage: &'a Storage,
    claim: &'a Claim,
    failed: std::sync::Mutex<Option<sqlx::Error>>,
}

impl Running<'_> {
    async fn running(&self) -> bool {
        match self.claim.is_running(self.storage).await {
            Ok(running) => running,
            Err(e) => {
                *self
                    .failed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
                false
            }
        }
    }

    /// The error that stopped the work, if one did.
    fn failure(&self) -> Option<sqlx::Error> {
        self.failed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl Job for Running<'_> {
    fn go_on(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(self.running())
    }

    fn progress(&self, done: usize, total: usize) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let progress =
            json!({ "stage": "extract", "done": done, "total": total, "unit": "model calls" });
        Box::pin(async move {
            let _ = self.claim.progress(self.storage, &progress).await;
        })
    }
}

async fn extract(
    state: &AppState,
    storage: &Arc<Storage>,
    claim: &Claim,
) -> Result<(), sqlx::Error> {
    // The ends a fixed message tells.
    let fail = |code, why: &'static str| claim.settle(storage, code, why);
    let input = claim.input().cloned().map(serde_json::from_value::<Input>);
    let Some(Ok(input)) = input else {
        return fail("invalid_request", "the job's input is not an extraction").await;
    };
    // The key's schema hash is the fields' own (PR-Daemon, #883).
    if schema_sha256(&input.fields).as_ref() != Ok(&input.schema_sha256) {
        return fail(
            "invalid_request",
            "the job's schema hash is not its fields'",
        )
        .await;
    }
    let Some(model) = state.model().cloned() else {
        return fail(
            "engine_unavailable",
            "the kernel offers this module no model",
        )
        .await;
    };
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT content_sha256, media_type FROM revisions WHERE document_id = ? AND revision = ?",
    )
    .bind(&input.document_id)
    .bind(input.revision)
    .fetch_optional(storage.db.pool())
    .await?;
    let Some((content, media)) = row else {
        return fail("not_found", "no such document revision").await;
    };
    let job = Running {
        storage,
        claim,
        failed: std::sync::Mutex::new(None),
    };
    // The layer may take minutes to build; a cancel is seen within a second.
    // A build already started goes on: it is shared, and pinned for later reads.
    let layer = state.layers().layer(storage, &content, &media, LAYER_WAIT);
    tokio::pin!(layer);
    let address = loop {
        tokio::select! {
            got = &mut layer => break got,
            () = tokio::time::sleep(LAYER_POLL) => {
                if !job.running().await {
                    return job.failure().map_or(Ok(()), Err);
                }
            }
        }
    };
    let address = match address {
        Ok(address) => address,
        Err(f) => {
            let (code, why) = layer_failure(&f);
            return claim.settle(storage, code, &why).await;
        }
    };
    let layer = match load(storage, &address).await {
        Ok(layer) => layer,
        // Lost or broken bytes of a pinned layer (§6).
        Err(_) => {
            return fail("storage_unavailable", "the text layer could not be read").await;
        }
    };
    let src = Source {
        document_id: &input.document_id,
        revision: input.revision,
        content_sha256: &content,
        media_type: &media,
        text_layer_sha256: &address,
        layer: &layer,
    };
    let partial = layer.parse_status == ParseStatus::Partial;
    let done = run(
        &job,
        model.as_ref(),
        &src,
        &input.fields,
        partial,
        MAX_VALUE_BYTES,
    )
    .await;
    match done {
        Ok(out) => commit(storage, claim, &input, &address, out).await,
        // Being cancelled: the runner ends it cancelled.
        // Or the status could not be read: the runner settles it as storage
        // that failed.
        Err(Failure::Cancelled) => job.failure().map_or(Ok(()), Err),
        Err(Failure::Interrupted) => claim.interrupt(storage, "the kernel is stopping").await,
        Err(Failure::Engine(why)) => claim.settle(storage, "engine_unavailable", &why).await,
        Err(Failure::Unanswered(why)) => claim.settle(storage, "parse_failed", &why).await,
    }
}

/// `engine_unavailable` and the like for a layer that could not be had.
fn layer_failure(f: &LayerFailure) -> (&'static str, String) {
    let code = match f {
        LayerFailure::NoEngine | LayerFailure::Busy | LayerFailure::Pending => "engine_unavailable",
        LayerFailure::Engine(crate::engine::EngineError::Unsupported) => "unsupported_format",
        LayerFailure::Engine(_) => "parse_failed",
        LayerFailure::Storage(..) => "storage_unavailable",
    };
    (code, f.to_string())
}

/// The extraction and the job's success, in one transaction: an extraction
/// is never seen without the job that made it, nor the job without it.
async fn commit(
    storage: &Storage,
    claim: &Claim,
    input: &Input,
    address: &str,
    out: Extracted,
) -> Result<(), sqlx::Error> {
    let Ok(id) = new_id(IdKind::Extraction) else {
        return claim
            .settle(
                storage,
                "storage_unavailable",
                "no randomness for an extraction id",
            )
            .await;
    };
    let left_out: Vec<String> = input
        .fields
        .iter()
        .zip(&out.values)
        .filter(|(_, m)| m.dropped > 0)
        .map(|(f, m)| format!("{} ({} more)", f.key, m.dropped))
        .collect();
    let stage = if left_out.is_empty() {
        "extracted".to_owned()
    } else {
        format!(
            "extracted; values past the first 8 left out of {}",
            left_out.join(", ")
        )
    };
    let Some(mut tx) = claim.begin_commit(storage).await? else {
        return Ok(());
    };
    let extraction = NewExtraction {
        id: id.clone(),
        document_id: input.document_id.clone(),
        revision: input.revision,
        schema_sha256: input.schema_sha256.clone(),
        extractor_version: super::EXTRACTOR_VERSION.into(),
        model_id: out.model_id,
        text_layer_sha256: address.into(),
        values: out.values.into_iter().map(|m| m.value).collect(),
    };
    insert(tx.conn(), &extraction).await?;
    let progress: Value = json!({ "stage": stage, "done": 1, "total": 1, "unit": "extraction" });
    tx.succeed_with(
        Some(&input.document_id),
        Some(input.revision),
        Some(&progress),
        Some(&id),
    )
    .await
}

#[cfg(test)]
mod tests;
