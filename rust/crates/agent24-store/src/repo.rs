//! Repositories over the protocol types.
//!
//! Concurrency-safe status updates: run transitions execute in a single
//! BEGIN IMMEDIATE transaction (read-check-update under the write lock);
//! approval/tool-call finishers use single-statement WHERE guards whose
//! misses classify stably (their source states are absorbing).

use agent24_core::check_run_transition;
use agent24_protocol::{
    Approval, ApprovalStatus, Decision, ErrorBody, Run, RunOutput, RunStatus, Schedule, Session,
    ToolCall, ToolCallStatus, Usage,
};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, StoreError};

fn status_str(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::AwaitingApproval => "awaiting_approval",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

fn parse_status(s: &str) -> Result<RunStatus> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(StoreError::from)
}

fn approval_status_str(s: ApprovalStatus) -> &'static str {
    match s {
        ApprovalStatus::Pending => "pending",
        ApprovalStatus::Approved => "approved",
        ApprovalStatus::Denied => "denied",
        ApprovalStatus::Aborted => "aborted",
        ApprovalStatus::TimedOut => "timed_out",
    }
}

fn tool_status_str(s: ToolCallStatus) -> &'static str {
    match s {
        ToolCallStatus::Running => "running",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        ToolCallStatus::Denied => "denied",
    }
}

fn row_to_run(row: &SqliteRow) -> Result<Run> {
    Ok(Run {
        id: row.get("id"),
        session_id: row.get("session_id"),
        status: parse_status(&row.get::<String, _>("status"))?,
        input: serde_json::from_str(&row.get::<String, _>("input"))?,
        output: row
            .get::<Option<String>, _>("output")
            .map(|s| serde_json::from_str::<RunOutput>(&s))
            .transpose()?,
        error: row
            .get::<Option<String>, _>("error")
            .map(|s| serde_json::from_str::<ErrorBody>(&s))
            .transpose()?,
        usage: serde_json::from_str(&row.get::<String, _>("usage"))?,
        schedule_id: row.get("schedule_id"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        ended_at: row.get("ended_at"),
    })
}

/// Fields settable when a run reaches a new state.
#[derive(Debug, Default, Clone)]
pub struct RunPatch {
    pub output: Option<RunOutput>,
    pub error: Option<ErrorBody>,
    pub usage: Option<Usage>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

impl Store {
    // ── sessions ─────────────────────────────────────────────────────────────

    pub async fn insert_session(&self, session: &Session) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions (id, title, channel, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&session.id)
        .bind(&session.title)
        .bind(&session.channel)
        .bind(&session.created_at)
        .bind(&session.updated_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let row = sqlx::query("SELECT * FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.map(|r| Session {
            id: r.get("id"),
            title: r.get("title"),
            channel: r.get("channel"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        }))
    }

    pub async fn list_sessions(&self) -> Result<Vec<Session>> {
        let rows = sqlx::query("SELECT * FROM sessions ORDER BY created_at DESC")
            .fetch_all(self.pool())
            .await?;
        Ok(rows
            .iter()
            .map(|r| Session {
                id: r.get("id"),
                title: r.get("title"),
                channel: r.get("channel"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            })
            .collect())
    }

    // ── runs ─────────────────────────────────────────────────────────────────

    pub async fn insert_run(&self, run: &Run) -> Result<()> {
        sqlx::query(
            "INSERT INTO runs (id, session_id, status, input, output, error, usage,
                               schedule_id, created_at, started_at, ended_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&run.id)
        .bind(&run.session_id)
        .bind(status_str(run.status))
        .bind(serde_json::to_string(&run.input)?)
        .bind(run.output.as_ref().map(serde_json::to_string).transpose()?)
        .bind(run.error.as_ref().map(serde_json::to_string).transpose()?)
        .bind(serde_json::to_string(&run.usage)?)
        .bind(&run.schedule_id)
        .bind(&run.created_at)
        .bind(&run.started_at)
        .bind(&run.ended_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn get_run(&self, id: &str) -> Result<Option<Run>> {
        let row = sqlx::query("SELECT * FROM runs WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_run).transpose()
    }

    pub async fn list_runs(&self, status: Option<RunStatus>) -> Result<Vec<Run>> {
        let rows = match status {
            Some(s) => {
                sqlx::query("SELECT * FROM runs WHERE status = ? ORDER BY created_at DESC")
                    .bind(status_str(s))
                    .fetch_all(self.pool())
                    .await?
            }
            None => {
                sqlx::query("SELECT * FROM runs ORDER BY created_at DESC")
                    .fetch_all(self.pool())
                    .await?
            }
        };
        rows.iter().map(row_to_run).collect()
    }

    /// Transition a run's status, applying `patch` atomically inside one
    /// BEGIN IMMEDIATE transaction: current status is read under the write
    /// lock, checked against the core matrix (precise Transition errors, no
    /// TOCTOU), then updated — the returned Run is exactly this write's row.
    pub async fn transition_run(&self, id: &str, to: RunStatus, patch: RunPatch) -> Result<Run> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;

        let current = sqlx::query("SELECT status FROM runs WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.get::<String, _>("status"));
        let Some(current) = current else {
            return Err(StoreError::NotFound(format!("run {id}")));
        };
        check_run_transition(parse_status(&current)?, to)?;

        let row = sqlx::query(
            "UPDATE runs SET status = ?,
                 output    = COALESCE(?, output),
                 error     = COALESCE(?, error),
                 usage     = COALESCE(?, usage),
                 started_at = COALESCE(?, started_at),
                 ended_at   = COALESCE(?, ended_at)
             WHERE id = ?
             RETURNING *",
        )
        .bind(status_str(to))
        .bind(
            patch
                .output
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(
            patch
                .error
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(
            patch
                .usage
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(&patch.started_at)
        .bind(&patch.ended_at)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let run = row_to_run(&row)?;
        tx.commit().await?;
        Ok(run)
    }

    // ── tool calls ───────────────────────────────────────────────────────────

    pub async fn insert_tool_call(&self, call: &ToolCall) -> Result<()> {
        sqlx::query(
            "INSERT INTO tool_calls (id, run_id, tool, input, status, output_summary,
                                     started_at, ended_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&call.id)
        .bind(&call.run_id)
        .bind(&call.tool)
        .bind(serde_json::to_string(&call.input)?)
        .bind(tool_status_str(call.status))
        .bind(&call.output_summary)
        .bind(&call.started_at)
        .bind(&call.ended_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Finish a tool call (running → completed|failed|denied enforced in SQL).
    pub async fn finish_tool_call(
        &self,
        id: &str,
        to: ToolCallStatus,
        output_summary: Option<String>,
        ended_at: String,
    ) -> Result<()> {
        agent24_core::check_tool_call_transition(ToolCallStatus::Running, to)?;
        let result = sqlx::query(
            "UPDATE tool_calls SET status = ?, output_summary = ?, ended_at = ?
             WHERE id = ? AND status = 'running'",
        )
        .bind(tool_status_str(to))
        .bind(&output_summary)
        .bind(&ended_at)
        .bind(id)
        .execute(self.pool())
        .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::Conflict(format!(
                "tool call {id} is not running (missing or already finished)"
            )));
        }
        Ok(())
    }

    pub async fn list_tool_calls(&self, run_id: &str) -> Result<Vec<ToolCall>> {
        let rows = sqlx::query("SELECT * FROM tool_calls WHERE run_id = ? ORDER BY started_at ASC")
            .bind(run_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| {
                Ok(ToolCall {
                    id: r.get("id"),
                    run_id: r.get("run_id"),
                    tool: r.get("tool"),
                    input: serde_json::from_str(&r.get::<String, _>("input"))?,
                    status: serde_json::from_value(serde_json::Value::String(
                        r.get::<String, _>("status"),
                    ))?,
                    output_summary: r.get("output_summary"),
                    started_at: r.get("started_at"),
                    ended_at: r.get("ended_at"),
                })
            })
            .collect()
    }

    // ── approvals ────────────────────────────────────────────────────────────

    pub async fn insert_approval(&self, approval: &Approval) -> Result<()> {
        sqlx::query(
            "INSERT INTO approvals (id, run_id, tool_call_id, kind, summary, payload,
                                    available_decisions, status, decision, expires_at,
                                    created_at, decided_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&approval.id)
        .bind(&approval.run_id)
        .bind(&approval.tool_call_id)
        .bind(&approval.kind)
        .bind(&approval.summary)
        .bind(serde_json::to_string(&approval.payload)?)
        .bind(serde_json::to_string(&approval.available_decisions)?)
        .bind(approval_status_str(approval.status))
        .bind(
            approval
                .decision
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(&approval.expires_at)
        .bind(&approval.created_at)
        .bind(&approval.decided_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    fn row_to_approval(r: &SqliteRow) -> Result<Approval> {
        Ok(Approval {
            id: r.get("id"),
            run_id: r.get("run_id"),
            tool_call_id: r.get("tool_call_id"),
            kind: r.get("kind"),
            summary: r.get("summary"),
            payload: serde_json::from_str(&r.get::<String, _>("payload"))?,
            available_decisions: serde_json::from_str(&r.get::<String, _>("available_decisions"))?,
            status: serde_json::from_value(serde_json::Value::String(
                r.get::<String, _>("status"),
            ))?,
            decision: r
                .get::<Option<String>, _>("decision")
                .map(|s| serde_json::from_str::<Decision>(&s))
                .transpose()?,
            expires_at: r.get("expires_at"),
            created_at: r.get("created_at"),
            decided_at: r.get("decided_at"),
        })
    }

    pub async fn get_approval(&self, id: &str) -> Result<Option<Approval>> {
        let row = sqlx::query("SELECT * FROM approvals WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(Self::row_to_approval).transpose()
    }

    pub async fn list_approvals(&self, status: Option<ApprovalStatus>) -> Result<Vec<Approval>> {
        let rows = match status {
            Some(s) => {
                sqlx::query("SELECT * FROM approvals WHERE status = ? ORDER BY created_at ASC")
                    .bind(approval_status_str(s))
                    .fetch_all(self.pool())
                    .await?
            }
            None => {
                sqlx::query("SELECT * FROM approvals ORDER BY created_at ASC")
                    .fetch_all(self.pool())
                    .await?
            }
        };
        rows.iter().map(Self::row_to_approval).collect()
    }

    /// Resolve a pending approval exactly once (pending-only WHERE clause —
    /// the second resolver gets Conflict, giving the REST layer its 409).
    pub async fn resolve_approval(
        &self,
        id: &str,
        to: ApprovalStatus,
        decision: Option<&Decision>,
        decided_at: String,
    ) -> Result<Approval> {
        agent24_core::check_approval_transition(ApprovalStatus::Pending, to)?;
        let result = sqlx::query(
            "UPDATE approvals SET status = ?, decision = ?, decided_at = ?
             WHERE id = ? AND status = 'pending'",
        )
        .bind(approval_status_str(to))
        .bind(decision.map(serde_json::to_string).transpose()?)
        .bind(&decided_at)
        .bind(id)
        .execute(self.pool())
        .await?;
        if result.rows_affected() == 0 {
            return match self.get_approval(id).await? {
                None => Err(StoreError::NotFound(format!("approval {id}"))),
                Some(_) => Err(StoreError::Conflict(format!(
                    "approval {id} already resolved"
                ))),
            };
        }
        self.get_approval(id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("approval {id}")))
    }

    /// Fail-closed startup sweep: abort every approval left pending by a
    /// previous daemon process (TASKS C4 acceptance: kill + restart ⇒ aborted).
    pub async fn abort_lingering_approvals(&self, decided_at: &str) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE approvals SET status = 'aborted', decided_at = ?
             WHERE status = 'pending'",
        )
        .bind(decided_at)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    // ── schedules ────────────────────────────────────────────────────────────

    pub async fn upsert_schedule(&self, schedule: &Schedule) -> Result<()> {
        sqlx::query(
            "INSERT INTO schedules (id, name, enabled, spec, action, delivery,
                                    last_run_at, next_run_at, consecutive_failures)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name, enabled = excluded.enabled,
                 spec = excluded.spec, action = excluded.action,
                 delivery = excluded.delivery, last_run_at = excluded.last_run_at,
                 next_run_at = excluded.next_run_at,
                 consecutive_failures = excluded.consecutive_failures",
        )
        .bind(&schedule.id)
        .bind(&schedule.name)
        .bind(schedule.enabled)
        .bind(serde_json::to_string(&schedule.spec)?)
        .bind(serde_json::to_string(&schedule.action)?)
        .bind(serde_json::to_string(&schedule.delivery)?)
        .bind(&schedule.last_run_at)
        .bind(&schedule.next_run_at)
        .bind(schedule.consecutive_failures)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    fn row_to_schedule(r: &SqliteRow) -> Result<Schedule> {
        Ok(Schedule {
            id: r.get("id"),
            name: r.get("name"),
            enabled: r.get("enabled"),
            spec: serde_json::from_str(&r.get::<String, _>("spec"))?,
            action: serde_json::from_str(&r.get::<String, _>("action"))?,
            delivery: serde_json::from_str(&r.get::<String, _>("delivery"))?,
            last_run_at: r.get("last_run_at"),
            next_run_at: r.get("next_run_at"),
            consecutive_failures: r.get("consecutive_failures"),
        })
    }

    pub async fn get_schedule(&self, id: &str) -> Result<Option<Schedule>> {
        let row = sqlx::query("SELECT * FROM schedules WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(Self::row_to_schedule).transpose()
    }

    pub async fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let rows = sqlx::query("SELECT * FROM schedules ORDER BY name ASC")
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(Self::row_to_schedule).collect()
    }

    pub async fn delete_schedule(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM schedules WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn status_strings_roundtrip_through_serde() {
        // Guards the hand-maintained *_str tables against drifting from the
        // serde snake_case wire names (review C1 minor).
        for s in [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::AwaitingApproval,
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            assert_eq!(parse_status(status_str(s)).unwrap(), s);
        }
        for s in [
            ApprovalStatus::Pending,
            ApprovalStatus::Approved,
            ApprovalStatus::Denied,
            ApprovalStatus::Aborted,
            ApprovalStatus::TimedOut,
        ] {
            let parsed: ApprovalStatus = serde_json::from_value(serde_json::Value::String(
                approval_status_str(s).to_owned(),
            ))
            .unwrap();
            assert_eq!(parsed, s);
        }
        for s in [
            ToolCallStatus::Running,
            ToolCallStatus::Completed,
            ToolCallStatus::Failed,
            ToolCallStatus::Denied,
        ] {
            let parsed: ToolCallStatus =
                serde_json::from_value(serde_json::Value::String(tool_status_str(s).to_owned()))
                    .unwrap();
            assert_eq!(parsed, s);
        }
    }
}
