//! The skill a run was started with (SL-5, plan §4.7, §4.8, §4.11).
//!
//! A pin is derived from `skill_pointer` inside the transaction that inserts
//! the run. The caller names the skill and the revision it resolved providers
//! for; neither is written. The stored pin copies the revision id and the
//! digest from the rows. A run without a pin row used no skill.

use sqlx::Row;

use crate::repo::insert_run_tx;
use crate::skill::{SkillOrigin, SkillRevisionKind};
use crate::{Result, Store, StoreError};
use agent24_protocol::Run;

/// What the caller asks for. `expected_body_revision_id` only guards against
/// the pointer moving between provider resolution and the insert. It is never
/// stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPinRequest {
    pub skill_id: String,
    pub expected_body_revision_id: String,
    /// JSON object, capability id to `component@version`. Record only.
    pub providers_json: String,
}

/// One stored pin, with the origin read from the pinned revision row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPin {
    pub run_id: String,
    pub skill_id: String,
    pub body_revision_id: String,
    pub content_digest: String,
    pub origin: SkillOrigin,
    pub providers_json: String,
}

/// A pin and the bytes of the revision it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedSkill {
    pub pin: SkillPin,
    pub content: Vec<u8>,
}

/// What [`Store::insert_run_with_skill_pin`] did.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillPinOutcome {
    /// The run and its pin were stored.
    Pinned(SkillPin),
    /// No pointer row for this skill. Nothing was written.
    NotActive,
    /// The pointer no longer names `expected_body_revision_id`. Nothing was written.
    PointerMoved,
}

impl Store {
    /// Insert a run together with the pin derived from the current pointer.
    ///
    /// Same session checks as [`Store::insert_run`] (non-workspace runs only).
    /// One `BEGIN IMMEDIATE` transaction reads the pointer, checks that the
    /// pinned row is this skill's `body` revision, inserts the run, and
    /// inserts the pin.
    ///
    /// # Errors
    /// [`StoreError::SkillOriginMismatch`] when the pointer's two sides have
    /// different origins, [`StoreError::SkillStateCorrupt`] when the pointed revision is
    /// not this skill's body row, or storage.
    pub async fn insert_run_with_skill_pin(
        &self,
        run: &Run,
        req: &SkillPinRequest,
    ) -> Result<SkillPinOutcome> {
        if run.workspace_id.is_some() || run.input.workspace_id.is_some() {
            return Err(StoreError::SkillStateCorrupt(
                "skill pin is not available for workspace runs".to_owned(),
            ));
        }
        if req.skill_id.is_empty()
            || req.expected_body_revision_id.is_empty()
            || serde_json::from_str::<serde_json::Value>(&req.providers_json)
                .ok()
                .is_none_or(|value| !value.is_object())
        {
            return Err(StoreError::SkillStateCorrupt(
                "skill pin request".to_owned(),
            ));
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT p.body_revision_id, b.skill_id AS rev_skill, b.kind, \
                    b.origin AS body_origin, e.origin AS evals_origin, b.content_digest \
             FROM skill_pointer p \
             JOIN skill_revision b ON b.revision_id = p.body_revision_id \
             JOIN skill_revision e ON e.revision_id = p.evals_revision_id \
             WHERE p.skill_id = ?",
        )
        .bind(&req.skill_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(SkillPinOutcome::NotActive);
        };
        let body_origin = SkillOrigin::parse(&row.get::<String, _>("body_origin"))?;
        let evals_origin = SkillOrigin::parse(&row.get::<String, _>("evals_origin"))?;
        if body_origin != evals_origin {
            return Err(StoreError::SkillOriginMismatch);
        }
        let kind: String = row.get("kind");
        let rev_skill: String = row.get("rev_skill");
        if kind != SkillRevisionKind::Body.as_str() || rev_skill != req.skill_id {
            return Err(StoreError::SkillStateCorrupt(
                "skill pointer names a revision that is not this skill's body".to_owned(),
            ));
        }
        let body_revision_id: String = row.get("body_revision_id");
        if body_revision_id != req.expected_body_revision_id {
            return Ok(SkillPinOutcome::PointerMoved);
        }
        let content_digest: String = row.get("content_digest");
        if let Some(session_id) = run.session_id.as_deref() {
            let found =
                sqlx::query("SELECT workspace_id FROM sessions WHERE id = ? COLLATE BINARY")
                    .bind(session_id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::NotFound(format!("session {session_id}")))?;
            if found.get::<Option<String>, _>("workspace_id").is_some() {
                return Err(StoreError::SkillStateCorrupt(
                    "workspace-bound session reached the skill pin".to_owned(),
                ));
            }
        }
        insert_run_tx(&mut tx, run).await?;
        sqlx::query(
            "INSERT INTO run_skill_pin \
             (run_id, skill_id, body_revision_id, content_digest, providers_json) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&run.id)
        .bind(&req.skill_id)
        .bind(&body_revision_id)
        .bind(&content_digest)
        .bind(&req.providers_json)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(SkillPinOutcome::Pinned(SkillPin {
            run_id: run.id.clone(),
            skill_id: req.skill_id.clone(),
            body_revision_id,
            content_digest,
            origin: body_origin,
            providers_json: req.providers_json.clone(),
        }))
    }

    /// The pin of a run and the pinned revision's bytes. `None` for a run with
    /// no pin row (it used no skill). The pin is checked against the revision
    /// row it names: skill, `body` kind, and digest must all agree.
    ///
    /// # Errors
    /// [`StoreError::SkillStateCorrupt`] when the pin and the revision disagree, or storage.
    pub async fn get_run_skill_pin(&self, run_id: &str) -> Result<Option<PinnedSkill>> {
        let row = sqlx::query(
            "SELECT p.run_id, p.skill_id, p.body_revision_id, p.content_digest, p.providers_json, \
                    r.skill_id AS rev_skill, r.kind, r.origin, r.content_digest AS rev_digest, r.content \
             FROM run_skill_pin p \
             LEFT JOIN skill_revision r ON r.revision_id = p.body_revision_id \
             WHERE p.run_id = ?",
        )
        .bind(run_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mismatch =
            || StoreError::SkillStateCorrupt("skill pin does not match its revision".to_owned());
        let rev_skill: Option<String> = row.get("rev_skill");
        let kind: Option<String> = row.get("kind");
        let rev_digest: Option<String> = row.get("rev_digest");
        let skill_id: String = row.get("skill_id");
        let content_digest: String = row.get("content_digest");
        if rev_skill.as_deref() != Some(skill_id.as_str())
            || kind.as_deref() != Some(SkillRevisionKind::Body.as_str())
            || rev_digest.as_deref() != Some(content_digest.as_str())
        {
            return Err(mismatch());
        }
        let origin = SkillOrigin::parse(&row.get::<String, _>("origin"))?;
        Ok(Some(PinnedSkill {
            pin: SkillPin {
                run_id: row.get("run_id"),
                skill_id,
                body_revision_id: row.get("body_revision_id"),
                content_digest,
                origin,
                providers_json: row.get("providers_json"),
            },
            content: row.get("content"),
        }))
    }
}
