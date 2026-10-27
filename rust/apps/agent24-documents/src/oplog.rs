//! Who started an operation (ADR-DOC-01 D9, DOC-1-06b §2.4): the page or
//! an agent run. A job keeps it as JSON in `jobs.origin`; the append-only
//! `oplog` keeps it as `origin` ('page' or 'run') with the run's
//! `run_id` and `tool_call_id` in columns of their own.

use serde_json::{Value, json};
use sqlx::SqliteConnection;

/// The run and the tool call in it that asked for an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub run_id: String,
    pub tool_call_id: String,
}

/// Where a request came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Page,
    Run(Run),
}

impl Origin {
    /// `jobs.origin`: `{"kind":"page"}` or
    /// `{"kind":"run","run_id":…,"tool_call_id":…}` (0001_init.sql).
    #[must_use]
    pub fn job_json(&self) -> String {
        match self {
            Origin::Page => json!({ "kind": "page" }),
            Origin::Run(run) => json!({
                "kind": "run",
                "run_id": run.run_id,
                "tool_call_id": run.tool_call_id,
            }),
        }
        .to_string()
    }
}

/// Appends one `oplog` row on `conn` — in the caller's transaction, if it
/// has one.
///
/// # Errors
/// The database's, including the table's CHECK (a run needs both ids).
pub async fn append(
    conn: &mut SqliteConnection,
    op: &str,
    document_id: &str,
    revision: i64,
    origin: &Origin,
    details: &Value,
) -> Result<(), sqlx::Error> {
    let (kind, run_id, tool_call_id) = match origin {
        Origin::Page => ("page", None, None),
        Origin::Run(run) => ("run", Some(&run.run_id), Some(&run.tool_call_id)),
    };
    sqlx::query(
        "INSERT INTO oplog (op, document_id, revision, origin, run_id, tool_call_id, details)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(op)
    .bind(document_id)
    .bind(revision)
    .bind(kind)
    .bind(run_id)
    .bind(tool_call_id)
    .bind(details.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// The row for an extract a run submitted, new or replayed (D9): the page
/// writes none, its job already records it.
///
/// # Errors
/// As [`append`].
pub async fn extract_submitted(
    conn: &mut SqliteConnection,
    run: &Run,
    document_id: &str,
    revision: i64,
    job_id: &str,
) -> Result<(), sqlx::Error> {
    let origin = Origin::Run(run.clone());
    let details = json!({ "job_id": job_id });
    append(conn, "extract", document_id, revision, &origin, &details).await
}

#[cfg(test)]
mod tests;
