//! The end of an operation's authorization (K1-06a-grant-path §2.5): the
//! kernel tells the module, on revocation or expiry, and waits for its answer
//! (10 s). The work of every call of that operation accepted before the
//! notice stops, queued or running: the job attempts its calls were accepted
//! for and the text layer builds they asked for. Attribution is by call,
//! never by time, and over-stopping is the safe side: a build a page shares
//! stops too. A 2xx says all of it reached a safe point; anything else is
//! recorded by the kernel as a stop it could not confirm. A call of the
//! operation still in flight answers `permission_denied`, not what it found.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use serde::Deserialize;
use sqlx::SqliteConnection;

use crate::engine::Requester;
use crate::jobs::{self, NOW};
use crate::oplog::Run;
use crate::state::{AppState, Storage};

/// What the whole module is in the notice.
pub const ALL: &str = "*";

/// How long the answer waits for the work to stop: inside the kernel's 10 s.
pub const STOP_WAIT: Duration = Duration::from_secs(8);
const POLL: Duration = Duration::from_millis(50);
/// Why a job its authorization's end cancelled was cancelled.
pub(crate) const ENDED: &str = "cancelled: its authorization ended";

/// A tool call accepting a job attempt: the operation, and the grant it was
/// made under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub operation: String,
    pub authorization_ref: Option<String>,
}

/// Records that `run`'s call, `accepted`, relies on the job's attempt, in the
/// caller's transaction (the claim's).
pub(crate) async fn record(
    conn: &mut SqliteConnection,
    job_id: &str,
    attempt: i64,
    run: &Run,
    accepted: &Accepted,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO job_acceptances (job_id, attempt, operation, authorization_ref, run_id, tool_call_id)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(job_id)
    .bind(attempt)
    .bind(&accepted.operation)
    .bind(&accepted.authorization_ref)
    .bind(&run.run_id)
    .bind(&run.tool_call_id)
    .execute(conn)
    .await
    .map(|_| ())
}

/// The tool calls in flight, and how many times each operation's
/// authorization, and the whole module's, has ended.
#[derive(Default)]
pub struct Calls(Mutex<Ledger>);

#[derive(Default)]
struct Ledger {
    ends: HashMap<String, u64>,
    all: u64,
    next: u64,
    /// Each call in flight: its operation, and the ends it began after.
    live: HashMap<u64, (String, u64)>,
}

impl Ledger {
    fn ended(&self, operation: &str) -> u64 {
        self.all + self.ends.get(operation).copied().unwrap_or(0)
    }
}

impl Calls {
    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A call of `operation` begins; it is in flight until the last clone
    /// of what this returns is dropped.
    #[must_use]
    pub fn begin(self: &Arc<Self>, operation: &str) -> Arc<Call> {
        let mut ledger = self.ledger();
        let (id, since) = (ledger.next, ledger.ended(operation));
        ledger.next += 1;
        ledger.live.insert(id, (operation.to_owned(), since));
        Arc::new(Call {
            calls: self.clone(),
            id,
        })
    }

    /// How many calls are in flight.
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> usize {
        self.ledger().live.len()
    }

    /// `operation`'s authorization (the whole module's: `*`) ends: every
    /// call of it in flight now began before the end.
    pub fn end(&self, operation: &str) {
        let mut ledger = self.ledger();
        if operation == ALL {
            ledger.all += 1;
        } else {
            *ledger.ends.entry(operation.to_owned()).or_default() += 1;
        }
    }

    /// How many calls of `operation` (any, for `*`) that began before one
    /// of its ends are still in flight.
    #[must_use]
    pub fn before_an_end(&self, operation: &str) -> usize {
        let ledger = self.ledger();
        ledger
            .live
            .values()
            .filter(|(op, since)| {
                (operation == ALL || op == operation) && *since != ledger.ended(op)
            })
            .count()
    }
}

/// A tool call in flight.
pub struct Call {
    calls: Arc<Calls>,
    id: u64,
}

impl Call {
    /// Whether its operation's authorization ended since it began: then
    /// what it found is not given.
    #[must_use]
    pub fn ended(&self) -> bool {
        let ledger = self.calls.ledger();
        ledger
            .live
            .get(&self.id)
            .is_some_and(|(op, since)| *since != ledger.ended(op))
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        self.calls.ledger().live.remove(&self.id);
    }
}

#[derive(Deserialize)]
pub struct Revoked {
    operation: String,
}

/// `POST /_a24/authorization/revoked` `{operation, revoked_at}`: 204 once the
/// work is stopped, 503 if it is not within [`STOP_WAIT`].
pub async fn revoked(
    State(state): State<AppState>,
    body: Result<Json<Revoked>, JsonRejection>,
) -> StatusCode {
    let Ok(Json(Revoked { operation })) = body else {
        return StatusCode::BAD_REQUEST;
    };
    if operation.is_empty() || operation.len() > 64 {
        return StatusCode::BAD_REQUEST;
    }
    if stop(&state, &operation, STOP_WAIT).await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Stops `operation`'s work and waits up to `wait` for it to stop; whether
/// it did. Everything, storage included, ends within `wait`.
pub(crate) async fn stop(state: &AppState, operation: &str, wait: Duration) -> bool {
    let (layers, calls) = (state.layers(), state.calls());
    // First: a call that finishes from now on gives nothing.
    calls.end(operation);
    let tool =
        |by: &Requester| matches!(by, Requester::Tool(op) if operation == ALL || op == operation);
    // These need no storage: stopped first, whatever follows.
    let mut builds = layers.stop(tool);
    let stopped = tokio::time::timeout(wait, async {
        let storage = state.storage().await.ok()?;
        let mut working = HashSet::new();
        loop {
            // Each round, the calls still in flight, then the jobs and the
            // builds: an attempt a call's claim commits until it ends, and a
            // build asked for until an attempt stops or by a call accepted
            // before the notice, are found.
            let calls_left = calls.before_an_end(operation);
            working.extend(
                cancel_jobs(&storage, operation)
                    .await
                    .map_err(|e| tracing::error!(error = %e, "documents: an ended authorization's jobs could not be cancelled"))
                    .ok()?,
            );
            let still = still_working(&storage, &working).await.ok()?;
            let accepted = accepted_of(&storage, operation, &layers.job_requesters())
                .await
                .ok()?;
            builds.extend(layers.stop(|by| {
                tool(by)
                    || matches!(by, Requester::Job { id, attempt } if accepted.contains(&(id.clone(), *attempt)))
            }));
            if calls_left == 0 && still == 0 && builds.iter().all(|b| b.borrow().is_some()) {
                return Some(());
            }
            tokio::time::sleep(POLL).await;
        }
    });
    matches!(stopped.await, Ok(Some(())))
}

/// Of the job attempts asking for builds, those `operation`'s calls were
/// accepted for, whichever attempt of the job they are: a build an earlier,
/// accepted attempt asked for may go on serving a later one, and stops.
async fn accepted_of(
    storage: &Storage,
    operation: &str,
    asking: &[(String, i64)],
) -> Result<HashSet<(String, i64)>, sqlx::Error> {
    let mut accepted = HashSet::new();
    for (id, attempt) in asking {
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM job_acceptances
             WHERE job_id = ?1 AND attempt = ?2 AND (?3 = '*' OR operation = ?3) LIMIT 1",
        )
        .bind(id)
        .bind(attempt)
        .bind(operation)
        .fetch_optional(storage.db.pool())
        .await?;
        if found.is_some() {
            accepted.insert((id.clone(), *attempt));
        }
    }
    Ok(accepted)
}

/// Under the write lock, so a claim commits wholly before or after it: the
/// jobs' current attempts `operation`'s calls were accepted for that still
/// work. Queued ones are cancelled at once; running ones are asked to stop
/// at their next safe point, and settle saying why (`job_revocations`).
async fn cancel_jobs(
    storage: &Storage,
    operation: &str,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let jobs: Vec<(String, i64)> = sqlx::query_as(
        "SELECT j.id, j.attempt FROM jobs j
         WHERE j.status IN ('queued', 'running', 'cancelling')
           AND EXISTS (SELECT 1 FROM job_acceptances a
                       WHERE a.job_id = j.id AND a.attempt = j.attempt
                         AND (?1 = '*' OR a.operation = ?1))",
    )
    .bind(operation)
    .fetch_all(&mut *tx)
    .await?;
    let mut ended = Vec::new();
    for (id, attempt) in &jobs {
        let done = sqlx::query(&format!(
            "UPDATE jobs SET updated_at = {NOW},
                    error = CASE WHEN status = 'queued'
                                 THEN json_object('code', 'cancelled', 'message', '{ENDED}')
                                 ELSE error END,
                    status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE 'cancelling' END
             WHERE id = ? AND attempt = ? AND status IN ('queued', 'running')"
        ))
        .bind(id)
        .bind(attempt)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() == 0 {
            continue;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO job_revocations (job_id, attempt)
             SELECT id, attempt FROM jobs WHERE id = ? AND attempt = ? AND status = 'cancelling'",
        )
        .bind(id)
        .bind(attempt)
        .execute(&mut *tx)
        .await?;
        ended.extend(
            jobs::load(&mut *tx, id)
                .await?
                .filter(|j| j.status == "cancelled"),
        );
    }
    tx.commit().await?;
    for job in &ended {
        storage.events.finished(job);
    }
    Ok(jobs)
}

/// How many of `jobs`' attempts are still working.
async fn still_working(
    storage: &Storage,
    jobs: &HashSet<(String, i64)>,
) -> Result<usize, sqlx::Error> {
    let mut n = 0;
    for (id, attempt) in jobs {
        let still: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM jobs WHERE id = ? AND attempt = ?
               AND status IN ('queued', 'running', 'cancelling')",
        )
        .bind(id)
        .bind(attempt)
        .fetch_optional(storage.db.pool())
        .await?;
        n += usize::from(still.is_some());
    }
    Ok(n)
}

#[cfg(test)]
mod tests;
