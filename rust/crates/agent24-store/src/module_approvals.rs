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
        let result = sqlx::query(
            "UPDATE module_approvals SET decision = ?, decided_at = ? \
             WHERE id = ? AND decision = 'pending' AND expires_at >= ?",
        )
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
        let rows = sqlx::query(
            "UPDATE module_approvals SET decision = 'timed_out', decided_at = ? \
             WHERE decision = 'pending' AND expires_at < ? \
             RETURNING id",
        )
        .bind(now)
        .bind(now)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>("id")).collect())
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
}
