//! ME4-S3 §2.4/§3.5 — the five typed clients (Events/Memory/Approval/
//! Scheduler/Model) and the machinery shared by all of them: [`Core`] (the
//! "no `Offer`, no client" construction rule, §2.4's common rules) and
//! [`set_opt`] ("omit, don't null" — §1.2 row 10).

mod approval;
mod events;
mod memory;
mod model;
mod scheduler;

pub use approval::{
    ApprovalAnswer, ApprovalClient, ApprovalDecision, ApprovalKind, ApprovalSubmit,
};
pub use events::{EventSink, EventSinkConfig, EventsClient};
pub use memory::{
    DEDUP_KEY_FIELD, MemoryClient, RECALL_PRECHECK_MAX_PAGES, RECALL_PRECHECK_PAGE_SIZE,
    RecallPage, Recollection, RememberOnce, Remembered,
};
pub use model::{
    CompleteRequest, CompleteResult, Complexity, JsonSchemaFormat, MODEL_RESPONSE_TIMEOUT,
    ModelClient, ModelMessage, ModelRole, ServedTier, Usage,
};
pub use scheduler::{
    DeleteOutcome, DeleteResult, LastFire, LastFires, ListResult, ScheduleSpec, ScheduleState,
    SchedulerClient, UpsertOutcome, UpsertRequest, UpsertResult,
};

use std::sync::Arc;
use std::time::Duration;

use agent24_os_proto::module::{CallOptions, Connection};
use serde_json::{Map, Value};

use crate::error::ClientError;

/// Every `_a24/*` method the SDK can send, per client (J-S15's source: the
/// wire-doc-alignment judgement scans this shape).
pub const METHODS: [&[&str]; 5] = [
    &events::METHODS,
    &memory::METHODS,
    &approval::METHODS,
    &scheduler::METHODS,
    &model::METHODS,
];

/// Shared by every client: "no `Offer` prefix, no client" (§2.4 — there is
/// no "call it anyway, it always fails" path), plus the one place a call is
/// actually dispatched. Cheap to clone (one `Arc` bump) — `EventsClient`
/// uses that to hand a copy to each of its sink's background workers.
#[derive(Clone)]
struct Core {
    conn: Arc<Connection>,
}

impl Core {
    /// `None` when `conn`'s `Offer` does not cover `prefix`.
    fn new(conn: &Arc<Connection>, prefix: &str) -> Option<Self> {
        conn.offer().provides(prefix).then(|| Self {
            conn: Arc::clone(conn),
        })
    }

    async fn call(&self, method: &'static str, params: Value) -> Result<Value, ClientError> {
        self.conn
            .call(method, params, CallOptions::default())
            .await
            .map_err(ClientError::from)
    }

    async fn call_with_timeout(
        &self,
        method: &'static str,
        params: Value,
        response_timeout: Duration,
    ) -> Result<Value, ClientError> {
        let opts = CallOptions {
            response_timeout,
            slot_wait: None,
        };
        self.conn
            .call(method, params, opts)
            .await
            .map_err(ClientError::from)
    }
}

/// "Omit, don't null" (§1.2 row 10): insert `key` only when `value` is
/// `Some` — a module sending an explicit `null` for an absent-means-default
/// field (e.g. scheduler's `enabled`) is a real wire difference from not
/// sending the field at all.
fn set_opt(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(v) = value {
        map.insert(key.to_owned(), v);
    }
}

fn malformed_result(what: &str, e: serde_json::Error) -> ClientError {
    ClientError::Other(format!("malformed {what} from the kernel: {e}"))
}
