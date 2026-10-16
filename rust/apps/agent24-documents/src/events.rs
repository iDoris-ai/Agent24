//! The documents module's WS events (ADR-DOC-02 §7; payloads are
//! `Documents*Event` in openapi.yaml). Sent through `_a24/events/emit`, the
//! kernel wraps each as a `type: "module"` event from `documents`. They are
//! hints: a client reads the job row for the truth, so an event lost to a
//! full queue or a refusal is dropped, never waited for.
//!
//! Slice 1 jobs have one stage and no progress to report, so only end states
//! are sent: `job.finished`, with `document.imported` for an import that made
//! its document. Payloads carry ids, status, counts and error codes only:
//! the stream reaches every WS client.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use crate::jobs::Job;

/// Sends one event; `Err` when the kernel refused it or did not answer.
pub type Emit = Arc<
    dyn Fn(
            &'static str,
            Map<String, Value>,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

/// Between two sends: at most 2 a second for the whole module (§7), under
/// the kernel's 5.
const GAP: Duration = Duration::from_millis(500);
/// A refused event is sent once more after this (§7: end states are
/// retried once).
const RETRY_AFTER: Duration = Duration::from_secs(1);
/// Events waiting to be sent; more than this are dropped.
const QUEUE: usize = 256;

type Queued = (&'static str, Map<String, Value>);

/// Where the OS's events go. The default sends nothing: a module without
/// the `events` grant, and tests that do not look.
#[derive(Clone, Default)]
pub struct Events(Option<mpsc::Sender<Queued>>);

impl Events {
    /// Sends through `emit`, one at a time and paced, from a task of its own.
    #[must_use]
    pub fn start(emit: Emit) -> Self {
        Self::start_with(emit, GAP, RETRY_AFTER)
    }

    pub(crate) fn start_with(emit: Emit, gap: Duration, retry_after: Duration) -> Self {
        let (tx, mut rx) = mpsc::channel::<Queued>(QUEUE);
        tokio::spawn(async move {
            while let Some((kind, payload)) = rx.recv().await {
                if let Err(e) = emit(kind, payload.clone()).await {
                    tracing::warn!(error = %e, kind, "documents: event refused, sending it once more");
                    tokio::time::sleep(retry_after).await;
                    if let Err(e) = emit(kind, payload).await {
                        tracing::warn!(error = %e, kind, "documents: event dropped");
                    }
                }
                tokio::time::sleep(gap).await;
            }
        });
        Self(Some(tx))
    }

    fn send(&self, kind: &'static str, payload: Value) {
        let (Some(tx), Value::Object(payload)) = (&self.0, payload) else {
            return;
        };
        if tx.try_send((kind, payload)).is_err() {
            tracing::warn!(kind, "documents: event queue full, event dropped");
        }
    }

    /// `job.finished` for a job in an end state (nothing for one still
    /// working), as the transaction that ended it left it.
    pub fn finished(&self, job: &Job) {
        if !matches!(
            job.status.as_str(),
            "succeeded" | "failed" | "cancelled" | "interrupted"
        ) {
            return;
        }
        self.ended(&Ended {
            job_id: &job.job_id,
            kind: &job.kind,
            status: &job.status,
            attempt: job.attempt,
            // The code only, never the message: messages may grow details.
            error_code: job
                .error
                .as_ref()
                .and_then(|e| e.get("code"))
                .and_then(Value::as_str),
            document: job.document_id.as_deref().zip(job.revision),
        });
    }

    /// `job.finished`, and `document.imported` for an import that succeeded.
    pub fn ended(&self, e: &Ended<'_>) {
        let mut finished = json!({
            "job_id": e.job_id,
            "kind": e.kind,
            "status": e.status,
            "attempt": e.attempt,
            "error_code": e.error_code,
        });
        if let Some((document_id, revision)) = e.document {
            finished["document_id"] = json!(document_id);
            finished["revision"] = json!(revision);
        }
        self.send("job.finished", finished);
        if let ("import", "succeeded", Some((document_id, 1))) = (e.kind, e.status, e.document) {
            self.send(
                "document.imported",
                json!({ "document_id": document_id, "revision": 1, "job_id": e.job_id }),
            );
        }
    }
}

/// One job's end, as `job.finished` tells it.
pub struct Ended<'a> {
    pub job_id: &'a str,
    pub kind: &'a str,
    pub status: &'a str,
    pub attempt: i64,
    pub error_code: Option<&'a str>,
    pub document: Option<(&'a str, i64)>,
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::Events;
    use crate::jobs::Job;

    pub(crate) type Seen = Arc<Mutex<Vec<(String, Value, Instant)>>>;

    /// Events that record what they send; `refuse` sends fail that many times.
    pub(crate) fn recorder(gap: Duration, refuse: usize) -> (Events, Seen) {
        let seen: Seen = Arc::default();
        let left = Arc::new(Mutex::new(refuse));
        let events = Events::start_with(
            Arc::new({
                let seen = seen.clone();
                move |kind, payload| {
                    seen.lock().unwrap().push((
                        kind.to_owned(),
                        Value::Object(payload),
                        Instant::now(),
                    ));
                    let mut left = left.lock().unwrap();
                    let refused = *left > 0;
                    *left = left.saturating_sub(1);
                    Box::pin(async move {
                        if refused {
                            Err("rate_limited".to_owned())
                        } else {
                            Ok(())
                        }
                    })
                }
            }),
            gap,
            Duration::from_millis(50),
        );
        (events, seen)
    }

    /// The first `n` events sent, once they are.
    pub(crate) async fn sent(seen: &Seen, n: usize) -> Vec<(String, Value)> {
        let start = Instant::now();
        while seen.lock().unwrap().len() < n {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{:?}",
                seen.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        seen.lock().unwrap()[..n]
            .iter()
            .map(|(k, v, _)| (k.clone(), v.clone()))
            .collect()
    }

    fn job(kind: &str, status: &str, error: Option<Value>, document: Option<(&str, i64)>) -> Job {
        Job {
            job_id: "job_01K75A0B1C2D3E4F5G6H7J8K9M".to_owned(),
            kind: kind.to_owned(),
            document_id: document.map(|d| d.0.to_owned()),
            revision: document.map(|d| d.1),
            status: status.to_owned(),
            attempt: 2,
            progress: None,
            error,
            result: None,
            created_at: "2026-10-10T00:00:00.000Z".to_owned(),
            updated_at: "2026-10-10T00:00:00.000Z".to_owned(),
        }
    }

    pub(crate) const JOB: &str = "job_01K75A0B1C2D3E4F5G6H7J8K9M";
    const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";

    #[tokio::test]
    async fn an_end_state_is_announced_with_ids_status_and_code_only() {
        let (events, seen) = recorder(Duration::ZERO, 0);
        events.finished(&job("import", "running", None, None));
        events.finished(&job(
            "import",
            "failed",
            Some(json!({ "code": "unsupported_format", "message": "/Users/x/a.pdf" })),
            None,
        ));
        events.finished(&job("import", "succeeded", None, Some((DOC, 1))));
        events.finished(&job("extract", "succeeded", None, Some((DOC, 3))));
        assert_eq!(
            sent(&seen, 4).await,
            [
                (
                    "job.finished".to_owned(),
                    json!({ "job_id": JOB, "kind": "import", "status": "failed", "attempt": 2, "error_code": "unsupported_format" })
                ),
                (
                    "job.finished".to_owned(),
                    json!({ "job_id": JOB, "kind": "import", "status": "succeeded", "attempt": 2, "error_code": null, "document_id": DOC, "revision": 1 })
                ),
                (
                    "document.imported".to_owned(),
                    json!({ "document_id": DOC, "revision": 1, "job_id": JOB })
                ),
                (
                    "job.finished".to_owned(),
                    json!({ "job_id": JOB, "kind": "extract", "status": "succeeded", "attempt": 2, "error_code": null, "document_id": DOC, "revision": 3 })
                ),
            ]
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(seen.lock().unwrap().len(), 4, "nothing for a running job");
    }

    #[tokio::test]
    async fn events_are_paced_and_a_refused_one_is_sent_once_more() {
        let (events, seen) = recorder(Duration::from_millis(100), 3);
        for (status, code) in [
            ("failed", "storage_unavailable"),
            ("cancelled", "cancelled"),
            ("interrupted", ""),
        ] {
            let error = (!code.is_empty()).then(|| json!({ "code": code, "message": "m" }));
            events.finished(&job("import", status, error, None));
        }
        // failed: refused twice, dropped; cancelled: refused, then sent; interrupted: sent.
        let sent = sent(&seen, 5).await;
        let statuses: Vec<_> = sent
            .iter()
            .map(|(_, v)| v["status"].as_str().unwrap())
            .collect();
        assert_eq!(
            statuses,
            ["failed", "failed", "cancelled", "cancelled", "interrupted"]
        );
        let at: Vec<_> = seen.lock().unwrap().iter().map(|e| e.2).collect();
        // One gap after each event, sent or dropped: never two within it.
        assert!(at[2] - at[1] >= Duration::from_millis(100));
        assert!(at[4] - at[3] >= Duration::from_millis(100));
    }
}
