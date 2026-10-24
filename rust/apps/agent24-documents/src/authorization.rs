//! The end of an operation's authorization (K1-06a-grant-path §2.5) stops
//! the work of that operation's calls accepted before it. What the end is
//! matched against is recorded here: the tool calls each job attempt was
//! accepted for, and the tool calls in flight: one the end overtakes
//! answers `permission_denied`, not what it found. Attribution is by call,
//! never by time.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use sqlx::SqliteConnection;

use crate::oplog::Run;

/// What the whole module is in the notice.
pub const ALL: &str = "*";

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

#[cfg(test)]
mod tests;
