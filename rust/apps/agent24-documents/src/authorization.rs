//! The end of an operation's authorization (K1-06a-grant-path §2.5) stops
//! the work of that operation's calls accepted before it. What the end is
//! matched against is recorded here: the tool calls each job attempt was
//! accepted for. Attribution is by call, never by time.

use sqlx::SqliteConnection;

use crate::oplog::Run;

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

#[cfg(test)]
mod tests;
