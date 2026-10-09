//! Running one attempt of a job (ADR-DOC-02 §7), whatever its kind: claim
//! it, do the kind's work, and make sure the attempt ends settled.
//!
//! A supervisor owns the outcome. However the work ends — with its result
//! committed, with an error, a panic, or returning early — the supervisor
//! settles whatever this worker claimed and left unfinished (failed, or
//! cancelled if cancelling), retrying until the database answers, so no job
//! is left queued or running without a worker. Everything is fenced by the
//! attempt, and settling by whether this worker claimed the job: a
//! supervisor that wakes late cannot touch a later attempt, nor a claim
//! another worker of the same attempt made. Success goes through
//! [`Claim::begin_commit`] and [`Commit::succeed`], fenced the same way.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use sqlx::{Sqlite, SqliteConnection, Transaction};

use crate::state::Storage;

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

/// Which rows of its attempt a worker may settle.
#[derive(Debug, Clone, Copy)]
pub enum Mine {
    /// It never claimed the job: only the still-queued job.
    Unclaimed,
    /// It claimed the job: only that claim, running or being cancelled.
    Claimed,
}

/// One attempt of one job, as a worker holds it.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub job_id: String,
    pub attempt: i64,
    pub mine: Mine,
}

/// A claimed attempt, handed to the kind's work with the job's input.
#[derive(Debug, Clone)]
pub struct Claim {
    pub attempt: Attempt,
    pub input: Option<Value>,
}

/// The transaction that ends a running attempt with its result.
pub struct Commit {
    tx: Transaction<'static, Sqlite>,
    attempt: Attempt,
}

/// Runs attempt `attempt` of `job_id` in the background: claims it (only
/// while queued at that attempt), then runs `work` on the claim.
pub fn spawn<W, F>(storage: Arc<Storage>, job_id: String, attempt: i64, work: W)
where
    W: FnOnce(Arc<Storage>, Claim) -> F + Send + 'static,
    F: Future<Output = Result<(), sqlx::Error>> + Send + 'static,
{
    tokio::spawn(async move {
        let claimed = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn({
            let (storage, job_id, claimed) = (storage.clone(), job_id.clone(), claimed.clone());
            async move {
                let Some(claim) = claim(&storage, &job_id, attempt, &claimed).await? else {
                    return Ok(false);
                };
                work(storage, claim).await.map(|()| true)
            }
        });
        let why = match task.await {
            // Nothing claimed: nothing of ours to settle.
            Ok(Ok(false)) => {
                #[cfg(test)]
                tests::worker_done(&job_id);
                return;
            }
            Ok(Ok(true)) => "the job ended without a result",
            Ok(Err(e)) => {
                tracing::error!(error = %e, job = job_id, "documents: job stopped on the database");
                "the database failed"
            }
            Err(e) => {
                tracing::error!(error = %e, job = job_id, "documents: job panicked");
                "the job stopped unexpectedly"
            }
        };
        let mine = if claimed.load(Ordering::SeqCst) {
            Mine::Claimed
        } else {
            Mine::Unclaimed
        };
        // Settling an attempt that already ended (its result committed) is
        // a no-op.
        let me = Attempt {
            job_id,
            attempt,
            mine,
        };
        let mut wait = Duration::from_millis(100);
        while let Err(e) = settle(&storage, &me, "storage_unavailable", why).await {
            tracing::error!(error = %e, job = me.job_id, "documents: cannot settle a job yet");
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(Duration::from_secs(60));
        }
        #[cfg(test)]
        tests::worker_done(&me.job_id);
    });
}

/// Queued → running for this attempt, in an explicit transaction so that a
/// failed commit is an error here, not a claim that silently did not happen.
async fn claim(
    storage: &Storage,
    job_id: &str,
    attempt: i64,
    claimed: &AtomicBool,
) -> Result<Option<Claim>, sqlx::Error> {
    #[cfg(test)]
    tests::before_start(job_id).await;
    #[cfg(test)]
    tests::fault(job_id, "claim")?;
    let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let input: Option<Option<String>> = sqlx::query_scalar(
        "SELECT input FROM jobs WHERE id = ? AND attempt = ? AND status = 'queued'",
    )
    .bind(job_id)
    .bind(attempt)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(input) = input else {
        tx.rollback().await?;
        return Ok(None);
    };
    sqlx::query(&format!(
        "UPDATE jobs SET status = 'running', updated_at = {NOW} WHERE id = ?"
    ))
    .bind(job_id)
    .execute(&mut *tx)
    .await?;
    #[cfg(test)]
    tests::before_claim_commit(&mut tx, job_id).await;
    tx.commit().await?;
    claimed.store(true, Ordering::SeqCst);
    Ok(Some(Claim {
        attempt: Attempt {
            job_id: job_id.to_owned(),
            attempt,
            mine: Mine::Claimed,
        },
        input: input.and_then(|s| serde_json::from_str(&s).ok()),
    }))
}

impl Claim {
    /// Takes the write lock to end this attempt with a result. `None` if the
    /// attempt is no longer running — being cancelled, ended or superseded —
    /// and nothing is to be committed; once the work returns, the supervisor
    /// settles what is left (a cancelling attempt ends cancelled).
    pub async fn begin_commit(&self, storage: &Storage) -> Result<Option<Commit>, sqlx::Error> {
        let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM jobs WHERE id = ? AND attempt = ?")
                .bind(&self.attempt.job_id)
                .bind(self.attempt.attempt)
                .fetch_optional(&mut *tx)
                .await?;
        match status.as_deref() {
            Some("running") => Ok(Some(Commit {
                tx,
                attempt: self.attempt.clone(),
            })),
            _ => {
                tx.rollback().await?;
                Ok(None)
            }
        }
    }
}

impl Commit {
    /// The transaction, for the kind's result rows.
    pub fn conn(&mut self) -> &mut SqliteConnection {
        &mut self.tx
    }

    /// Marks the attempt succeeded, with what it produced, and commits it
    /// with the result rows written through [`Commit::conn`].
    pub async fn succeed(
        mut self,
        document_id: Option<&str>,
        revision: Option<i64>,
        progress: Option<&Value>,
    ) -> Result<(), sqlx::Error> {
        let done = sqlx::query(&format!(
            "UPDATE jobs SET status = 'succeeded', document_id = ?1, revision = ?2,
                    progress = ?3, updated_at = {NOW}
             WHERE id = ?4 AND attempt = ?5 AND status = 'running'"
        ))
        .bind(document_id)
        .bind(revision)
        .bind(progress.map(Value::to_string))
        .bind(&self.attempt.job_id)
        .bind(self.attempt.attempt)
        .execute(&mut *self.tx)
        .await?
        .rows_affected();
        if done == 1 {
            self.tx.commit().await
        } else {
            // Cannot happen under the lock taken in begin_commit; never commit
            // result rows without the success that owns them.
            self.tx.rollback().await
        }
    }
}

/// Ends an unfinished attempt: failed with `code` (from the closed §6 set),
/// or cancelled if it was being cancelled.
pub async fn settle(
    storage: &Storage,
    me: &Attempt,
    code: &str,
    message: &str,
) -> Result<(), sqlx::Error> {
    #[cfg(test)]
    tests::fault(&me.job_id, "settle")?;
    let states = match me.mine {
        Mine::Unclaimed => "('queued')",
        Mine::Claimed => "('running', 'cancelling')",
    };
    sqlx::query(&format!(
        "UPDATE jobs SET updated_at = {NOW},
                status = CASE status WHEN 'cancelling' THEN 'cancelled' ELSE 'failed' END,
                error = CASE status WHEN 'cancelling'
                        THEN json_object('code', 'cancelled', 'message', 'cancelled by the user')
                        ELSE json_object('code', ?1, 'message', ?2) END
         WHERE id = ?3 AND attempt = ?4 AND status IN {states}"
    ))
    .bind(code)
    .bind(message)
    .bind(&me.job_id)
    .bind(me.attempt)
    .execute(storage.db.pool())
    .await?;
    #[cfg(test)]
    tests::settle_done(&me.job_id);
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
