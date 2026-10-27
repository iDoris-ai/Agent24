//! The end of an operation's authorization (K1-06a-grant-path §2.5): the
//! kernel tells the module, on revocation or expiry, and waits for its answer
//! (10 s). The work of every call of that operation accepted before the
//! notice stops, queued or running: the job attempts its calls were accepted
//! for and the text layer builds they asked for. Attribution is by call,
//! never by time, and over-stopping is the safe side: a build a page shares
//! stops too. A 2xx says all of it reached a safe point; anything else is
//! recorded by the kernel as a stop it could not confirm. A call of the
//! operation still in flight answers `permission_denied`, not what it found.
//!
//! A notice naming the grants it ended (`authorization_refs`, §2.5.1) stops
//! only their work, whatever its `operation`: a new grant's work goes on.
//! The references are remembered, in memory, for as long as this process
//! runs: a call under one of them is refused, arriving or in flight. Work
//! under no reference is stopped by operation, as is all of it when the
//! notice names none.

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

impl Accepted {
    /// A call of `operation` under no reference.
    #[must_use]
    pub fn bare(operation: &str) -> Self {
        Self {
            operation: operation.to_owned(),
            authorization_ref: None,
        }
    }
}

/// An end of authorization: `operation`'s (the whole module's: `*`), and
/// the grants it ended, when the notice names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    pub operation: String,
    pub refs: Option<HashSet<String>>,
}

impl Ended {
    /// An end naming no grants: all of `operation`'s work stops.
    #[must_use]
    pub fn by_operation(operation: &str) -> Self {
        Self {
            operation: operation.to_owned(),
            refs: None,
        }
    }

    /// Whether work `accepted` stops: by its reference when both name one,
    /// otherwise by operation. Mirrored by [`COVERS`] for the stored rows.
    #[must_use]
    pub fn covers(&self, accepted: &Accepted) -> bool {
        match (&self.refs, &accepted.authorization_ref) {
            (Some(refs), Some(reference)) => refs.contains(reference),
            _ => self.operation == ALL || accepted.operation == self.operation,
        }
    }

    /// The references, as the JSON array [`COVERS`] binds; `None` if none.
    fn refs_json(&self) -> Option<String> {
        self.refs.as_ref().map(|refs| {
            serde_json::Value::from(refs.iter().cloned().collect::<Vec<_>>()).to_string()
        })
    }
}

/// [`Ended::covers`] for a `job_acceptances` row `a`: `?1` the operation,
/// `?2` the references as a JSON array, or NULL.
const COVERS: &str = "CASE WHEN ?2 IS NULL OR a.authorization_ref IS NULL
                           THEN ?1 = '*' OR a.operation = ?1
                           ELSE a.authorization_ref IN (SELECT value FROM json_each(?2)) END";

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

/// The tool calls in flight, how many times each operation's
/// authorization, and the whole module's, has ended, and the grants ended.
#[derive(Default)]
pub struct Calls(Mutex<Ledger>);

#[derive(Default)]
struct Ledger {
    /// Ends of an operation (`*`: `all`) that stop all of its calls...
    ends: HashMap<String, u64>,
    all: u64,
    /// ...and those that stop only its calls under no reference.
    bare_ends: HashMap<String, u64>,
    bare_all: u64,
    /// The grants ended: memory only, this process's (K1-06a §2.5.1).
    ended_refs: HashSet<String>,
    next: u64,
    /// Each call in flight: what it was accepted as, and the ends that
    /// count for it when it began.
    live: HashMap<u64, (Accepted, u64)>,
}

impl Ledger {
    /// The ends that count for a call `accepted` as.
    fn ends_of(&self, accepted: &Accepted) -> u64 {
        let op = &accepted.operation;
        let mut n = self.all + self.ends.get(op).copied().unwrap_or(0);
        if accepted.authorization_ref.is_none() {
            n += self.bare_all + self.bare_ends.get(op).copied().unwrap_or(0);
        }
        n
    }

    /// Whether a call `accepted` as, counting `since` ends when it began,
    /// has had its authorization end.
    fn ended(&self, (accepted, since): &(Accepted, u64)) -> bool {
        *since != self.ends_of(accepted)
            || accepted
                .authorization_ref
                .as_ref()
                .is_some_and(|r| self.ended_refs.contains(r))
    }
}

impl Calls {
    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A call `accepted` as begins; it is in flight until what this
    /// returns is finished or dropped. Under a grant already ended, it is
    /// [`Call::ended`] from the start.
    #[must_use]
    pub fn begin(self: &Arc<Self>, accepted: Accepted) -> Call {
        let mut ledger = self.ledger();
        let since = ledger.ends_of(&accepted);
        self.register(&mut ledger, accepted, since)
    }

    fn register(self: &Arc<Self>, ledger: &mut Ledger, accepted: Accepted, since: u64) -> Call {
        let id = ledger.next;
        ledger.next += 1;
        ledger.live.insert(id, (accepted, since));
        Call {
            calls: self.clone(),
            id,
        }
    }

    /// How many calls are in flight.
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> usize {
        self.ledger().live.len()
    }

    /// `ended` ends: every call it covers in flight now began before it,
    /// and a call under one of its grants is refused from now on.
    pub fn end(&self, ended: &Ended) {
        let mut ledger = self.ledger();
        let ledger = &mut *ledger;
        let (ends, all) = match &ended.refs {
            None => (&mut ledger.ends, &mut ledger.all),
            Some(refs) => {
                ledger.ended_refs.extend(refs.iter().cloned());
                (&mut ledger.bare_ends, &mut ledger.bare_all)
            }
        };
        if ended.operation == ALL {
            *all += 1;
        } else {
            *ends.entry(ended.operation.clone()).or_default() += 1;
        }
    }

    /// How many calls `ended` covers that began before an end of theirs
    /// are still in flight.
    #[must_use]
    pub fn before_an_end(&self, ended: &Ended) -> usize {
        let ledger = self.ledger();
        ledger
            .live
            .values()
            .filter(|live| ended.covers(&live.0) && ledger.ended(live))
            .count()
    }
}

/// A tool call in flight.
pub struct Call {
    calls: Arc<Calls>,
    id: u64,
}

impl Call {
    /// Whether its authorization ended since it began, or before, under
    /// a grant ended: then what it found is not given.
    #[must_use]
    pub fn ended(&self) -> bool {
        let ledger = self.calls.ledger();
        ledger
            .live
            .get(&self.id)
            .is_some_and(|live| ledger.ended(live))
    }

    /// The call's answer is decided: whether its authorization ended
    /// since it began, decided as it stops being in flight, under one lock.
    /// An end after this does not count it, as if it answered before the
    /// end; an end before it does, and it gives nothing.
    #[must_use]
    pub fn finish(self) -> bool {
        let mut ledger = self.calls.ledger();
        let live = ledger.live.remove(&self.id);
        let ended = live.is_some_and(|live| ledger.ended(&live));
        drop(ledger);
        ended
    }

    /// Another hold on the call's place in flight, for work it leaves
    /// running (`extract`'s claim): the end waits for it too, and it counts
    /// as begun when the call did.
    #[must_use]
    pub fn hold(&self) -> Call {
        let mut ledger = self.calls.ledger();
        // Always there: only `finish`, which takes the call, and its drop
        // remove it.
        let (accepted, since) = ledger
            .live
            .get(&self.id)
            .cloned()
            .unwrap_or_else(|| (Accepted::bare(""), 0));
        self.calls.register(&mut ledger, accepted, since)
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
    /// Absent (or `null`) is not empty: absent stops by operation, empty
    /// stops no grant's work (K1-06a §2.5.1, #1202 G2).
    #[serde(default)]
    authorization_refs: Option<Vec<String>>,
}

/// The longest grant reference taken (`ga1:` and a digest are far shorter).
const REF_MAX: usize = 256;

/// `POST /_a24/authorization/revoked` `{operation, revoked_at, reason,
/// authorization_refs}`: 204 once the work is stopped, 503 if it is not
/// within [`STOP_WAIT`].
pub async fn revoked(
    State(state): State<AppState>,
    body: Result<Json<Revoked>, JsonRejection>,
) -> StatusCode {
    let Ok(Json(Revoked {
        operation,
        authorization_refs,
    })) = body
    else {
        return StatusCode::BAD_REQUEST;
    };
    if operation.is_empty() || operation.len() > 64 {
        return StatusCode::BAD_REQUEST;
    }
    if authorization_refs
        .iter()
        .flatten()
        .any(|r| r.is_empty() || r.len() > REF_MAX)
    {
        return StatusCode::BAD_REQUEST;
    }
    let ended = Ended {
        operation,
        refs: authorization_refs.map(HashSet::from_iter),
    };
    if stop(&state, &ended, STOP_WAIT).await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Stops the work `ended` covers and waits up to `wait` for it to stop;
/// whether it did. Everything, storage included, ends within `wait`.
pub(crate) async fn stop(state: &AppState, ended: &Ended, wait: Duration) -> bool {
    let (layers, calls) = (state.layers(), state.calls());
    // First: a call that finishes from now on gives nothing.
    calls.end(ended);
    let tool = |by: &Requester| matches!(by, Requester::Tool(accepted) if ended.covers(accepted));
    // These need no storage: stopped first, whatever follows.
    let mut builds = layers.stop(tool);
    let stopped = tokio::time::timeout(wait, async {
        let storage = state.storage().await.ok()?;
        let mut working = HashSet::new();
        let mut swept = false;
        loop {
            // Each round, the calls still in flight, then the jobs and the
            // builds: an attempt a call's claim commits until it ends, and a
            // build asked for until an attempt stops or by a call accepted
            // before the notice, are found. Once a sweep began with no such
            // call left, every claim of theirs was in it: the write lock is
            // not taken again, and what is accepted after is not this
            // notice's.
            let calls_left = calls.before_an_end(ended);
            if !swept {
                working.extend(
                    cancel_jobs(&storage, ended)
                        .await
                        .map_err(|e| tracing::error!(error = %e, "documents: an ended authorization's jobs could not be cancelled"))
                        .ok()?,
                );
                swept = calls_left == 0;
            }
            let still = still_working(&storage, &working).await.ok()?;
            let accepted = accepted_of(&storage, ended, &layers.job_requesters())
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
/// The rows are read as a set (#1201 G2): repeated rows of one attempt
/// count once, and any one `ended` covers ([`COVERS`]) stops the attempt.
async fn accepted_of(
    storage: &Storage,
    ended: &Ended,
    asking: &[(String, i64)],
) -> Result<HashSet<(String, i64)>, sqlx::Error> {
    let mut accepted = HashSet::new();
    let refs = ended.refs_json();
    for (id, attempt) in asking {
        let found: Option<i64> = sqlx::query_scalar(&format!(
            "SELECT 1 FROM job_acceptances a
             WHERE a.job_id = ?3 AND a.attempt = ?4 AND ({COVERS}) LIMIT 1"
        ))
        .bind(&ended.operation)
        .bind(&refs)
        .bind(id)
        .bind(attempt)
        .fetch_optional(storage.db.pool())
        .await?;
        if found.is_some() {
            accepted.insert((id.clone(), *attempt));
        }
    }
    Ok(accepted)
}

/// Under the write lock, so a claim commits wholly before or after it: the
/// jobs' current attempts accepted by calls `ended` covers that still
/// work. Queued ones are cancelled at once; running ones are asked to stop
/// at their next safe point, and settle saying why (`job_revocations`).
async fn cancel_jobs(storage: &Storage, ended: &Ended) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let jobs: Vec<(String, i64)> = sqlx::query_as(&format!(
        "SELECT j.id, j.attempt FROM jobs j
         WHERE j.status IN ('queued', 'running', 'cancelling')
           AND EXISTS (SELECT 1 FROM job_acceptances a
                       WHERE a.job_id = j.id AND a.attempt = j.attempt
                         AND ({COVERS}))"
    ))
    .bind(&ended.operation)
    .bind(ended.refs_json())
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
