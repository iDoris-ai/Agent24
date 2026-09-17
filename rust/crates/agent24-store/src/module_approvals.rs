//! T7b/ME-3e: module (gate/advise) approvals persistence. See
//! `docs/design/T7b-ME3e-approvals.md` decisions 3/5/7 and migration
//! `0005_module_approvals.sql`.

use agent24_protocol::{ModuleApproval, ModuleApprovalDecision, ModuleApprovalKind};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, StoreError};

fn kind_str(k: ModuleApprovalKind) -> &'static str {
    match k {
        ModuleApprovalKind::Gate => "gate",
        ModuleApprovalKind::Advise => "advise",
    }
}

fn parse_kind(s: &str) -> Result<ModuleApprovalKind> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(StoreError::from)
}

fn decision_str(d: ModuleApprovalDecision) -> &'static str {
    match d {
        ModuleApprovalDecision::Pending => "pending",
        ModuleApprovalDecision::Approved => "approved",
        ModuleApprovalDecision::Denied => "denied",
        ModuleApprovalDecision::TimedOut => "timed_out",
    }
}

fn parse_decision(s: &str) -> Result<ModuleApprovalDecision> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(StoreError::from)
}

/// Every column ANY `UPDATE module_approvals` statement in this module may
/// set — deliberately exhaustive. There is NO variant for `action`/`target`/
/// `payload`/`payload_digest`: extending one of the three statements below
/// (or adding a fourth) to touch one of those columns requires first adding
/// a variant here, a one-line change any reviewer sees, rather than just a
/// new substring inside a new SQL string. This makes "approved A, executed
/// B" (judgement 6) structurally unreachable through this helper, instead of
/// merely true of today's three statements by convention — a source-text
/// scan (an earlier version of this guard) proves only the latter, and can
/// be defeated by whitespace, casing, or moving the statement to another
/// file.
#[derive(Clone, Copy)]
enum MutableColumn {
    Decision,
    DecidedAt,
    ExecutedAt,
}

impl MutableColumn {
    fn name(self) -> &'static str {
        match self {
            MutableColumn::Decision => "decision",
            MutableColumn::DecidedAt => "decided_at",
            MutableColumn::ExecutedAt => "executed_at",
        }
    }
}

/// Builds `col1 = ?, col2 = ?, …`, in order — the SOLE place a SET clause
/// against `module_approvals` is assembled, so every mutation below binds
/// its values in exactly the order `columns` lists them.
fn set_clause(columns: &[MutableColumn]) -> String {
    columns
        .iter()
        .map(|c| format!("{} = ?", c.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn row_to_module_approval(row: &SqliteRow) -> Result<ModuleApproval> {
    Ok(ModuleApproval {
        id: row.get("id"),
        module: row.get("module"),
        request_id: row.get("request_id"),
        kind: parse_kind(&row.get::<String, _>("kind"))?,
        binding: row.get::<bool, _>("binding"),
        action: row.get("action"),
        target: row.get("target"),
        payload: serde_json::from_str(&row.get::<String, _>("payload"))?,
        payload_digest: row.get("payload_digest"),
        decision: parse_decision(&row.get::<String, _>("decision"))?,
        created_at: row.get("created_at"),
        decided_at: row.get("decided_at"),
        expires_at: row.get("expires_at"),
        executed_at: row.get("executed_at"),
    })
}

impl Store {
    /// Idempotency lookup (design doc decision 4, step 3): does a record
    /// already exist for this `(module, request_id, kind)`? The caller must
    /// use this BEFORE touching `Generation::admit_approval_callback` — a
    /// lost response, retried by the module with the same `request_id`, must
    /// not re-validate the token or create a second row.
    pub async fn find_module_approval(
        &self,
        module: &str,
        request_id: &str,
        kind: ModuleApprovalKind,
    ) -> Result<Option<ModuleApproval>> {
        let row = sqlx::query(
            "SELECT * FROM module_approvals WHERE module = ? AND request_id = ? AND kind = ?",
        )
        .bind(module)
        .bind(request_id)
        .bind(kind_str(kind))
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(row_to_module_approval).transpose()
    }

    /// Insert a brand-new `Pending` row (design doc decision 4, step 5). The
    /// caller must already have done the capability check, the closed-set
    /// check (for `Gate`), the idempotency lookup above, and — for the
    /// out-of-process path — token admission; this call assumes all of that
    /// already succeeded and does not re-check any of it.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_module_approval(
        &self,
        id: &str,
        module: &str,
        request_id: &str,
        kind: ModuleApprovalKind,
        binding: bool,
        action: &str,
        target: Option<&str>,
        payload: &serde_json::Value,
        payload_digest: &str,
        created_at: &str,
        expires_at: &str,
    ) -> Result<ModuleApproval> {
        let payload_json = serde_json::to_string(payload)?;
        let row = sqlx::query(
            "INSERT INTO module_approvals
                (id, module, request_id, kind, binding, action, target, payload,
                 payload_digest, decision, created_at, decided_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, NULL, ?)
             RETURNING *",
        )
        .bind(id)
        .bind(module)
        .bind(request_id)
        .bind(kind_str(kind))
        .bind(binding)
        .bind(action)
        .bind(target)
        .bind(payload_json)
        .bind(payload_digest)
        .bind(created_at)
        .bind(expires_at)
        .fetch_one(self.pool())
        .await?;
        row_to_module_approval(&row)
    }

    pub async fn get_module_approval(&self, id: &str) -> Result<Option<ModuleApproval>> {
        let row = sqlx::query("SELECT * FROM module_approvals WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_module_approval).transpose()
    }

    /// Scoped by `module` INSIDE the SQL condition (design doc decision 4,
    /// step 2): "belongs to another module" and "does not exist at all" must
    /// be indistinguishable, which requires the same query path for both —
    /// not a row fetched by `id` alone and compared against `module` in
    /// application code afterwards.
    pub async fn get_module_approval_for_module(
        &self,
        module: &str,
        id: &str,
    ) -> Result<Option<ModuleApproval>> {
        let row = sqlx::query("SELECT * FROM module_approvals WHERE id = ? AND module = ?")
            .bind(id)
            .bind(module)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_module_approval).transpose()
    }

    pub async fn list_module_approvals(
        &self,
        decision: Option<ModuleApprovalDecision>,
    ) -> Result<Vec<ModuleApproval>> {
        let rows = match decision {
            Some(d) => {
                sqlx::query(
                    "SELECT * FROM module_approvals WHERE decision = ? \
                     ORDER BY created_at ASC, id ASC",
                )
                .bind(decision_str(d))
                .fetch_all(self.pool())
                .await?
            }
            None => {
                sqlx::query("SELECT * FROM module_approvals ORDER BY created_at ASC, id ASC")
                    .fetch_all(self.pool())
                    .await?
            }
        };
        rows.iter().map(row_to_module_approval).collect()
    }

    /// The decision CAS (design doc decision 3): `now` is checked against
    /// `expires_at` in the SAME statement as the `pending` check — `>=` here
    /// and `<` in [`Self::timeout_expired_module_approvals`] are mutually
    /// exclusive and jointly exhaustive against one injected clock, so there
    /// is no instant at which neither query would touch a given row.
    ///
    /// 0 rows affected → re-query by `id` ALONE to distinguish 404 (never
    /// existed) from 409 (already resolved, or expired) — a second read,
    /// not part of the CAS itself, following `resolve_approval`'s existing
    /// precedent in `repo.rs`.
    pub async fn decide_module_approval(
        &self,
        id: &str,
        to: ModuleApprovalDecision,
        now: &str,
    ) -> Result<ModuleApproval> {
        let sql = format!(
            "UPDATE module_approvals SET {} \
             WHERE id = ? AND decision = 'pending' AND expires_at >= ?",
            set_clause(&[MutableColumn::Decision, MutableColumn::DecidedAt]),
        );
        let result = sqlx::query(&sql)
            .bind(decision_str(to))
            .bind(now)
            .bind(id)
            .bind(now)
            .execute(self.pool())
            .await?;
        if result.rows_affected() == 0 {
            return match self.get_module_approval(id).await? {
                None => Err(StoreError::NotFound(format!("module approval {id}"))),
                Some(_) => Err(StoreError::Conflict(format!(
                    "module approval {id} already resolved or expired"
                ))),
            };
        }
        self.get_module_approval(id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("module approval {id}")))
    }

    /// Design doc decision 5's periodic scan: judge every `pending` row
    /// whose `expires_at < now` as `timed_out`, in one statement, returning
    /// the ids so the caller can push `module-approval.resolved` for each
    /// (best-effort WS push — the DB write here is what actually determines
    /// the state; a dropped event does not un-decide anything, and a client
    /// can always re-read via `status`/REST).
    pub async fn timeout_expired_module_approvals(&self, now: &str) -> Result<Vec<String>> {
        let sql = format!(
            "UPDATE module_approvals SET {} \
             WHERE decision = 'pending' AND expires_at < ? \
             RETURNING id",
            set_clause(&[MutableColumn::Decision, MutableColumn::DecidedAt]),
        );
        let rows = sqlx::query(&sql)
            .bind(decision_str(ModuleApprovalDecision::TimedOut))
            .bind(now)
            .bind(now)
            .fetch_all(self.pool())
            .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>("id")).collect())
    }

    /// T7c/ME-3e decision "批准即生效": the scan's SECOND, INDEPENDENT query —
    /// judge every `approved`, not-yet-executed `gate` row whose closed-set
    /// action is `schedule_callback` and whose (already-canonicalized, at
    /// submission time) `target` has been reached (`<=`, not `<` — a `target`
    /// exactly equal to `now` counts as reached, judgement 13) as executed,
    /// in one CAS statement. `WHERE executed_at IS NULL` is what makes this a
    /// CAS: two overlapping scans can both match the row, but only one
    /// `UPDATE` actually flips it (judgement 8) — the same pattern
    /// [`Self::decide_module_approval`] and
    /// [`Self::timeout_expired_module_approvals`] already use.
    ///
    /// No `RETURNING`, unlike [`Self::timeout_expired_module_approvals`]:
    /// this round pushes no WS event for "executed" (design doc decision
    /// "扩展 T7b 的周期扫描" — `status` polling is enough), so the caller only
    /// needs a count, not which rows. Returns the number of rows this call
    /// itself flipped (0 on a scan that finds nothing due, or on a second
    /// racing scan that lost the CAS).
    pub async fn execute_due_schedule_callbacks(&self, now: &str) -> Result<u64> {
        let sql = format!(
            "UPDATE module_approvals SET {} \
             WHERE kind = 'gate' AND decision = 'approved' AND executed_at IS NULL \
               AND action = 'schedule_callback' AND target <= ?",
            set_clause(&[MutableColumn::ExecutedAt]),
        );
        let result = sqlx::query(&sql)
            .bind(now)
            .bind(now)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn kind_and_decision_strings_roundtrip_through_serde() {
        // Guards the hand-maintained *_str tables against drifting from the
        // serde snake_case wire names (mirrors repo.rs's
        // `status_strings_roundtrip_through_serde`).
        for k in [ModuleApprovalKind::Gate, ModuleApprovalKind::Advise] {
            assert_eq!(parse_kind(kind_str(k)).unwrap(), k);
        }
        for d in [
            ModuleApprovalDecision::Pending,
            ModuleApprovalDecision::Approved,
            ModuleApprovalDecision::Denied,
            ModuleApprovalDecision::TimedOut,
        ] {
            assert_eq!(parse_decision(decision_str(d)).unwrap(), d);
        }
    }

    async fn store() -> Store {
        Store::open_memory().await.unwrap()
    }

    #[tokio::test]
    async fn insert_then_find_then_get_roundtrip() {
        let store = store().await;
        let inserted = store
            .insert_module_approval(
                "apr_1",
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                false,
                "send_email",
                Some("ops@example.com"),
                &serde_json::json!({"x": 1}),
                "sha256:deadbeef",
                "2026-09-17T00:00:00Z",
                "2026-09-17T00:05:00Z",
            )
            .await
            .unwrap();
        assert_eq!(inserted.decision, ModuleApprovalDecision::Pending);
        assert!(!inserted.binding);

        let found = store
            .find_module_approval("probe", "req-1", ModuleApprovalKind::Advise)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, "apr_1");

        // Wrong kind: this request_id's `gate` row does not exist.
        assert!(
            store
                .find_module_approval("probe", "req-1", ModuleApprovalKind::Gate)
                .await
                .unwrap()
                .is_none()
        );

        let got = store.get_module_approval("apr_1").await.unwrap().unwrap();
        assert_eq!(got.payload_digest, "sha256:deadbeef");

        // Cross-module and nonexistent lookups both come back None, via the
        // same query path (judgement 11/12).
        assert!(
            store
                .get_module_approval_for_module("someone-else", "apr_1")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_module_approval_for_module("probe", "does-not-exist")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_module_approval_for_module("probe", "apr_1")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn the_unique_constraint_rejects_a_second_row_for_the_same_key() {
        let store = store().await;
        store
            .insert_module_approval(
                "apr_1",
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                false,
                "a",
                None,
                &serde_json::json!({}),
                "sha256:x",
                "t0",
                "t1",
            )
            .await
            .unwrap();
        let err = store
            .insert_module_approval(
                "apr_2",
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                false,
                "a",
                None,
                &serde_json::json!({}),
                "sha256:x",
                "t0",
                "t1",
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Sqlx(_)));
    }

    #[tokio::test]
    async fn decide_cas_distinguishes_404_from_409() {
        let store = store().await;
        assert!(matches!(
            store
                .decide_module_approval("nope", ModuleApprovalDecision::Approved, "t0")
                .await
                .unwrap_err(),
            StoreError::NotFound(_)
        ));

        store
            .insert_module_approval(
                "apr_1",
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                false,
                "a",
                None,
                &serde_json::json!({}),
                "sha256:x",
                "t0",
                "2026-09-17T00:05:00Z",
            )
            .await
            .unwrap();
        let decided = store
            .decide_module_approval(
                "apr_1",
                ModuleApprovalDecision::Approved,
                "2026-09-17T00:00:01Z",
            )
            .await
            .unwrap();
        assert_eq!(decided.decision, ModuleApprovalDecision::Approved);
        assert!(decided.decided_at.is_some());

        // A second decide on the same (now resolved) row is a conflict, not
        // a silent overwrite.
        assert!(matches!(
            store
                .decide_module_approval(
                    "apr_1",
                    ModuleApprovalDecision::Denied,
                    "2026-09-17T00:00:02Z"
                )
                .await
                .unwrap_err(),
            StoreError::Conflict(_)
        ));
    }

    #[tokio::test]
    async fn decide_cas_refuses_an_already_expired_row() {
        let store = store().await;
        store
            .insert_module_approval(
                "apr_1",
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                false,
                "a",
                None,
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-17T00:00:00Z",
                "2026-09-17T00:00:00Z", // already expired at t0
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .decide_module_approval(
                    "apr_1",
                    ModuleApprovalDecision::Approved,
                    "2026-09-17T00:00:01Z"
                )
                .await
                .unwrap_err(),
            StoreError::Conflict(_)
        ));
    }

    #[tokio::test]
    async fn the_periodic_scan_times_out_expired_pending_rows_and_only_those() {
        let store = store().await;
        store
            .insert_module_approval(
                "apr_expired",
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                false,
                "a",
                None,
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-17T00:00:00Z",
                "2026-09-17T00:00:00Z",
            )
            .await
            .unwrap();
        store
            .insert_module_approval(
                "apr_fresh",
                "probe",
                "req-2",
                ModuleApprovalKind::Advise,
                false,
                "a",
                None,
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-17T00:00:00Z",
                "2026-09-17T01:00:00Z",
            )
            .await
            .unwrap();

        let timed_out = store
            .timeout_expired_module_approvals("2026-09-17T00:00:01Z")
            .await
            .unwrap();
        assert_eq!(timed_out, vec!["apr_expired".to_owned()]);

        let expired = store
            .get_module_approval("apr_expired")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(expired.decision, ModuleApprovalDecision::TimedOut);
        let fresh = store
            .get_module_approval("apr_fresh")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fresh.decision, ModuleApprovalDecision::Pending);

        // A second scan at the same instant is a no-op: nothing left pending
        // that is also expired.
        assert!(
            store
                .timeout_expired_module_approvals("2026-09-17T00:00:01Z")
                .await
                .unwrap()
                .is_empty()
        );
    }

    // ── T7c/ME-3e: `execute_due_schedule_callbacks` ───────────────────────

    #[tokio::test]
    async fn execute_due_schedule_callbacks_marks_approved_rows_whose_target_has_been_reached() {
        let store = store().await;
        store
            .insert_module_approval(
                "apr_due",
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some("2026-09-17T00:00:00Z"),
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                "2026-09-18T00:00:00Z",
            )
            .await
            .unwrap();
        store
            .decide_module_approval(
                "apr_due",
                ModuleApprovalDecision::Approved,
                "2026-09-16T00:00:01Z",
            )
            .await
            .unwrap();

        store
            .insert_module_approval(
                "apr_not_due",
                "probe",
                "req-2",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some("2026-09-20T00:00:00Z"),
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                "2026-09-18T00:00:00Z",
            )
            .await
            .unwrap();
        store
            .decide_module_approval(
                "apr_not_due",
                ModuleApprovalDecision::Approved,
                "2026-09-16T00:00:01Z",
            )
            .await
            .unwrap();

        // Judgement 13: `target` EXACTLY equal to `now` must count as
        // "reached" (`<=`, not `<`).
        let n = store
            .execute_due_schedule_callbacks("2026-09-17T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(n, 1);

        let due = store.get_module_approval("apr_due").await.unwrap().unwrap();
        assert_eq!(due.executed_at.as_deref(), Some("2026-09-17T00:00:00Z"));
        let not_due = store
            .get_module_approval("apr_not_due")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            not_due.executed_at, None,
            "a row whose target has not been reached must be left alone"
        );
    }

    #[tokio::test]
    async fn execute_due_schedule_callbacks_never_touches_non_approved_or_non_gate_rows() {
        // Judgement 4/5/10: pending, denied, timed_out gate rows, and an
        // advise row with the identical action/target shape, must all be
        // immune — even though every one of them has a `target` far in the
        // past by the time the scan runs.
        let store = store().await;
        let past = "2020-01-01T00:00:00Z";
        let far_expiry = "2026-09-18T00:00:00Z";

        store
            .insert_module_approval(
                "apr_pending",
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some(past),
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                far_expiry,
            )
            .await
            .unwrap();

        store
            .insert_module_approval(
                "apr_denied",
                "probe",
                "req-2",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some(past),
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                far_expiry,
            )
            .await
            .unwrap();
        store
            .decide_module_approval(
                "apr_denied",
                ModuleApprovalDecision::Denied,
                "2026-09-16T00:00:01Z",
            )
            .await
            .unwrap();

        store
            .insert_module_approval(
                "apr_timed_out",
                "probe",
                "req-3",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some(past),
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                "2026-09-16T00:00:00Z", // already expired
            )
            .await
            .unwrap();
        store
            .timeout_expired_module_approvals("2026-09-16T00:00:01Z")
            .await
            .unwrap();

        store
            .insert_module_approval(
                "apr_advise",
                "probe",
                "req-4",
                ModuleApprovalKind::Advise,
                false,
                "schedule_callback",
                Some(past),
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                far_expiry,
            )
            .await
            .unwrap();

        let n = store
            .execute_due_schedule_callbacks("2026-09-17T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(n, 0);

        for id in ["apr_pending", "apr_denied", "apr_timed_out", "apr_advise"] {
            let row = store.get_module_approval(id).await.unwrap().unwrap();
            assert_eq!(row.executed_at, None, "{id} must never be marked executed");
        }
    }

    #[tokio::test]
    async fn execute_due_schedule_callbacks_is_a_cas_a_later_scan_never_overwrites_it() {
        // Judgement 8 (sequential half; `module_approval_broker`'s test
        // covers a genuinely concurrent race): once executed, `WHERE
        // executed_at IS NULL` removes the row from every later scan's
        // candidate set, so a later `now` can never push the timestamp
        // forward.
        let store = store().await;
        store
            .insert_module_approval(
                "apr_1",
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some("2026-09-17T00:00:00Z"),
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                "2026-09-18T00:00:00Z",
            )
            .await
            .unwrap();
        store
            .decide_module_approval(
                "apr_1",
                ModuleApprovalDecision::Approved,
                "2026-09-16T00:00:01Z",
            )
            .await
            .unwrap();

        let first = store
            .execute_due_schedule_callbacks("2026-09-17T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(first, 1);

        let second = store
            .execute_due_schedule_callbacks("2026-09-17T05:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            second, 0,
            "an already-executed row must not be re-matched by a later scan"
        );

        let row = store.get_module_approval("apr_1").await.unwrap().unwrap();
        assert_eq!(
            row.executed_at.as_deref(),
            Some("2026-09-17T00:00:00Z"),
            "executed_at must not be overwritten with a later timestamp"
        );
    }

    #[tokio::test]
    async fn a_target_already_in_the_past_at_submission_still_executes_on_the_next_scan() {
        // Judgement 9: submitting (and approving) a `target` that has
        // already passed is not an error — the next scan executes it
        // immediately, exactly like a target that passed while waiting for a
        // human to approve it.
        let store = store().await;
        store
            .insert_module_approval(
                "apr_1",
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some("2000-01-01T00:00:00Z"),
                &serde_json::json!({}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                "2026-09-18T00:00:00Z",
            )
            .await
            .unwrap();
        let approved = store
            .decide_module_approval(
                "apr_1",
                ModuleApprovalDecision::Approved,
                "2026-09-16T00:00:01Z",
            )
            .await
            .unwrap();
        assert_eq!(approved.executed_at, None, "not executed until scanned");

        let n = store
            .execute_due_schedule_callbacks("2026-09-16T00:00:02Z")
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    // ── judgement 17: `0006` backfills existing rows with NULL, untouched ──

    #[tokio::test]
    async fn upgrading_from_0005_to_0006_leaves_a_preexisting_advise_row_with_a_null_executed_at() {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        // A standalone pool, deliberately NOT `Store::open_memory` (which
        // runs every migration up to the newest in one pass) — this
        // simulates a database that was already on `0005` (T7b, shipped)
        // before `0006` (T7c) ever existed, by replaying the two migration
        // files as two separate steps with a real row inserted in between.
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        const MIGRATION_0005: &str = include_str!("../migrations/0005_module_approvals.sql");
        sqlx::raw_sql(MIGRATION_0005).execute(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO module_approvals
                (id, module, request_id, kind, binding, action, target, payload,
                 payload_digest, decision, created_at, decided_at, expires_at)
             VALUES ('apr_legacy', 'probe', 'req-1', 'advise', 0, 'send_email', NULL,
                     '{}', 'sha256:x', 'pending', '2026-09-01T00:00:00Z', NULL,
                     '2026-09-01T00:05:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        const MIGRATION_0006: &str =
            include_str!("../migrations/0006_module_approval_executed_at.sql");
        sqlx::raw_sql(MIGRATION_0006).execute(&pool).await.unwrap();

        let row = sqlx::query("SELECT executed_at FROM module_approvals WHERE id = 'apr_legacy'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let executed_at: Option<String> = row.get("executed_at");
        assert_eq!(
            executed_at, None,
            "a pre-0006 row must backfill to NULL, not be otherwise affected"
        );
    }

    // ── judgement 6: no mutation entry point can ever rewrite action/target/
    // payload/payload_digest ────────────────────────────────────────────────
    //
    // Structural enforcement lives in `MutableColumn`/`set_clause` above —
    // there is no variant for these four columns, so none of the three
    // functions below can express a SET clause that touches them. What
    // follows are BEHAVIOR tests against each of the three exposed mutation
    // entry points (not a source-text scan, which a rename, a whitespace
    // change, or moving a statement to another file could all defeat
    // without this guarantee actually breaking): insert a row, capture its
    // action/target/payload/payload_digest, call the mutator, and assert
    // those four fields read back byte-identical.

    #[tokio::test]
    async fn decide_module_approval_never_touches_action_target_payload_or_digest() {
        let store = store().await;
        let before = store
            .insert_module_approval(
                "apr_1",
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some("2026-09-17T00:00:00Z"),
                &serde_json::json!({"x": 1}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                "2026-09-18T00:00:00Z",
            )
            .await
            .unwrap();

        let after = store
            .decide_module_approval(
                "apr_1",
                ModuleApprovalDecision::Approved,
                "2026-09-16T00:00:01Z",
            )
            .await
            .unwrap();

        assert_eq!(after.action, before.action);
        assert_eq!(after.target, before.target);
        assert_eq!(after.payload, before.payload);
        assert_eq!(after.payload_digest, before.payload_digest);
        // Sanity: the decision CAS did do its actual job.
        assert_eq!(after.decision, ModuleApprovalDecision::Approved);
    }

    #[tokio::test]
    async fn timeout_expired_module_approvals_never_touches_action_target_payload_or_digest() {
        let store = store().await;
        let before = store
            .insert_module_approval(
                "apr_1",
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some("2026-09-17T00:00:00Z"),
                &serde_json::json!({"x": 1}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                "2026-09-16T00:00:00Z", // already expired
            )
            .await
            .unwrap();

        let timed_out = store
            .timeout_expired_module_approvals("2026-09-16T00:00:01Z")
            .await
            .unwrap();
        assert_eq!(timed_out, vec!["apr_1".to_owned()]);

        let after = store.get_module_approval("apr_1").await.unwrap().unwrap();
        assert_eq!(after.action, before.action);
        assert_eq!(after.target, before.target);
        assert_eq!(after.payload, before.payload);
        assert_eq!(after.payload_digest, before.payload_digest);
        // Sanity: the timeout scan did do its actual job.
        assert_eq!(after.decision, ModuleApprovalDecision::TimedOut);
    }

    #[tokio::test]
    async fn execute_due_schedule_callbacks_never_touches_action_target_payload_or_digest() {
        let store = store().await;
        let before = store
            .insert_module_approval(
                "apr_1",
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                true,
                "schedule_callback",
                Some("2026-09-17T00:00:00Z"),
                &serde_json::json!({"x": 1}),
                "sha256:x",
                "2026-09-16T00:00:00Z",
                "2026-09-18T00:00:00Z",
            )
            .await
            .unwrap();
        store
            .decide_module_approval(
                "apr_1",
                ModuleApprovalDecision::Approved,
                "2026-09-16T00:00:01Z",
            )
            .await
            .unwrap();

        let n = store
            .execute_due_schedule_callbacks("2026-09-17T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(n, 1);

        let after = store.get_module_approval("apr_1").await.unwrap().unwrap();
        assert_eq!(after.action, before.action);
        assert_eq!(after.target, before.target);
        assert_eq!(after.payload, before.payload);
        assert_eq!(after.payload_digest, before.payload_digest);
        // Sanity: the execution scan did do its actual job.
        assert!(after.executed_at.is_some());
    }
}
