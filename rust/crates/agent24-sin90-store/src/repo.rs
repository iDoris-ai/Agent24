//! Sin90 repository: direct entity writes + the CAS-idempotent Proposal apply.
//!
//! Every method that changes a status runs inside `BEGIN IMMEDIATE`, reads the
//! current status under the write lock, checks the `agent24-sin90` transition
//! matrix, updates, and appends a self-contained event — all in one tx.

use std::collections::HashMap;

use agent24_core::util::{now_iso8601, ulid};
use agent24_sin90::{
    Direction, DirectionStatus, RhythmStatus, ScheduleBlock, ScheduleBlockStatus, Sin90Op,
    Sin90Proposal, TaskStatus, ValidationCtx, Week, WeekStatus, check_rhythm_transition,
    check_schedule_block_transition, check_task_transition, validate,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use sqlx::{Row, Sqlite, Transaction};

use crate::{Result, Sin90Store, StoreError};

/// Receipt of a successful (or idempotently-replayed) proposal apply.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppliedProposal {
    pub proposal_id: String,
    /// Ids of events appended by this apply, in order.
    pub event_ids: Vec<String>,
}

// A unit-variant enum serializes to a JSON string; unwrap that to the wire text.
fn to_wire<T: Serialize>(v: &T) -> Result<String> {
    match serde_json::to_value(v)? {
        serde_json::Value::String(s) => Ok(s),
        other => Ok(other.to_string()),
    }
}

fn from_wire<T: DeserializeOwned>(s: &str) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(
        s.to_string(),
    ))?)
}

type Tx<'a> = Transaction<'a, Sqlite>;

// One low-level append; the fixed event-row shape is clearer as positional args
// than a throwaway builder struct.
#[allow(clippy::too_many_arguments)]
async fn append_event(
    tx: &mut Tx<'_>,
    entity: &str,
    entity_id: &str,
    kind: &str,
    from_state: Option<&str>,
    to_state: Option<&str>,
    payload: &serde_json::Value,
    at: &str,
) -> Result<String> {
    let id = ulid();
    sqlx::query(
        "INSERT INTO sin90_events (id, entity, entity_id, kind, from_state, to_state, payload, at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(entity)
    .bind(entity_id)
    .bind(kind)
    .bind(from_state)
    .bind(to_state)
    .bind(serde_json::to_string(payload)?)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

impl Sin90Store {
    // ----- direct entity creation (user/planner actions, not AI proposals) ---

    pub async fn create_direction(&self, title: &str, target_window: &str) -> Result<Direction> {
        let id = ulid();
        let now = now_iso8601();
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO sin90_directions (id, title, status, target_window, created_at, updated_at)
             VALUES (?, ?, 'draft', ?, ?, ?)",
        )
        .bind(&id)
        .bind(title)
        .bind(target_window)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        append_event(
            &mut tx,
            "direction",
            &id,
            "created",
            None,
            Some("draft"),
            &json!({"id": id, "title": title, "status": "draft", "target_window": target_window}),
            &now,
        )
        .await?;
        tx.commit().await?;
        Ok(Direction {
            id,
            title: title.to_string(),
            status: DirectionStatus::Draft,
            target_window: target_window.to_string(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub async fn create_week(&self, iso_week: &str) -> Result<Week> {
        let id = ulid();
        let now = now_iso8601();
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO sin90_weeks (id, status, iso_week, created_at, updated_at)
             VALUES (?, 'planning', ?, ?, ?)",
        )
        .bind(&id)
        .bind(iso_week)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        append_event(
            &mut tx,
            "week",
            &id,
            "created",
            None,
            Some("planning"),
            &json!({"id": id, "iso_week": iso_week, "status": "planning"}),
            &now,
        )
        .await?;
        tx.commit().await?;
        Ok(Week {
            id,
            status: WeekStatus::Planning,
            iso_week: iso_week.to_string(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub async fn create_block(
        &self,
        direction_id: Option<&str>,
        task_id: Option<&str>,
        planned_minutes: u32,
    ) -> Result<ScheduleBlock> {
        let id = ulid();
        let now = now_iso8601();
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO sin90_schedule_blocks
                 (id, direction_id, task_id, status, planned_minutes, created_at, updated_at)
             VALUES (?, ?, ?, 'planned', ?, ?, ?)",
        )
        .bind(&id)
        .bind(direction_id)
        .bind(task_id)
        .bind(planned_minutes as i64)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        append_event(
            &mut tx,
            "block",
            &id,
            "created",
            None,
            Some("planned"),
            &json!({"id": id, "direction_id": direction_id, "planned_minutes": planned_minutes}),
            &now,
        )
        .await?;
        tx.commit().await?;
        Ok(ScheduleBlock {
            id,
            direction_id: direction_id.map(str::to_string),
            task_id: task_id.map(str::to_string),
            status: ScheduleBlockStatus::Planned,
            planned_minutes,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Transition a block; a `completed` transition appends a SELF-CONTAINED
    /// event carrying the direction snapshot + minutes + occurred_at, so the
    /// attention replay never has to join the mutable blocks/directions tables.
    pub async fn transition_block(
        &self,
        id: &str,
        to: ScheduleBlockStatus,
    ) -> Result<ScheduleBlock> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT status, planned_minutes, direction_id, task_id, created_at
             FROM sin90_schedule_blocks WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Err(StoreError::NotFound(format!("schedule_block {id}")));
        };
        let from: ScheduleBlockStatus = from_wire(&row.get::<String, _>("status"))?;
        check_schedule_block_transition(from, to)?;

        let minutes: i64 = row.get("planned_minutes");
        let direction_id: Option<String> = row.get("direction_id");
        let task_id: Option<String> = row.get("task_id");
        let created_at: String = row.get("created_at");
        let now = now_iso8601();
        let to_wire_str = to_wire(&to)?;

        sqlx::query("UPDATE sin90_schedule_blocks SET status = ?, updated_at = ? WHERE id = ?")
            .bind(&to_wire_str)
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;

        // Snapshot the direction title INTO the event payload (self-contained).
        let direction_title: Option<String> = match &direction_id {
            Some(did) => sqlx::query("SELECT title FROM sin90_directions WHERE id = ?")
                .bind(did)
                .fetch_optional(&mut *tx)
                .await?
                .map(|r| r.get::<String, _>("title")),
            None => None,
        };
        append_event(
            &mut tx,
            "block",
            id,
            "transitioned",
            Some(&to_wire(&from)?),
            Some(&to_wire_str),
            &json!({
                "block_id": id,
                "direction_id": direction_id,
                "direction_title": direction_title,
                "minutes": minutes,
                "occurred_at": now,
            }),
            &now,
        )
        .await?;
        tx.commit().await?;
        Ok(ScheduleBlock {
            id: id.to_string(),
            direction_id,
            task_id,
            status: to,
            planned_minutes: minutes as u32,
            created_at,
            updated_at: now,
        })
    }

    // ----- proposal gate ------------------------------------------------------

    /// Persist a proposal as `pending`. Idempotent on `id` (re-submit is a no-op).
    pub async fn submit_proposal(&self, p: &Sin90Proposal) -> Result<()> {
        let now = now_iso8601();
        sqlx::query(
            "INSERT INTO sin90_proposals (id, status, source, ops, rationale, created_at)
             VALUES (?, 'pending', ?, ?, ?, ?)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(&p.id)
        .bind(to_wire(&p.source)?)
        .bind(serde_json::to_string(&p.ops)?)
        .bind(&p.rationale)
        .bind(&now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Accept + apply a pending proposal in ONE transaction, CAS-idempotent:
    /// `pending → applying` is a compare-and-set; a re-tried accept whose row is
    /// already `applied` returns the stored receipt without re-applying. A
    /// validation or apply error rolls the whole tx back (proposal → pending).
    pub async fn apply_proposal(&self, proposal_id: &str) -> Result<AppliedProposal> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;

        // CAS claim.
        let claimed = sqlx::query(
            "UPDATE sin90_proposals SET status = 'applying'
             WHERE id = ? AND status = 'pending'
             RETURNING ops, source",
        )
        .bind(proposal_id)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(claimed) = claimed else {
            // Not claimable — decide why (idempotent replay vs conflict vs missing).
            let existing = sqlx::query("SELECT status, result FROM sin90_proposals WHERE id = ?")
                .bind(proposal_id)
                .fetch_optional(&mut *tx)
                .await?;
            return match existing {
                None => Err(StoreError::NotFound(format!("proposal {proposal_id}"))),
                Some(r) => match r.get::<String, _>("status").as_str() {
                    "applied" => {
                        let result: String = r
                            .get::<Option<String>, _>("result")
                            .ok_or_else(|| StoreError::Conflict("applied without result".into()))?;
                        Ok(serde_json::from_str(&result)?)
                    }
                    "applying" => Err(StoreError::Conflict(format!(
                        "proposal {proposal_id} is already being applied"
                    ))),
                    other => Err(StoreError::Conflict(format!(
                        "proposal {proposal_id} is {other}, not pending"
                    ))),
                },
            };
        };

        let ops: Vec<Sin90Op> = serde_json::from_str(&claimed.get::<String, _>("ops"))?;
        let source = from_wire(&claimed.get::<String, _>("source"))?;

        // Pre-fetch referenced current states under the lock → pure validation.
        let snapshot = build_snapshot(&mut tx, &ops).await?;
        let proposal = Sin90Proposal {
            id: proposal_id.to_string(),
            status: agent24_sin90::ProposalStatus::Applying,
            source,
            ops: ops.clone(),
            rationale: None,
        };
        validate(&proposal, &snapshot)?; // Err → tx drops → rollback → back to pending

        let mut event_ids = Vec::new();
        for op in &ops {
            apply_op(&mut tx, op, &mut event_ids).await?;
        }

        let receipt = AppliedProposal {
            proposal_id: proposal_id.to_string(),
            event_ids,
        };
        sqlx::query(
            "UPDATE sin90_proposals SET status = 'applied', result = ?, decided_at = ? WHERE id = ?",
        )
        .bind(serde_json::to_string(&receipt)?)
        .bind(now_iso8601())
        .bind(proposal_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(receipt)
    }
}

/// A read-only snapshot of the entities a proposal references, so
/// `agent24_sin90::validate` stays pure (SIN90-domain.md §2.3).
#[derive(Default)]
struct DbSnapshot {
    tasks: HashMap<String, TaskStatus>,
    weeks: HashMap<String, WeekStatus>,
    rhythms_retired: HashMap<String, bool>,
}

impl ValidationCtx for DbSnapshot {
    fn task_status(&self, id: &str) -> Option<TaskStatus> {
        self.tasks.get(id).copied()
    }
    fn week_status(&self, id: &str) -> Option<WeekStatus> {
        self.weeks.get(id).copied()
    }
    fn rhythm_is_retired(&self, id: &str) -> Option<bool> {
        self.rhythms_retired.get(id).copied()
    }
}

async fn build_snapshot(tx: &mut Tx<'_>, ops: &[Sin90Op]) -> Result<DbSnapshot> {
    let mut snap = DbSnapshot::default();
    for op in ops {
        match op {
            Sin90Op::CreateDirection { .. } => {}
            Sin90Op::TransitionTask { task_id, .. } => load_task(tx, task_id, &mut snap).await?,
            Sin90Op::CreateTasks { week_id, .. } | Sin90Op::ReorderTasks { week_id, .. } => {
                load_week(tx, week_id, &mut snap).await?
            }
            Sin90Op::AdjustRhythm { rhythm_id, .. } => {
                load_rhythm(tx, rhythm_id, &mut snap).await?
            }
            Sin90Op::CarryOverTask { task_id, to_week } => {
                load_task(tx, task_id, &mut snap).await?;
                load_week(tx, to_week, &mut snap).await?;
            }
        }
    }
    Ok(snap)
}

async fn load_task(tx: &mut Tx<'_>, id: &str, snap: &mut DbSnapshot) -> Result<()> {
    if let Some(row) = sqlx::query("SELECT status FROM sin90_tasks WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
    {
        snap.tasks
            .insert(id.to_string(), from_wire(&row.get::<String, _>("status"))?);
    }
    Ok(())
}

async fn load_week(tx: &mut Tx<'_>, id: &str, snap: &mut DbSnapshot) -> Result<()> {
    if let Some(row) = sqlx::query("SELECT status FROM sin90_weeks WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
    {
        snap.weeks
            .insert(id.to_string(), from_wire(&row.get::<String, _>("status"))?);
    }
    Ok(())
}

async fn load_rhythm(tx: &mut Tx<'_>, id: &str, snap: &mut DbSnapshot) -> Result<()> {
    if let Some(row) = sqlx::query("SELECT status FROM sin90_rhythms WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
    {
        let status: RhythmStatus = from_wire(&row.get::<String, _>("status"))?;
        snap.rhythms_retired
            .insert(id.to_string(), status == RhythmStatus::Retired);
    }
    Ok(())
}

async fn apply_op(tx: &mut Tx<'_>, op: &Sin90Op, event_ids: &mut Vec<String>) -> Result<()> {
    let now = now_iso8601();
    match op {
        Sin90Op::CreateDirection {
            title,
            target_window,
        } => {
            let id = ulid();
            sqlx::query(
                "INSERT INTO sin90_directions (id, title, status, target_window, created_at, updated_at)
                 VALUES (?, ?, 'draft', ?, ?, ?)",
            )
            .bind(&id)
            .bind(title)
            .bind(target_window)
            .bind(&now)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
            let ev = append_event(
                tx,
                "direction",
                &id,
                "created",
                None,
                Some("draft"),
                &json!({"id": id, "title": title, "status": "draft", "target_window": target_window}),
                &now,
            )
            .await?;
            event_ids.push(ev);
        }

        Sin90Op::TransitionTask { task_id, to } => {
            let from = read_task_status(tx, task_id).await?;
            check_task_transition(from, *to)?;
            let to_str = to_wire(to)?;
            sqlx::query("UPDATE sin90_tasks SET status = ?, updated_at = ? WHERE id = ?")
                .bind(&to_str)
                .bind(&now)
                .bind(task_id)
                .execute(&mut **tx)
                .await?;
            let ev = append_event(
                tx,
                "task",
                task_id,
                "transitioned",
                Some(&to_wire(&from)?),
                Some(&to_str),
                &json!({"task_id": task_id}),
                &now,
            )
            .await?;
            event_ids.push(ev);
        }

        Sin90Op::CreateTasks { week_id, tasks } => {
            for (i, t) in tasks.iter().enumerate() {
                let id = ulid();
                sqlx::query(
                    "INSERT INTO sin90_tasks
                         (id, direction_id, week_id, title, status, kind, energy,
                          est_minutes, sort_key, carried_from, created_at, updated_at)
                     VALUES (?, ?, ?, ?, 'planned', 'other', 'mid', NULL, ?, NULL, ?, ?)",
                )
                .bind(&id)
                .bind(&t.direction_id)
                .bind(week_id)
                .bind(&t.title)
                .bind(i as i64)
                .bind(&now)
                .bind(&now)
                .execute(&mut **tx)
                .await?;
                let ev = append_event(
                    tx,
                    "task",
                    &id,
                    "created",
                    None,
                    Some("planned"),
                    &json!({"id": id, "week_id": week_id, "title": t.title}),
                    &now,
                )
                .await?;
                event_ids.push(ev);
            }
        }

        Sin90Op::ReorderTasks { week_id, order } => {
            for (i, tid) in order.iter().enumerate() {
                sqlx::query("UPDATE sin90_tasks SET sort_key = ?, updated_at = ? WHERE id = ? AND week_id = ?")
                    .bind(i as i64)
                    .bind(&now)
                    .bind(tid)
                    .bind(week_id)
                    .execute(&mut **tx)
                    .await?;
            }
            let ev = append_event(
                tx,
                "week",
                week_id,
                "reordered",
                None,
                None,
                &json!({"week_id": week_id, "order": order}),
                &now,
            )
            .await?;
            event_ids.push(ev);
        }

        Sin90Op::AdjustRhythm {
            rhythm_id,
            new_alloc,
        } => {
            let from = read_rhythm_status(tx, rhythm_id).await?;
            // Active → Adjusted is a real transition; re-adjusting an Adjusted
            // rhythm just updates allocations (stays Adjusted).
            if from == RhythmStatus::Active {
                check_rhythm_transition(from, RhythmStatus::Adjusted)?;
            }
            sqlx::query(
                "UPDATE sin90_rhythms SET status = 'adjusted', allocations = ?, updated_at = ? WHERE id = ?",
            )
            .bind(serde_json::to_string(new_alloc)?)
            .bind(&now)
            .bind(rhythm_id)
            .execute(&mut **tx)
            .await?;
            let ev = append_event(
                tx,
                "rhythm",
                rhythm_id,
                "adjusted",
                Some(&to_wire(&from)?),
                Some("adjusted"),
                &json!({"rhythm_id": rhythm_id, "allocations": new_alloc}),
                &now,
            )
            .await?;
            event_ids.push(ev);
        }

        Sin90Op::CarryOverTask { task_id, to_week } => {
            let from = read_task_status(tx, task_id).await?;
            check_task_transition(from, TaskStatus::CarriedOver)?;
            // Close the source task.
            sqlx::query(
                "UPDATE sin90_tasks SET status = 'carried_over', updated_at = ? WHERE id = ?",
            )
            .bind(&now)
            .bind(task_id)
            .execute(&mut **tx)
            .await?;
            let close_ev = append_event(
                tx,
                "task",
                task_id,
                "transitioned",
                Some(&to_wire(&from)?),
                Some("carried_over"),
                &json!({"task_id": task_id}),
                &now,
            )
            .await?;
            event_ids.push(close_ev);
            // Create the fresh next-week task, linked via carried_from.
            let src = sqlx::query("SELECT title, direction_id FROM sin90_tasks WHERE id = ?")
                .bind(task_id)
                .fetch_one(&mut **tx)
                .await?;
            let title: String = src.get("title");
            let direction_id: Option<String> = src.get("direction_id");
            let new_id = ulid();
            sqlx::query(
                "INSERT INTO sin90_tasks
                     (id, direction_id, week_id, title, status, kind, energy,
                      est_minutes, sort_key, carried_from, created_at, updated_at)
                 VALUES (?, ?, ?, ?, 'planned', 'other', 'mid', NULL, 0, ?, ?, ?)",
            )
            .bind(&new_id)
            .bind(&direction_id)
            .bind(to_week)
            .bind(&title)
            .bind(task_id)
            .bind(&now)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
            let create_ev = append_event(
                tx,
                "task",
                &new_id,
                "created",
                None,
                Some("planned"),
                &json!({"id": new_id, "week_id": to_week, "carried_from": task_id}),
                &now,
            )
            .await?;
            event_ids.push(create_ev);
        }
    }
    Ok(())
}

async fn read_task_status(tx: &mut Tx<'_>, id: &str) -> Result<TaskStatus> {
    let row = sqlx::query("SELECT status FROM sin90_tasks WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("task {id}")))?;
    from_wire(&row.get::<String, _>("status"))
}

async fn read_rhythm_status(tx: &mut Tx<'_>, id: &str) -> Result<RhythmStatus> {
    let row = sqlx::query("SELECT status FROM sin90_rhythms WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("rhythm {id}")))?;
    from_wire(&row.get::<String, _>("status"))
}
