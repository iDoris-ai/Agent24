//! Skill revisions, the current pointer, activation history, and eval bindings.
//!
//! SL-2 (skill-loader plan §4.2, §4.3, §4.5, §4.11). Revisions are insert-only.
//! [`Store::insert_skill_revision`] checks a byte SHA-256, then dedupes on
//! `(skill_id, origin, kind, content_digest)`. [`Store::install_skill_pair`]
//! looks up `(skill_id, origin, semver, kind)` first. The same semver with a
//! different digest is [`StoreError::SkillRevisionConflict`] and does not
//! replace the row.
//!
//! The pointer moves by compare-and-swap on `generation`. No pointer row is
//! generation 0. A stale generation is [`StoreError::SkillPointerConflict`]
//! and is reported before [`StoreError::SkillActivationMixed`]. Activation and
//! rollback share one move. The body revision and the evals revision must have
//! the same origin ([`StoreError::SkillOriginMismatch`]). When that shared
//! origin differs from the current pointer, both sides may change in one
//! activation (a source switch, treated like the first pair). The pointer
//! primary key stays `skill_id`. History rows carry `origin_from` / `origin_to`
//! for the confirmation summary.
//!
//! [`Store::insert_skill_revision`] treats `content_digest` as the lowercase
//! hex SHA-256 of `content` and rejects a mismatch with [`StoreError::Conflict`].
//! That single-row entry is what the store tests call. Install and scan use
//! [`Store::install_skill_pair`]: a body row's digest is the manifest digest
//! (not recomputed here), and an evals row's digest is the SHA-256 of its
//! bytes, which the pair path recomputes.
//!
//! # `StoreError::Conflict` from this module
//!
//! - a required text field is empty
//! - `content_digest` is not the SHA-256 of `content`
//! - `revision_id`, `result_id`, or `activation_id` is already stored,
//!   including when the pointer is already on this pair and the stored row
//!   is not this skill's row for that pair
//! - the digest row exists but the bytes differ
//! - the revision's skill or kind is not the one requested
//! - the rollback source belongs to another skill
//! - an accepted id is not a failed case id in this binding, or is repeated
//! - `cases_json` or `accepted_json` is not a JSON array of the required shape
//! - a stored origin, kind, or result code is outside its closed set
//! - the generation would overflow
//!
//! Those duplicate ids are checked before insert, so a repeat comes back as
//! [`StoreError::Conflict`] and not as a SQLite unique-constraint
//! [`StoreError::Sqlx`].
//!
//! SL-4 treats only `SQLITE_BUSY` as retryable. A busy timeout surfaces as
//! [`StoreError::Sqlx`]. Any other [`StoreError::Sqlx`], [`StoreError::Conflict`],
//! and the typed skill codes are not retryable.
//!
//! This module does not parse a manifest, score an eval, or read the filesystem.

use std::collections::HashSet;

use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::Transaction;

use crate::{Result, Store, StoreError};

const AUDIT_ACTIVATE: &str = "skill.activate";
const AUDIT_ROLLBACK: &str = "skill.rollback";
const CODE_ACTIVATED: &str = "activated";
const CODE_ROLLED_BACK: &str = "rolled_back";

/// Where a revision came from. Callers cannot promote one into the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOrigin {
    /// Written under the user's data directory.
    User,
    /// Imported text. Stays imported after activation.
    Imported,
}

impl SkillOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Imported => "imported",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "imported" => Ok(Self::Imported),
            other => Err(StoreError::Conflict(format!("skill origin {other}"))),
        }
    }
}

/// Body text and eval text are separate immutable rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillRevisionKind {
    /// `SKILL.md` bytes.
    Body,
    /// Eval file bytes.
    Evals,
}

impl SkillRevisionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Evals => "evals",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "body" => Ok(Self::Body),
            "evals" => Ok(Self::Evals),
            other => Err(StoreError::Conflict(format!("skill kind {other}"))),
        }
    }
}

/// Revision identity without the blob. List responses use this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRevisionMeta {
    pub revision_id: String,
    pub skill_id: String,
    pub origin: SkillOrigin,
    pub semver: String,
    pub kind: SkillRevisionKind,
    pub content_digest: String,
    pub created_at: String,
}

/// One immutable revision. `content` is the raw bytes the caller stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRevision {
    pub revision_id: String,
    pub skill_id: String,
    pub origin: SkillOrigin,
    pub semver: String,
    pub kind: SkillRevisionKind,
    pub content_digest: String,
    pub content: Vec<u8>,
    pub created_at: String,
}

/// Insert input. The store does not allocate `revision_id` and does not read a clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSkillRevision {
    pub revision_id: String,
    pub skill_id: String,
    pub origin: SkillOrigin,
    pub semver: String,
    pub kind: SkillRevisionKind,
    pub content_digest: String,
    pub content: Vec<u8>,
    pub created_at: String,
}

/// What [`Store::insert_skill_revision`] did. Neither variant overwrites a row.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRevisionInsert {
    /// A new row was stored under the caller's `revision_id`.
    Inserted { revision_id: String },
    /// The same bytes were already stored. `revision_id` is the stored one.
    Existing { revision_id: String },
}

/// The current pair. Absence means the skill is installed and not active.
///
/// `origin` is the shared origin of the two revisions. The table primary key
/// is `skill_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPointer {
    pub skill_id: String,
    pub origin: SkillOrigin,
    pub body_revision_id: String,
    pub evals_revision_id: String,
    pub generation: i64,
}

/// Eval binding written with an activation.
///
/// `cases_json` is a JSON array of case summaries. `accepted_json` is a JSON
/// array of ids. Each id appears once and is a failed case in `cases_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEvalBinding {
    pub result_id: String,
    pub segment_digest: String,
    pub renderer_version: String,
    pub cases_json: String,
    pub accepted_json: String,
}

/// Activation request. `expected_generation` is 0 when no pointer row exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSkillActivation {
    pub activation_id: String,
    pub skill_id: String,
    pub expected_generation: i64,
    pub body_revision_id: String,
    pub evals_revision_id: String,
    pub binding: SkillEvalBinding,
    pub created_at: String,
    pub actor: String,
}

/// Rollback request. The pair comes from `source_activation_id`, not from the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRollback {
    pub activation_id: String,
    pub skill_id: String,
    pub expected_generation: i64,
    pub source_activation_id: String,
    pub created_at: String,
    pub actor: String,
}

/// Result of a pointer move.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillActivationOutcome {
    /// Pointer generation advanced and a history row was inserted.
    Activated {
        generation: i64,
        activation_id: String,
    },
    /// The pointer already named this pair. Nothing was written. The request's
    /// eval binding was not validated.
    Unchanged { generation: i64 },
}

/// One history row joined to its eval binding and the body revision's origin.
///
/// Rows are ordered by `seq`, not by `created_at`. `origin_from` is the
/// previous row's origin (`None` on the first row). `origin_to` is this row.
/// The confirmation summary reads that pair as the source switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillActivationRecord {
    pub seq: i64,
    pub activation_id: String,
    pub skill_id: String,
    pub origin: SkillOrigin,
    pub origin_from: Option<SkillOrigin>,
    pub origin_to: SkillOrigin,
    pub body_revision_id: String,
    pub evals_revision_id: String,
    pub result_id: String,
    pub segment_digest: String,
    pub renderer_version: String,
    pub cases_json: String,
    pub accepted_json: String,
    pub result_code: String,
    pub created_at: String,
}

struct StoredRevision {
    origin: SkillOrigin,
    content_digest: String,
}

struct StoredActivation {
    skill_id: String,
    body_revision_id: String,
    evals_revision_id: String,
    result_id: String,
}

struct PointerMove<'a> {
    activation_id: &'a str,
    skill_id: &'a str,
    expected_generation: i64,
    body_revision_id: &'a str,
    evals_revision_id: &'a str,
    result_id: &'a str,
    result_code: &'a str,
    created_at: &'a str,
    actor: &'a str,
    audit_action: &'a str,
    /// Rollback restores a historical pair, which may differ on both sides.
    allow_both_sides: bool,
    binding: Option<&'a SkillEvalBinding>,
}

fn require_text(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(StoreError::Conflict(format!("skill {field} is empty")));
    }
    Ok(())
}

fn require_revision_input(rev: &NewSkillRevision) -> Result<()> {
    require_text("revision_id", &rev.revision_id)?;
    require_text("skill_id", &rev.skill_id)?;
    require_text("semver", &rev.semver)?;
    require_text("content_digest", &rev.content_digest)?;
    require_text("created_at", &rev.created_at)?;
    Ok(())
}

fn json_array(field: &str, value: &str) -> Result<Vec<Value>> {
    let parsed = serde_json::from_str::<Value>(value)
        .map_err(|_| StoreError::Conflict(format!("skill {field} is not a json array")))?;
    parsed
        .as_array()
        .cloned()
        .ok_or_else(|| StoreError::Conflict(format!("skill {field} is not a json array")))
}

/// Cases must be non-empty. Every accepted id must be a failed case, and
/// every failed case must be accepted. Empty is [`StoreError::SkillEvalEmpty`].
/// A failed id left out is [`StoreError::SkillEvalFailed`].
fn require_accepted(cases_json: &str, accepted_json: &str) -> Result<()> {
    let cases = json_array("cases_json", cases_json)?;
    if cases.is_empty() {
        return Err(StoreError::SkillEvalEmpty);
    }
    let mut failed = HashSet::new();
    let mut seen = HashSet::new();
    for case in &cases {
        let Some(obj) = case.as_object() else {
            return Err(StoreError::Conflict(
                "skill case is not an object".to_owned(),
            ));
        };
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| StoreError::Conflict("skill case id is missing".to_owned()))?;
        if !seen.insert(id.to_owned()) {
            return Err(StoreError::Conflict(
                "skill case id is duplicated".to_owned(),
            ));
        }
        let passed = obj
            .get("passed")
            .and_then(Value::as_bool)
            .ok_or_else(|| StoreError::Conflict("skill case passed is missing".to_owned()))?;
        if !passed {
            failed.insert(id.to_owned());
        }
    }
    let mut accepted = HashSet::new();
    for item in json_array("accepted_json", accepted_json)? {
        let Some(id) = item.as_str() else {
            return Err(StoreError::Conflict(
                "skill accepted id is not a string".to_owned(),
            ));
        };
        if !accepted.insert(id.to_owned()) {
            return Err(StoreError::Conflict(
                "skill accepted id is duplicated".to_owned(),
            ));
        }
        if !failed.contains(id) {
            return Err(StoreError::Conflict(
                "skill accepted id is not a failed case".to_owned(),
            ));
        }
    }
    if failed.iter().any(|id| !accepted.contains(id)) {
        return Err(StoreError::SkillEvalFailed);
    }
    Ok(())
}

/// Lowercase hex SHA-256 of the revision bytes. Callers pass this as
/// `content_digest`; the store recomputes it and rejects a mismatch.
fn content_digest_of(content: &[u8]) -> String {
    Sha256::digest(content)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn same_pair(pointer: &SkillPointer, body_revision_id: &str, evals_revision_id: &str) -> bool {
    pointer.body_revision_id == body_revision_id && pointer.evals_revision_id == evals_revision_id
}

impl Store {
    /// Insert one revision, or return the stored id when the bytes match.
    ///
    /// Lookup order: the same semver with a different digest is
    /// [`StoreError::SkillRevisionConflict`]. The same
    /// `(skill_id, origin, kind, content_digest)` and the same bytes return
    /// [`SkillRevisionInsert::Existing`], including when semver is higher.
    ///
    /// # Errors
    /// Conflict, or storage.
    pub async fn insert_skill_revision(
        &self,
        rev: &NewSkillRevision,
    ) -> Result<SkillRevisionInsert> {
        require_revision_input(rev)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let inserted = insert_revision_tx(&mut tx, rev).await?;
        tx.commit().await?;
        Ok(inserted)
    }

    /// Insert a body row and an evals row in one transaction.
    ///
    /// Lookup is `(skill_id, origin, semver, kind)` first. The same digest
    /// returns the stored id, even when a body row's bytes differ (the body
    /// digest is the manifest digest, so a version-only edit reuses the row).
    /// A different digest at that semver is
    /// [`StoreError::SkillRevisionConflict`] and rolls back both inserts.
    /// Only a semver miss falls through to
    /// `(skill_id, origin, kind, content_digest)`.
    ///
    /// An evals row's digest is the SHA-256 of its bytes. This path recomputes
    /// it and rejects a mismatch with [`StoreError::Conflict`]. A body row is
    /// not checked here; the route asserts the manifest digest.
    /// [`Store::insert_skill_revision`] still requires a byte digest for both.
    ///
    /// A requested `revision_id` that already names a different row is
    /// [`StoreError::Conflict`] before either side returns
    /// [`SkillRevisionInsert::Existing`]. The same id as the adopted row is
    /// not a conflict.
    ///
    /// # Errors
    /// Conflict, a kind mismatch, or storage.
    pub async fn install_skill_pair(
        &self,
        body: &NewSkillRevision,
        evals: &NewSkillRevision,
    ) -> Result<(SkillRevisionInsert, SkillRevisionInsert)> {
        require_revision_input(body)?;
        require_revision_input(evals)?;
        if body.kind != SkillRevisionKind::Body || evals.kind != SkillRevisionKind::Evals {
            return Err(StoreError::Conflict(
                "skill pair kinds are body then evals".to_owned(),
            ));
        }
        if body.skill_id != evals.skill_id
            || body.origin != evals.origin
            || body.semver != evals.semver
        {
            return Err(StoreError::Conflict(
                "skill pair does not share skill, origin, and semver".to_owned(),
            ));
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let body_row = adopt_or_insert(&mut tx, body).await?;
        let evals_row = adopt_or_insert(&mut tx, evals).await?;
        tx.commit().await?;
        Ok((body_row, evals_row))
    }

    /// Every revision, without blobs, oldest `created_at` first.
    ///
    /// # Errors
    /// Storage, or a row outside the closed sets.
    pub async fn list_skill_revisions(&self) -> Result<Vec<SkillRevisionMeta>> {
        let rows = sqlx::query(
            "SELECT revision_id, skill_id, origin, semver, kind, content_digest, created_at \
             FROM skill_revision ORDER BY skill_id, created_at, revision_id",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(meta_from_row).collect()
    }

    /// The current pointer, or `None` when the skill has never been activated.
    ///
    /// The view includes the shared `origin` of the two revisions.
    ///
    /// # Errors
    /// Storage, a closed-set failure, or revisions whose origins differ.
    pub async fn get_skill_pointer(&self, skill_id: &str) -> Result<Option<SkillPointer>> {
        require_text("skill_id", skill_id)?;
        let row = sqlx::query(
            "SELECT p.skill_id, p.body_revision_id, p.evals_revision_id, p.generation, \
                    b.origin AS body_origin, e.origin AS evals_origin \
             FROM skill_pointer p \
             JOIN skill_revision b ON b.revision_id = p.body_revision_id \
             JOIN skill_revision e ON e.revision_id = p.evals_revision_id \
             WHERE p.skill_id = ?",
        )
        .bind(skill_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| pointer_from_row(&row)).transpose()
    }

    /// One revision, including its bytes.
    ///
    /// # Errors
    /// Storage, or a row outside the closed sets.
    pub async fn get_skill_revision(&self, revision_id: &str) -> Result<Option<SkillRevision>> {
        require_text("revision_id", revision_id)?;
        let row = sqlx::query(
            "SELECT revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
             FROM skill_revision WHERE revision_id = ?",
        )
        .bind(revision_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| revision_from_row(&row)).transpose()
    }

    /// Activation and rollback history for one skill, oldest `seq` first.
    ///
    /// # Errors
    /// Storage, or a row outside the closed sets.
    pub async fn list_skill_activations(
        &self,
        skill_id: &str,
    ) -> Result<Vec<SkillActivationRecord>> {
        require_text("skill_id", skill_id)?;
        let rows = sqlx::query(
            "SELECT a.seq, a.activation_id, a.skill_id, a.body_revision_id, a.evals_revision_id, \
                    a.result_id, a.result_code, a.created_at, \
                    r.segment_digest, r.renderer_version, r.cases_json, r.accepted_json, \
                    b.origin AS origin \
             FROM skill_activation a \
             JOIN skill_eval_result r ON r.result_id = a.result_id \
             JOIN skill_revision b ON b.revision_id = a.body_revision_id \
             WHERE a.skill_id = ? \
             ORDER BY a.seq ASC",
        )
        .bind(skill_id)
        .fetch_all(self.pool())
        .await?;
        let mut history = Vec::with_capacity(rows.len());
        let mut previous_origin = None;
        for row in &rows {
            let mut record = activation_from_row(row)?;
            record.origin_from = previous_origin;
            record.origin_to = record.origin;
            previous_origin = Some(record.origin);
            history.push(record);
        }
        Ok(history)
    }

    /// Point the skill at one pair.
    ///
    /// The whole move, including the history row and the audit row, is one
    /// transaction. A generation mismatch writes nothing. Repeating the pair
    /// that is already current writes nothing and returns
    /// [`SkillActivationOutcome::Unchanged`]. That outcome means nothing was
    /// written. It does not mean this request's eval binding was checked and
    /// accepted. An `activation_id` or `result_id` already stored for another
    /// skill or another pair is still [`StoreError::Conflict`].
    ///
    /// # Errors
    /// [`StoreError::SkillPointerConflict`], [`StoreError::SkillActivationMixed`],
    /// [`StoreError::SkillOriginMismatch`], [`StoreError::SkillEvalEmpty`],
    /// [`StoreError::SkillEvalFailed`], missing revisions, or storage.
    pub async fn activate_skill(&self, req: &NewSkillActivation) -> Result<SkillActivationOutcome> {
        require_text("activation_id", &req.activation_id)?;
        require_text("skill_id", &req.skill_id)?;
        require_text("body_revision_id", &req.body_revision_id)?;
        require_text("evals_revision_id", &req.evals_revision_id)?;
        require_text("created_at", &req.created_at)?;
        require_text("actor", &req.actor)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let outcome = move_pointer(
            &mut tx,
            &PointerMove {
                activation_id: &req.activation_id,
                skill_id: &req.skill_id,
                expected_generation: req.expected_generation,
                body_revision_id: &req.body_revision_id,
                evals_revision_id: &req.evals_revision_id,
                result_id: &req.binding.result_id,
                result_code: CODE_ACTIVATED,
                created_at: &req.created_at,
                actor: &req.actor,
                audit_action: AUDIT_ACTIVATE,
                allow_both_sides: false,
                binding: Some(&req.binding),
            },
        )
        .await?;
        if let SkillActivationOutcome::Activated { .. } = outcome {
            tx.commit().await?;
        }
        Ok(outcome)
    }

    /// Move the pointer back to a historical pair.
    ///
    /// Does not insert an eval binding and does not change the binding the
    /// source row already points at. The audit row is in the same transaction.
    /// Both sides may differ from the current pointer. Their origins must match.
    ///
    /// # Errors
    /// [`StoreError::SkillPointerConflict`], [`StoreError::SkillOriginMismatch`],
    /// an unknown history id, or storage.
    pub async fn rollback_skill(&self, req: &SkillRollback) -> Result<SkillActivationOutcome> {
        require_text("activation_id", &req.activation_id)?;
        require_text("skill_id", &req.skill_id)?;
        require_text("source_activation_id", &req.source_activation_id)?;
        require_text("created_at", &req.created_at)?;
        require_text("actor", &req.actor)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let outcome = rollback_tx(&mut tx, req).await?;
        if let SkillActivationOutcome::Activated { .. } = outcome {
            tx.commit().await?;
        }
        Ok(outcome)
    }
}

async fn insert_revision_tx(
    tx: &mut Transaction<'_, Sqlite>,
    rev: &NewSkillRevision,
) -> Result<SkillRevisionInsert> {
    let computed = content_digest_of(&rev.content);
    if computed != rev.content_digest {
        return Err(StoreError::Conflict(
            "skill content_digest does not match content".to_owned(),
        ));
    }

    let by_semver = sqlx::query(
        "SELECT revision_id, content_digest, content FROM skill_revision \
         WHERE skill_id = ? AND origin = ? AND semver = ? AND kind = ?",
    )
    .bind(&rev.skill_id)
    .bind(rev.origin.as_str())
    .bind(&rev.semver)
    .bind(rev.kind.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = by_semver {
        let stored_id: String = row.get("revision_id");
        let decision = same_bytes_or_conflict(&row, rev)?;
        if matches!(decision, SkillRevisionInsert::Existing { .. }) {
            // Before accepting the dedupe hit: the caller's id must not already
            // name some other revision.
            ensure_revision_id_free_or_same(tx, &rev.revision_id, Some(&stored_id)).await?;
        }
        return Ok(decision);
    }

    let by_digest = sqlx::query(
        "SELECT revision_id, content_digest, content FROM skill_revision \
         WHERE skill_id = ? AND origin = ? AND kind = ? AND content_digest = ?",
    )
    .bind(&rev.skill_id)
    .bind(rev.origin.as_str())
    .bind(rev.kind.as_str())
    .bind(&rev.content_digest)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = by_digest {
        let digest: String = row.get("content_digest");
        let content: Vec<u8> = row.get("content");
        if digest == rev.content_digest && content == rev.content {
            let stored_id: String = row.get("revision_id");
            ensure_revision_id_free_or_same(tx, &rev.revision_id, Some(&stored_id)).await?;
            return Ok(SkillRevisionInsert::Existing {
                revision_id: stored_id,
            });
        }
        return Err(StoreError::Conflict(
            "skill revision bytes do not match the stored digest".to_owned(),
        ));
    }

    ensure_revision_id_free_or_same(tx, &rev.revision_id, None).await?;

    sqlx::query(
        "INSERT INTO skill_revision ( \
            revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&rev.revision_id)
    .bind(&rev.skill_id)
    .bind(rev.origin.as_str())
    .bind(&rev.semver)
    .bind(rev.kind.as_str())
    .bind(&rev.content_digest)
    .bind(&rev.content)
    .bind(&rev.created_at)
    .execute(&mut **tx)
    .await?;
    Ok(SkillRevisionInsert::Inserted {
        revision_id: rev.revision_id.clone(),
    })
}

/// Semver first, then digest. An evals digest is the byte SHA-256 and is
/// recomputed here. A body digest is the caller's manifest digest.
async fn adopt_or_insert(
    tx: &mut Transaction<'_, Sqlite>,
    rev: &NewSkillRevision,
) -> Result<SkillRevisionInsert> {
    if rev.kind == SkillRevisionKind::Evals {
        let computed = content_digest_of(&rev.content);
        if computed != rev.content_digest {
            return Err(StoreError::Conflict(
                "skill content_digest does not match content".to_owned(),
            ));
        }
    }
    let by_semver = sqlx::query(
        "SELECT revision_id, content_digest FROM skill_revision \
         WHERE skill_id = ? AND origin = ? AND semver = ? AND kind = ?",
    )
    .bind(&rev.skill_id)
    .bind(rev.origin.as_str())
    .bind(&rev.semver)
    .bind(rev.kind.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = by_semver {
        let digest: String = row.get("content_digest");
        if digest == rev.content_digest {
            let stored_id: String = row.get("revision_id");
            // Before accepting the dedupe hit: the caller's id must not already
            // name some other revision.
            ensure_revision_id_free_or_same(tx, &rev.revision_id, Some(&stored_id)).await?;
            return Ok(SkillRevisionInsert::Existing {
                revision_id: stored_id,
            });
        }
        return Err(StoreError::SkillRevisionConflict);
    }
    let by_digest = sqlx::query(
        "SELECT revision_id FROM skill_revision \
         WHERE skill_id = ? AND origin = ? AND kind = ? AND content_digest = ?",
    )
    .bind(&rev.skill_id)
    .bind(rev.origin.as_str())
    .bind(rev.kind.as_str())
    .bind(&rev.content_digest)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = by_digest {
        let stored_id: String = row.get("revision_id");
        ensure_revision_id_free_or_same(tx, &rev.revision_id, Some(&stored_id)).await?;
        return Ok(SkillRevisionInsert::Existing {
            revision_id: stored_id,
        });
    }
    insert_pair_row(tx, rev).await
}

/// `adopting` is the revision a dedupe hit would return. The requested id may
/// be that same row. Any other stored row with the requested id is
/// [`StoreError::Conflict`], including when the bytes would otherwise dedupe.
async fn ensure_revision_id_free_or_same(
    tx: &mut Transaction<'_, Sqlite>,
    requested: &str,
    adopting: Option<&str>,
) -> Result<()> {
    let taken = sqlx::query("SELECT 1 AS present FROM skill_revision WHERE revision_id = ?")
        .bind(requested)
        .fetch_optional(&mut **tx)
        .await?;
    if taken.is_some() && adopting != Some(requested) {
        return Err(StoreError::Conflict(
            "skill revision_id already stored".to_owned(),
        ));
    }
    Ok(())
}

fn meta_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SkillRevisionMeta> {
    Ok(SkillRevisionMeta {
        revision_id: row.get("revision_id"),
        skill_id: row.get("skill_id"),
        origin: SkillOrigin::parse(&row.get::<String, _>("origin"))?,
        semver: row.get("semver"),
        kind: SkillRevisionKind::parse(&row.get::<String, _>("kind"))?,
        content_digest: row.get("content_digest"),
        created_at: row.get("created_at"),
    })
}

async fn insert_pair_row(
    tx: &mut Transaction<'_, Sqlite>,
    rev: &NewSkillRevision,
) -> Result<SkillRevisionInsert> {
    let taken = sqlx::query("SELECT 1 AS present FROM skill_revision WHERE revision_id = ?")
        .bind(&rev.revision_id)
        .fetch_optional(&mut **tx)
        .await?;
    if taken.is_some() {
        return Err(StoreError::Conflict(
            "skill revision_id already stored".to_owned(),
        ));
    }
    sqlx::query(
        "INSERT INTO skill_revision ( \
            revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&rev.revision_id)
    .bind(&rev.skill_id)
    .bind(rev.origin.as_str())
    .bind(&rev.semver)
    .bind(rev.kind.as_str())
    .bind(&rev.content_digest)
    .bind(&rev.content)
    .bind(&rev.created_at)
    .execute(&mut **tx)
    .await?;
    Ok(SkillRevisionInsert::Inserted {
        revision_id: rev.revision_id.clone(),
    })
}

fn same_bytes_or_conflict(
    row: &sqlx::sqlite::SqliteRow,
    rev: &NewSkillRevision,
) -> Result<SkillRevisionInsert> {
    let digest: String = row.get("content_digest");
    let content: Vec<u8> = row.get("content");
    if digest == rev.content_digest && content == rev.content {
        return Ok(SkillRevisionInsert::Existing {
            revision_id: row.get("revision_id"),
        });
    }
    Err(StoreError::SkillRevisionConflict)
}

async fn read_pointer(
    tx: &mut Transaction<'_, Sqlite>,
    skill_id: &str,
) -> Result<Option<SkillPointer>> {
    let row = sqlx::query(
        "SELECT p.skill_id, p.body_revision_id, p.evals_revision_id, p.generation, \
                b.origin AS body_origin, e.origin AS evals_origin \
         FROM skill_pointer p \
         JOIN skill_revision b ON b.revision_id = p.body_revision_id \
         JOIN skill_revision e ON e.revision_id = p.evals_revision_id \
         WHERE p.skill_id = ?",
    )
    .bind(skill_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|row| pointer_from_row(&row)).transpose()
}

/// `None` means the pointer already names this pair (no write).
///
/// A missing pointer row is generation 0. The read failed earlier, so this
/// `None` is not a failed query. Generation is compared before a mixed move.
fn decide_generation(
    pointer: Option<&SkillPointer>,
    expected_generation: i64,
    body_revision_id: &str,
    evals_revision_id: &str,
) -> Result<Option<i64>> {
    if let Some(pointer) = pointer
        && same_pair(pointer, body_revision_id, evals_revision_id)
    {
        return Ok(None);
    }
    let current = pointer.map(|row| row.generation).unwrap_or(0);
    if current != expected_generation {
        return Err(StoreError::SkillPointerConflict);
    }
    Ok(Some(expected_generation))
}

async fn require_stored_revision(
    tx: &mut Transaction<'_, Sqlite>,
    revision_id: &str,
    skill_id: &str,
    kind: SkillRevisionKind,
) -> Result<StoredRevision> {
    let row = sqlx::query(
        "SELECT skill_id, origin, kind, content_digest FROM skill_revision \
         WHERE revision_id = ?",
    )
    .bind(revision_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Err(StoreError::NotFound(format!(
            "skill revision {revision_id}"
        )));
    };
    let stored_skill: String = row.get("skill_id");
    let stored_kind = SkillRevisionKind::parse(&row.get::<String, _>("kind"))?;
    if stored_skill != skill_id || stored_kind != kind {
        return Err(StoreError::Conflict(
            "skill revision does not match the requested skill and kind".to_owned(),
        ));
    }
    Ok(StoredRevision {
        origin: SkillOrigin::parse(&row.get::<String, _>("origin"))?,
        content_digest: row.get("content_digest"),
    })
}

async fn cas_pointer(
    tx: &mut Transaction<'_, Sqlite>,
    skill_id: &str,
    expected_generation: i64,
    body_revision_id: &str,
    evals_revision_id: &str,
    existed: bool,
) -> Result<i64> {
    if !existed {
        sqlx::query(
            "INSERT INTO skill_pointer ( \
                skill_id, body_revision_id, evals_revision_id, generation \
             ) VALUES (?, ?, ?, 1)",
        )
        .bind(skill_id)
        .bind(body_revision_id)
        .bind(evals_revision_id)
        .execute(&mut **tx)
        .await?;
        return Ok(1);
    }
    let next = expected_generation
        .checked_add(1)
        .ok_or_else(|| StoreError::Conflict("skill generation overflow".to_owned()))?;
    let updated = sqlx::query(
        "UPDATE skill_pointer \
         SET body_revision_id = ?, evals_revision_id = ?, generation = ? \
         WHERE skill_id = ? AND generation = ?",
    )
    .bind(body_revision_id)
    .bind(evals_revision_id)
    .bind(next)
    .bind(skill_id)
    .bind(expected_generation)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(StoreError::SkillPointerConflict);
    }
    Ok(next)
}

async fn insert_eval_result(
    tx: &mut Transaction<'_, Sqlite>,
    binding: &SkillEvalBinding,
    body_revision_id: &str,
    evals_revision_id: &str,
    created_at: &str,
) -> Result<()> {
    require_text("result_id", &binding.result_id)?;
    require_text("segment_digest", &binding.segment_digest)?;
    require_text("renderer_version", &binding.renderer_version)?;
    require_accepted(&binding.cases_json, &binding.accepted_json)?;
    if id_stored(
        tx,
        "SELECT 1 AS present FROM skill_eval_result WHERE result_id = ?",
        &binding.result_id,
    )
    .await?
    {
        return Err(StoreError::Conflict(
            "skill result_id already stored".to_owned(),
        ));
    }
    sqlx::query(
        "INSERT INTO skill_eval_result ( \
            result_id, body_revision_id, evals_revision_id, segment_digest, \
            renderer_version, cases_json, accepted_json, created_at \
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&binding.result_id)
    .bind(body_revision_id)
    .bind(evals_revision_id)
    .bind(&binding.segment_digest)
    .bind(&binding.renderer_version)
    .bind(&binding.cases_json)
    .bind(&binding.accepted_json)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_activation_row(
    tx: &mut Transaction<'_, Sqlite>,
    row: &PointerMove<'_>,
    result_id: &str,
) -> Result<()> {
    if id_stored(
        tx,
        "SELECT 1 AS present FROM skill_activation WHERE activation_id = ?",
        row.activation_id,
    )
    .await?
    {
        return Err(StoreError::Conflict(
            "skill activation_id already stored".to_owned(),
        ));
    }
    sqlx::query(
        "INSERT INTO skill_activation ( \
            activation_id, skill_id, body_revision_id, evals_revision_id, \
            result_id, result_code, created_at \
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(row.activation_id)
    .bind(row.skill_id)
    .bind(row.body_revision_id)
    .bind(row.evals_revision_id)
    .bind(result_id)
    .bind(row.result_code)
    .bind(row.created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The pair is already current, so the move writes nothing. An id that
/// already names another skill or another pair is still a conflict.
/// Rollback supplies no new eval binding, so only `activation_id` is checked
/// on that path.
async fn reject_idempotent_id_reuse(
    tx: &mut Transaction<'_, Sqlite>,
    move_req: &PointerMove<'_>,
) -> Result<()> {
    if let Some(row) = sqlx::query(
        "SELECT skill_id, body_revision_id, evals_revision_id \
         FROM skill_activation WHERE activation_id = ?",
    )
    .bind(move_req.activation_id)
    .fetch_optional(&mut **tx)
    .await?
    {
        let same = row.get::<String, _>("skill_id") == move_req.skill_id
            && row.get::<String, _>("body_revision_id") == move_req.body_revision_id
            && row.get::<String, _>("evals_revision_id") == move_req.evals_revision_id;
        if !same {
            return Err(StoreError::Conflict(
                "skill activation_id already stored".to_owned(),
            ));
        }
    }
    let Some(binding) = move_req.binding else {
        return Ok(());
    };
    if let Some(row) = sqlx::query(
        "SELECT r.body_revision_id, r.evals_revision_id, b.skill_id \
         FROM skill_eval_result r \
         LEFT JOIN skill_revision b ON b.revision_id = r.body_revision_id \
         WHERE r.result_id = ?",
    )
    .bind(&binding.result_id)
    .fetch_optional(&mut **tx)
    .await?
    {
        let body: String = row.get("body_revision_id");
        let evals: String = row.get("evals_revision_id");
        let skill_id: Option<String> = row.get("skill_id");
        let same = skill_id.as_deref() == Some(move_req.skill_id)
            && body == move_req.body_revision_id
            && evals == move_req.evals_revision_id;
        if !same {
            return Err(StoreError::Conflict(
                "skill result_id already stored".to_owned(),
            ));
        }
    }
    Ok(())
}

async fn id_stored(tx: &mut Transaction<'_, Sqlite>, sql: &'static str, id: &str) -> Result<bool> {
    let row = sqlx::query(sql).bind(id).fetch_optional(&mut **tx).await?;
    Ok(row.is_some())
}

async fn audit_move(
    tx: &mut Transaction<'_, Sqlite>,
    row: &PointerMove<'_>,
    content_digest: &str,
    origin_from: Option<SkillOrigin>,
    origin_to: SkillOrigin,
) -> Result<()> {
    let detail = serde_json::json!({
        "skill_id": row.skill_id,
        "body_revision_id": row.body_revision_id,
        "evals_revision_id": row.evals_revision_id,
        "content_digest": content_digest,
        "result_code": row.result_code,
        "origin_from": origin_from.map(SkillOrigin::as_str),
        "origin_to": origin_to.as_str(),
    });
    Store::append_audit_tx(tx, row.created_at, row.actor, row.audit_action, &detail).await?;
    Ok(())
}

async fn move_pointer(
    tx: &mut Transaction<'_, Sqlite>,
    move_req: &PointerMove<'_>,
) -> Result<SkillActivationOutcome> {
    let pointer = read_pointer(tx, move_req.skill_id).await?;
    let expected = decide_generation(
        pointer.as_ref(),
        move_req.expected_generation,
        move_req.body_revision_id,
        move_req.evals_revision_id,
    )?;
    let Some(expected) = expected else {
        reject_idempotent_id_reuse(tx, move_req).await?;
        let generation = pointer
            .as_ref()
            .map(|row| row.generation)
            .ok_or_else(|| StoreError::Conflict("skill pointer missing".to_owned()))?;
        return Ok(SkillActivationOutcome::Unchanged { generation });
    };
    let body = require_stored_revision(
        tx,
        move_req.body_revision_id,
        move_req.skill_id,
        SkillRevisionKind::Body,
    )
    .await?;
    let evals = require_stored_revision(
        tx,
        move_req.evals_revision_id,
        move_req.skill_id,
        SkillRevisionKind::Evals,
    )
    .await?;
    if body.origin != evals.origin {
        return Err(StoreError::SkillOriginMismatch);
    }
    if let Some(pointer) = pointer.as_ref() {
        let body_changed = pointer.body_revision_id != move_req.body_revision_id;
        let evals_changed = pointer.evals_revision_id != move_req.evals_revision_id;
        // Same-origin moves still change one side at a time. A different
        // shared origin is a source switch and is allowed, like the first pair.
        let source_switch = pointer.origin != body.origin;
        if body_changed && evals_changed && !move_req.allow_both_sides && !source_switch {
            return Err(StoreError::SkillActivationMixed);
        }
    }
    let result_id = match move_req.binding {
        Some(binding) => {
            insert_eval_result(
                tx,
                binding,
                move_req.body_revision_id,
                move_req.evals_revision_id,
                move_req.created_at,
            )
            .await?;
            binding.result_id.as_str()
        }
        None => move_req.result_id,
    };
    insert_activation_row(tx, move_req, result_id).await?;
    let generation = cas_pointer(
        tx,
        move_req.skill_id,
        expected,
        move_req.body_revision_id,
        move_req.evals_revision_id,
        pointer.is_some(),
    )
    .await?;
    audit_move(
        tx,
        move_req,
        &body.content_digest,
        pointer.as_ref().map(|row| row.origin),
        body.origin,
    )
    .await?;
    Ok(SkillActivationOutcome::Activated {
        generation,
        activation_id: move_req.activation_id.to_owned(),
    })
}

async fn load_activation(
    tx: &mut Transaction<'_, Sqlite>,
    activation_id: &str,
) -> Result<StoredActivation> {
    let row = sqlx::query(
        "SELECT skill_id, body_revision_id, evals_revision_id, result_id \
         FROM skill_activation WHERE activation_id = ?",
    )
    .bind(activation_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Err(StoreError::NotFound(format!(
            "skill activation {activation_id}"
        )));
    };
    Ok(StoredActivation {
        skill_id: row.get("skill_id"),
        body_revision_id: row.get("body_revision_id"),
        evals_revision_id: row.get("evals_revision_id"),
        result_id: row.get("result_id"),
    })
}

async fn rollback_tx(
    tx: &mut Transaction<'_, Sqlite>,
    req: &SkillRollback,
) -> Result<SkillActivationOutcome> {
    let source = load_activation(tx, &req.source_activation_id).await?;
    if source.skill_id != req.skill_id {
        return Err(StoreError::Conflict(
            "skill activation belongs to another skill".to_owned(),
        ));
    }
    move_pointer(
        tx,
        &PointerMove {
            activation_id: &req.activation_id,
            skill_id: &req.skill_id,
            expected_generation: req.expected_generation,
            body_revision_id: &source.body_revision_id,
            evals_revision_id: &source.evals_revision_id,
            result_id: &source.result_id,
            result_code: CODE_ROLLED_BACK,
            created_at: &req.created_at,
            actor: &req.actor,
            audit_action: AUDIT_ROLLBACK,
            allow_both_sides: true,
            binding: None,
        },
    )
    .await
}

fn pointer_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SkillPointer> {
    let body_origin = SkillOrigin::parse(&row.get::<String, _>("body_origin"))?;
    let evals_origin = SkillOrigin::parse(&row.get::<String, _>("evals_origin"))?;
    if body_origin != evals_origin {
        return Err(StoreError::SkillOriginMismatch);
    }
    Ok(SkillPointer {
        skill_id: row.get("skill_id"),
        origin: body_origin,
        body_revision_id: row.get("body_revision_id"),
        evals_revision_id: row.get("evals_revision_id"),
        generation: row.get("generation"),
    })
}

fn revision_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SkillRevision> {
    Ok(SkillRevision {
        revision_id: row.get("revision_id"),
        skill_id: row.get("skill_id"),
        origin: SkillOrigin::parse(&row.get::<String, _>("origin"))?,
        semver: row.get("semver"),
        kind: SkillRevisionKind::parse(&row.get::<String, _>("kind"))?,
        content_digest: row.get("content_digest"),
        content: row.get("content"),
        created_at: row.get("created_at"),
    })
}

fn activation_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SkillActivationRecord> {
    let result_code: String = row.get("result_code");
    if result_code != CODE_ACTIVATED && result_code != CODE_ROLLED_BACK {
        return Err(StoreError::Conflict(format!(
            "skill result_code {result_code}"
        )));
    }
    let origin = SkillOrigin::parse(&row.get::<String, _>("origin"))?;
    Ok(SkillActivationRecord {
        seq: row.get("seq"),
        activation_id: row.get("activation_id"),
        skill_id: row.get("skill_id"),
        origin,
        origin_from: None,
        origin_to: origin,
        body_revision_id: row.get("body_revision_id"),
        evals_revision_id: row.get("evals_revision_id"),
        result_id: row.get("result_id"),
        segment_digest: row.get("segment_digest"),
        renderer_version: row.get("renderer_version"),
        cases_json: row.get("cases_json"),
        accepted_json: row.get("accepted_json"),
        result_code,
        created_at: row.get("created_at"),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, unused_must_use)]

    use std::str::FromStr;

    use sqlx::Row;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    use super::*;
    use crate::test_hooks;

    fn revision(
        revision_id: &str,
        skill_id: &str,
        origin: SkillOrigin,
        semver: &str,
        kind: SkillRevisionKind,
        _digest: &str,
        content: &[u8],
    ) -> NewSkillRevision {
        NewSkillRevision {
            revision_id: revision_id.to_owned(),
            skill_id: skill_id.to_owned(),
            origin,
            semver: semver.to_owned(),
            kind,
            content_digest: content_digest_of(content),
            content: content.to_vec(),
            created_at: "2026-11-12T00:00:00Z".to_owned(),
        }
    }

    fn binding(result_id: &str, digest: &str) -> SkillEvalBinding {
        SkillEvalBinding {
            result_id: result_id.to_owned(),
            segment_digest: digest.to_owned(),
            renderer_version: "skill-render-1".to_owned(),
            cases_json: r#"[{"id":"e1:1","passed":true}]"#.to_owned(),
            accepted_json: "[]".to_owned(),
        }
    }

    fn activation(
        activation_id: &str,
        skill_id: &str,
        expected: i64,
        body: &str,
        evals: &str,
        result_id: &str,
        created_at: &str,
    ) -> NewSkillActivation {
        NewSkillActivation {
            activation_id: activation_id.to_owned(),
            skill_id: skill_id.to_owned(),
            expected_generation: expected,
            body_revision_id: body.to_owned(),
            evals_revision_id: evals.to_owned(),
            binding: binding(result_id, "segment"),
            created_at: created_at.to_owned(),
            actor: "host".to_owned(),
        }
    }

    async fn put(store: &Store, rev: &NewSkillRevision) {
        store.insert_skill_revision(rev).await.unwrap();
    }

    async fn audit_details(store: &Store, action: &str) -> Vec<String> {
        let rows = sqlx::query("SELECT detail FROM audit_log WHERE action = ? ORDER BY seq ASC")
            .bind(action)
            .fetch_all(test_hooks::pool(store))
            .await
            .unwrap();
        rows.into_iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect()
    }

    async fn revision_count(store: &Store) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM skill_revision")
            .fetch_one(test_hooks::pool(store))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn same_digest_returns_the_existing_revision_id() {
        let store = Store::open_memory().await.unwrap();
        let first = revision(
            "rev-body-1",
            "notes",
            SkillOrigin::User,
            "1.0.0",
            SkillRevisionKind::Body,
            "digest-a",
            b"hello skill",
        );
        let inserted = store.insert_skill_revision(&first).await.unwrap();
        assert_eq!(
            inserted,
            SkillRevisionInsert::Inserted {
                revision_id: "rev-body-1".to_owned()
            }
        );

        let mut again = first.clone();
        again.revision_id = "rev-body-other".to_owned();
        again.created_at = "2026-11-12T01:00:00Z".to_owned();
        let existing = store.insert_skill_revision(&again).await.unwrap();
        assert_eq!(
            existing,
            SkillRevisionInsert::Existing {
                revision_id: "rev-body-1".to_owned()
            }
        );
        let stored = store
            .get_skill_revision("rev-body-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.content, b"hello skill");
        assert_eq!(stored.created_at, "2026-11-12T00:00:00Z");
        assert_eq!(revision_count(&store).await, 1);

        let mut flipped = first.clone();
        flipped.revision_id = "rev-body-flipped".to_owned();
        flipped.content = b"hello skill!".to_vec();
        flipped.content_digest = content_digest_of(&flipped.content);
        let err = store.insert_skill_revision(&flipped).await.unwrap_err();
        assert!(matches!(err, StoreError::SkillRevisionConflict), "{err:?}");
        assert_eq!(
            store
                .get_skill_revision("rev-body-1")
                .await
                .unwrap()
                .unwrap()
                .content,
            b"hello skill"
        );
    }

    #[tokio::test]
    async fn semver_bump_with_the_same_digest_returns_the_existing_revision() {
        let store = Store::open_memory().await.unwrap();
        let first = revision(
            "rev-body-1",
            "notes",
            SkillOrigin::User,
            "1.0.0",
            SkillRevisionKind::Body,
            "digest-a",
            b"hello skill",
        );
        put(&store, &first).await;
        let mut bumped = first.clone();
        bumped.revision_id = "rev-body-2".to_owned();
        bumped.semver = "1.1.0".to_owned();
        let existing = store.insert_skill_revision(&bumped).await.unwrap();
        assert_eq!(
            existing,
            SkillRevisionInsert::Existing {
                revision_id: "rev-body-1".to_owned()
            }
        );
        assert_eq!(revision_count(&store).await, 1);
        let stored = store
            .get_skill_revision("rev-body-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.semver, "1.0.0");
        assert!(
            store
                .get_skill_revision("rev-body-2")
                .await
                .unwrap()
                .is_none()
        );

        let mut collided = bumped.clone();
        collided.revision_id = "rev-body-3".to_owned();
        collided.content = b"other bytes".to_vec();
        let err = store.insert_skill_revision(&collided).await.unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message.contains("content_digest")),
            "{err:?}"
        );
        assert_eq!(revision_count(&store).await, 1);
    }

    #[tokio::test]
    async fn same_semver_different_digest_keeps_the_first_row() {
        let store = Store::open_memory().await.unwrap();
        let first = revision(
            "rev-body-1",
            "notes",
            SkillOrigin::Imported,
            "1.2.3",
            SkillRevisionKind::Body,
            "digest-a",
            b"first body",
        );
        put(&store, &first).await;
        let mut second = first.clone();
        second.revision_id = "rev-body-2".to_owned();
        second.content = b"second body".to_vec();
        second.content_digest = content_digest_of(&second.content);
        let err = store.insert_skill_revision(&second).await.unwrap_err();
        assert_eq!(err.to_string(), "skill_revision_conflict");
        let stored = store
            .get_skill_revision("rev-body-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.content, b"first body");
        assert_eq!(stored.content_digest, content_digest_of(b"first body"));
        assert!(
            store
                .get_skill_revision("rev-body-2")
                .await
                .unwrap()
                .is_none()
        );
        put(
            &store,
            &revision(
                "rev-evals-1",
                "notes",
                SkillOrigin::Imported,
                "1.2.3",
                SkillRevisionKind::Evals,
                "digest-e",
                b"",
            ),
        )
        .await;
        assert_eq!(revision_count(&store).await, 2);
    }

    #[tokio::test]
    async fn different_origins_share_a_name_and_version() {
        let store = Store::open_memory().await.unwrap();
        let user = revision(
            "user-body",
            "notes",
            SkillOrigin::User,
            "0.1.0",
            SkillRevisionKind::Body,
            "same-digest",
            b"same bytes",
        );
        let mut imported = user.clone();
        imported.revision_id = "imported-body".to_owned();
        imported.origin = SkillOrigin::Imported;
        put(&store, &user).await;
        assert!(matches!(
            store.insert_skill_revision(&imported).await.unwrap(),
            SkillRevisionInsert::Inserted { .. }
        ));
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());
        assert_eq!(revision_count(&store).await, 2);
    }

    #[tokio::test]
    async fn revision_id_already_stored_is_a_conflict() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "rev-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "d1",
                b"one",
            ),
        )
        .await;
        let err = store
            .insert_skill_revision(&revision(
                "rev-1",
                "notes",
                SkillOrigin::User,
                "2.0.0",
                SkillRevisionKind::Evals,
                "d2",
                b"two",
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message.contains("revision_id")),
            "{err:?}"
        );
        assert_eq!(revision_count(&store).await, 1);
    }

    #[tokio::test]
    async fn pointer_cas_mismatch_is_skill_pointer_conflict() {
        let store = Store::open_memory().await.unwrap();
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());
        put(
            &store,
            &revision(
                "body-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "b1",
                b"SECRET-BODY-BYTES",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "body-2",
                "notes",
                SkillOrigin::User,
                "1.1.0",
                SkillRevisionKind::Body,
                "b2",
                b"next body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "e1",
                b"cases",
            ),
        )
        .await;
        let first = store
            .activate_skill(&activation(
                "act-1",
                "notes",
                0,
                "body-1",
                "evals-1",
                "result-1",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap();
        assert_eq!(
            first,
            SkillActivationOutcome::Activated {
                generation: 1,
                activation_id: "act-1".to_owned()
            }
        );
        let err = store
            .activate_skill(&activation(
                "act-stale",
                "notes",
                0,
                "body-2",
                "evals-1",
                "result-stale",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "skill_pointer_conflict");
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.generation, 1);
        assert_eq!(pointer.origin, SkillOrigin::User);
        assert_eq!(pointer.body_revision_id, "body-1");
        assert_eq!(
            store.list_skill_activations("notes").await.unwrap().len(),
            1
        );
        let details = audit_details(&store, AUDIT_ACTIVATE).await;
        assert_eq!(details.len(), 1);
        assert!(!details[0].contains("SECRET-BODY-BYTES"));
        assert!(details[0].contains(&format!(
            "\"content_digest\":\"{}\"",
            content_digest_of(b"SECRET-BODY-BYTES")
        )));
        assert!(details[0].contains("\"result_code\":\"activated\""));

        let repeat = store
            .activate_skill(&activation(
                "act-repeat",
                "notes",
                0,
                "body-1",
                "evals-1",
                "result-repeat",
                "2026-11-12T00:00:03Z",
            ))
            .await
            .unwrap();
        assert_eq!(repeat, SkillActivationOutcome::Unchanged { generation: 1 });
        assert_eq!(
            store.list_skill_activations("notes").await.unwrap().len(),
            1
        );
        assert_eq!(audit_details(&store, AUDIT_ACTIVATE).await.len(), 1);

        let moved = store
            .activate_skill(&activation(
                "act-2",
                "notes",
                1,
                "body-2",
                "evals-1",
                "result-2",
                "2026-11-12T00:00:04Z",
            ))
            .await
            .unwrap();
        assert_eq!(
            moved,
            SkillActivationOutcome::Activated {
                generation: 2,
                activation_id: "act-2".to_owned()
            }
        );
        put(
            &store,
            &revision(
                "evals-2",
                "notes",
                SkillOrigin::User,
                "1.1.0",
                SkillRevisionKind::Evals,
                "e2",
                b"more cases",
            ),
        )
        .await;
        let err = store
            .activate_skill(&activation(
                "act-mixed",
                "notes",
                2,
                "body-1",
                "evals-2",
                "result-mixed",
                "2026-11-12T00:00:06Z",
            ))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "skill_activation_mixed");
        assert_eq!(
            store
                .get_skill_pointer("notes")
                .await
                .unwrap()
                .unwrap()
                .body_revision_id,
            "body-2"
        );
        assert_eq!(
            store.list_skill_activations("notes").await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn missing_pointer_rejects_a_nonzero_generation() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "body-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "b1",
                b"body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "e1",
                b"evals",
            ),
        )
        .await;
        let err = store
            .activate_skill(&activation(
                "act-7",
                "notes",
                7,
                "body-1",
                "evals-1",
                "result-7",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "skill_pointer_conflict");
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());
        assert!(
            store
                .list_skill_activations("notes")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn stale_generation_beats_a_mixed_move() {
        let store = Store::open_memory().await.unwrap();
        for (id, semver, kind, digest) in [
            ("body-1", "1.0.0", SkillRevisionKind::Body, "b1"),
            ("body-2", "1.1.0", SkillRevisionKind::Body, "b2"),
            ("evals-1", "1.0.0", SkillRevisionKind::Evals, "e1"),
            ("evals-2", "1.1.0", SkillRevisionKind::Evals, "e2"),
        ] {
            put(
                &store,
                &revision(
                    id,
                    "notes",
                    SkillOrigin::User,
                    semver,
                    kind,
                    digest,
                    id.as_bytes(),
                ),
            )
            .await;
        }
        store
            .activate_skill(&activation(
                "act-1",
                "notes",
                0,
                "body-1",
                "evals-1",
                "result-1",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap();
        let err = store
            .activate_skill(&activation(
                "act-stale-mixed",
                "notes",
                0,
                "body-2",
                "evals-2",
                "result-stale",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "skill_pointer_conflict");
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.body_revision_id, "body-1");
        assert_eq!(pointer.evals_revision_id, "evals-1");
        assert_eq!(pointer.generation, 1);
    }

    #[tokio::test]
    async fn swapped_kinds_and_another_skills_revision_leave_the_pointer() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "body-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "b1",
                b"body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "e1",
                b"evals",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "other-body",
                "other",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "ob",
                b"other",
            ),
        )
        .await;
        let swapped = store
            .activate_skill(&activation(
                "act-swap",
                "notes",
                0,
                "evals-1",
                "body-1",
                "result-swap",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap_err();
        assert!(matches!(swapped, StoreError::Conflict(_)), "{swapped:?}");
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());

        store
            .activate_skill(&activation(
                "act-1",
                "notes",
                0,
                "body-1",
                "evals-1",
                "result-1",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap();
        let foreign = store
            .activate_skill(&activation(
                "act-foreign",
                "notes",
                1,
                "other-body",
                "evals-1",
                "result-foreign",
                "2026-11-12T00:00:03Z",
            ))
            .await
            .unwrap_err();
        assert!(matches!(foreign, StoreError::Conflict(_)), "{foreign:?}");
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.body_revision_id, "body-1");
        assert_eq!(pointer.evals_revision_id, "evals-1");
        assert_eq!(pointer.generation, 1);
        assert_eq!(
            store.list_skill_activations("notes").await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn accepted_ids_must_be_failed_cases_of_this_binding() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "body-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "b1",
                b"body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "e1",
                b"evals",
            ),
        )
        .await;
        let mut req = activation(
            "act-bad",
            "notes",
            0,
            "body-1",
            "evals-1",
            "result-bad",
            "2026-11-12T00:00:01Z",
        );
        req.binding.cases_json =
            r#"[{"id":"ok","passed":true},{"id":"bad","passed":false}]"#.to_owned();
        req.binding.accepted_json = r#"["ok"]"#.to_owned();
        let err = store.activate_skill(&req).await.unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message.contains("failed case")),
            "{err:?}"
        );
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());

        req.activation_id = "act-ok".to_owned();
        req.binding.result_id = "result-ok".to_owned();
        req.binding.accepted_json = r#"["bad"]"#.to_owned();
        store.activate_skill(&req).await.unwrap();
        let history = store.list_skill_activations("notes").await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].accepted_json, r#"["bad"]"#);
        assert_eq!(history[0].origin, SkillOrigin::User);
    }

    #[tokio::test]
    async fn origin_mismatch_is_rejected_on_activate_and_rollback() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "body-user",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "bu",
                b"user body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-imported",
                "notes",
                SkillOrigin::Imported,
                "1.0.0",
                SkillRevisionKind::Evals,
                "ei",
                b"imported evals",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-user",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "eu",
                b"user evals",
            ),
        )
        .await;
        let err = store
            .activate_skill(&activation(
                "act-mix",
                "notes",
                0,
                "body-user",
                "evals-imported",
                "result-mix",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "skill_origin_mismatch");
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());

        store
            .activate_skill(&activation(
                "act-1",
                "notes",
                0,
                "body-user",
                "evals-user",
                "result-1",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO skill_eval_result ( \
                result_id, body_revision_id, evals_revision_id, segment_digest, \
                renderer_version, cases_json, accepted_json, created_at \
             ) VALUES ('result-cross', 'body-user', 'evals-imported', 'seg', 'r1', '[]', '[]', \
                       '2026-11-12T00:00:03Z')",
        )
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO skill_activation ( \
                activation_id, skill_id, body_revision_id, evals_revision_id, \
                result_id, result_code, created_at \
             ) VALUES ('act-cross', 'notes', 'body-user', 'evals-imported', 'result-cross', \
                       'activated', '2026-11-12T00:00:03Z')",
        )
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
        let err = store
            .rollback_skill(&SkillRollback {
                activation_id: "act-back".to_owned(),
                skill_id: "notes".to_owned(),
                expected_generation: 1,
                source_activation_id: "act-cross".to_owned(),
                created_at: "2026-11-12T00:00:04Z".to_owned(),
                actor: "host".to_owned(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "skill_origin_mismatch");
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.body_revision_id, "body-user");
        assert_eq!(pointer.evals_revision_id, "evals-user");
        assert_eq!(pointer.origin, SkillOrigin::User);
        assert_eq!(pointer.generation, 1);
    }

    #[tokio::test]
    async fn missing_revision_and_rollback_source_are_not_found() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "evals-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "e1",
                b"evals",
            ),
        )
        .await;
        let missing = store
            .activate_skill(&activation(
                "act-missing",
                "notes",
                0,
                "body-missing",
                "evals-1",
                "result-missing",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(missing, StoreError::NotFound(ref message) if message.contains("body-missing")),
            "{missing:?}"
        );
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());

        put(
            &store,
            &revision(
                "body-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "b1",
                b"body",
            ),
        )
        .await;
        store
            .activate_skill(&activation(
                "act-1",
                "notes",
                0,
                "body-1",
                "evals-1",
                "result-1",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap();
        let absent = store
            .rollback_skill(&SkillRollback {
                activation_id: "act-nope".to_owned(),
                skill_id: "notes".to_owned(),
                expected_generation: 1,
                source_activation_id: "act-does-not-exist".to_owned(),
                created_at: "2026-11-12T00:00:03Z".to_owned(),
                actor: "host".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(absent, StoreError::NotFound(ref message) if message.contains("act-does-not-exist")),
            "{absent:?}"
        );

        put(
            &store,
            &revision(
                "other-body",
                "other",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "ob",
                b"other",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "other-evals",
                "other",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "oe",
                b"other evals",
            ),
        )
        .await;
        store
            .activate_skill(&activation(
                "act-other",
                "other",
                0,
                "other-body",
                "other-evals",
                "result-other",
                "2026-11-12T00:00:04Z",
            ))
            .await
            .unwrap();
        let foreign = store
            .rollback_skill(&SkillRollback {
                activation_id: "act-steal".to_owned(),
                skill_id: "notes".to_owned(),
                expected_generation: 1,
                source_activation_id: "act-other".to_owned(),
                created_at: "2026-11-12T00:00:05Z".to_owned(),
                actor: "host".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(foreign, StoreError::Conflict(ref message) if message.contains("another skill")),
            "{foreign:?}"
        );
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.body_revision_id, "body-1");
        assert_eq!(pointer.generation, 1);
    }

    #[tokio::test]
    async fn activation_rolls_back_when_the_audit_insert_fails() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "body-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "b1",
                b"body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "e1",
                b"evals",
            ),
        )
        .await;
        sqlx::query(
            "CREATE TRIGGER skill_audit_fail BEFORE INSERT ON audit_log \
             WHEN NEW.action = 'skill.activate' \
             BEGIN SELECT RAISE(ABORT, 'boom'); END",
        )
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
        let err = store
            .activate_skill(&activation(
                "act-1",
                "notes",
                0,
                "body-1",
                "evals-1",
                "result-1",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Sqlx(_)), "{err:?}");
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());
        assert!(
            store
                .list_skill_activations("notes")
                .await
                .unwrap()
                .is_empty()
        );
        let results: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_eval_result")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(results, 0);
    }

    #[tokio::test]
    async fn rollback_restores_the_pair_and_keeps_the_binding() {
        let store = Store::open_memory().await.unwrap();
        for (id, semver, kind, digest, bytes) in [
            (
                "body-1",
                "1.0.0",
                SkillRevisionKind::Body,
                "b1",
                &b"one"[..],
            ),
            (
                "body-2",
                "1.1.0",
                SkillRevisionKind::Body,
                "b2",
                &b"two"[..],
            ),
            (
                "evals-1",
                "1.0.0",
                SkillRevisionKind::Evals,
                "e1",
                &b"ev"[..],
            ),
        ] {
            put(
                &store,
                &revision(
                    id,
                    "notes",
                    SkillOrigin::Imported,
                    semver,
                    kind,
                    digest,
                    bytes,
                ),
            )
            .await;
        }
        store
            .activate_skill(&activation(
                "act-1",
                "notes",
                0,
                "body-1",
                "evals-1",
                "result-1",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap();
        store
            .activate_skill(&activation(
                "act-2",
                "notes",
                1,
                "body-2",
                "evals-1",
                "result-2",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap();
        let rolled = store
            .rollback_skill(&SkillRollback {
                activation_id: "act-3".to_owned(),
                skill_id: "notes".to_owned(),
                expected_generation: 2,
                source_activation_id: "act-1".to_owned(),
                created_at: "2026-11-12T00:00:03Z".to_owned(),
                actor: "host".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(
            rolled,
            SkillActivationOutcome::Activated {
                generation: 3,
                activation_id: "act-3".to_owned()
            }
        );
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.origin, SkillOrigin::Imported);
        assert_eq!(pointer.body_revision_id, "body-1");
        assert_eq!(pointer.evals_revision_id, "evals-1");
        let history = store.list_skill_activations("notes").await.unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[2].result_id, "result-1");
        assert_eq!(history[2].result_code, "rolled_back");
        assert_eq!(history[2].origin, SkillOrigin::Imported);
        assert_eq!(history[0].segment_digest, "segment");
        assert!(history[0].seq < history[1].seq && history[1].seq < history[2].seq);
        let results: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_eval_result")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(results, 2);
        let again = store
            .rollback_skill(&SkillRollback {
                activation_id: "act-4".to_owned(),
                skill_id: "notes".to_owned(),
                expected_generation: 1,
                source_activation_id: "act-1".to_owned(),
                created_at: "2026-11-12T00:00:04Z".to_owned(),
                actor: "host".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(again, SkillActivationOutcome::Unchanged { generation: 3 });
        assert_eq!(
            store.list_skill_activations("notes").await.unwrap().len(),
            3
        );
        assert_eq!(audit_details(&store, AUDIT_ROLLBACK).await.len(), 1);
    }

    #[tokio::test]
    async fn rollback_restores_a_pair_that_differs_on_both_sides() {
        let store = Store::open_memory().await.unwrap();
        for (id, semver, kind, digest) in [
            ("body-1", "1.0.0", SkillRevisionKind::Body, "b1"),
            ("body-2", "1.1.0", SkillRevisionKind::Body, "b2"),
            ("evals-1", "1.0.0", SkillRevisionKind::Evals, "e1"),
            ("evals-2", "1.1.0", SkillRevisionKind::Evals, "e2"),
        ] {
            put(
                &store,
                &revision(
                    id,
                    "notes",
                    SkillOrigin::User,
                    semver,
                    kind,
                    digest,
                    id.as_bytes(),
                ),
            )
            .await;
        }
        store
            .activate_skill(&activation(
                "act-1",
                "notes",
                0,
                "body-1",
                "evals-1",
                "result-1",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap();
        store
            .activate_skill(&activation(
                "act-2",
                "notes",
                1,
                "body-2",
                "evals-1",
                "result-2",
                "2026-11-12T00:00:03Z",
            ))
            .await
            .unwrap();
        store
            .activate_skill(&activation(
                "act-3",
                "notes",
                2,
                "body-2",
                "evals-2",
                "result-3",
                "2026-11-12T00:00:04Z",
            ))
            .await
            .unwrap();
        let rolled = store
            .rollback_skill(&SkillRollback {
                activation_id: "act-4".to_owned(),
                skill_id: "notes".to_owned(),
                expected_generation: 3,
                source_activation_id: "act-1".to_owned(),
                created_at: "2026-11-12T00:00:01Z".to_owned(),
                actor: "host".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(
            rolled,
            SkillActivationOutcome::Activated {
                generation: 4,
                activation_id: "act-4".to_owned()
            }
        );
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.body_revision_id, "body-1");
        assert_eq!(pointer.evals_revision_id, "evals-1");
        assert_eq!(pointer.origin, SkillOrigin::User);
        let history = store.list_skill_activations("notes").await.unwrap();
        assert_eq!(history[0].activation_id, "act-1");
        assert_eq!(history[3].activation_id, "act-4");
        assert_eq!(history[3].result_code, "rolled_back");
        assert!(history[3].created_at < history[0].created_at);
        assert!(history[0].seq < history[3].seq);
        let results: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_eval_result")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(results, 3);
    }

    #[tokio::test]
    async fn concurrent_activations_on_two_connections_let_one_win() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let store = test_hooks::from_pool(pool);
        put(
            &store,
            &revision(
                "body-a",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "ba",
                b"A",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "body-b",
                "notes",
                SkillOrigin::User,
                "1.1.0",
                SkillRevisionKind::Body,
                "bb",
                b"B",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "e",
                b"E",
            ),
        )
        .await;
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let run = |activation_id: &'static str, body: &'static str| {
            let (store, barrier) = (store.clone(), barrier.clone());
            tokio::spawn(async move {
                barrier.wait().await;
                store
                    .activate_skill(&activation(
                        activation_id,
                        "notes",
                        0,
                        body,
                        "evals-1",
                        activation_id,
                        "2026-11-12T00:00:01Z",
                    ))
                    .await
            })
        };
        let (left, right) = tokio::join!(run("act-a", "body-a"), run("act-b", "body-b"));
        let results = [left.unwrap(), right.unwrap()];
        let wins = results.iter().filter(|result| result.is_ok()).count();
        let conflicts = results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::SkillPointerConflict)))
            .count();
        assert_eq!(wins, 1, "{results:?}");
        assert_eq!(conflicts, 1, "{results:?}");
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.generation, 1);
        assert!(pointer.body_revision_id == "body-a" || pointer.body_revision_id == "body-b");
        assert_eq!(
            store.list_skill_activations("notes").await.unwrap().len(),
            1
        );
        assert_eq!(audit_details(&store, AUDIT_ACTIVATE).await.len(), 1);
    }

    #[tokio::test]
    async fn digest_lookup_includes_kind() {
        let store = Store::open_memory().await.unwrap();
        let bytes = b"shared text";
        put(
            &store,
            &revision(
                "rev-body",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "ignored",
                bytes,
            ),
        )
        .await;
        let evals = revision(
            "rev-evals",
            "notes",
            SkillOrigin::User,
            "1.0.0",
            SkillRevisionKind::Evals,
            "ignored",
            bytes,
        );
        let inserted = store.insert_skill_revision(&evals).await.unwrap();
        assert_eq!(
            inserted,
            SkillRevisionInsert::Inserted {
                revision_id: "rev-evals".to_owned()
            }
        );
        assert_eq!(revision_count(&store).await, 2);
        assert_eq!(
            store
                .get_skill_revision("rev-body")
                .await
                .unwrap()
                .unwrap()
                .kind,
            SkillRevisionKind::Body
        );
    }

    #[tokio::test]
    async fn stored_digest_bytes_must_match_the_row() {
        let store = Store::open_memory().await.unwrap();
        let fresh = b"fresh bytes";
        let digest = content_digest_of(fresh);
        sqlx::query(
            "INSERT INTO skill_revision ( \
                revision_id, skill_id, origin, semver, kind, content_digest, content, created_at \
             ) VALUES ('rev-forged', 'notes', 'user', '9.0.0', 'body', ?, 'stale', \
                       '2026-11-12T00:00:00Z')",
        )
        .bind(&digest)
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
        let err = store
            .insert_skill_revision(&revision(
                "rev-fresh",
                "notes",
                SkillOrigin::User,
                "9.1.0",
                SkillRevisionKind::Body,
                &digest,
                fresh,
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message.contains("stored digest")),
            "{err:?}"
        );
        assert_eq!(revision_count(&store).await, 1);
    }

    #[tokio::test]
    async fn source_switch_allows_both_sides_and_same_source_stays_mixed() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "imp-body",
                "notes",
                SkillOrigin::Imported,
                "1.0.0",
                SkillRevisionKind::Body,
                "ib",
                b"imported body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "imp-evals",
                "notes",
                SkillOrigin::Imported,
                "1.0.0",
                SkillRevisionKind::Evals,
                "ie",
                b"imported evals",
            ),
        )
        .await;
        store
            .activate_skill(&activation(
                "act-imp",
                "notes",
                0,
                "imp-body",
                "imp-evals",
                "result-imp",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap();
        put(
            &store,
            &revision(
                "user-body",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "ub",
                b"user body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "user-evals",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "ue",
                b"user evals",
            ),
        )
        .await;
        let switched = store
            .activate_skill(&activation(
                "act-user",
                "notes",
                1,
                "user-body",
                "user-evals",
                "result-user",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap();
        assert_eq!(
            switched,
            SkillActivationOutcome::Activated {
                generation: 2,
                activation_id: "act-user".to_owned()
            }
        );
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.origin, SkillOrigin::User);
        assert_eq!(pointer.body_revision_id, "user-body");
        assert_eq!(pointer.evals_revision_id, "user-evals");
        let history = store.list_skill_activations("notes").await.unwrap();
        assert_eq!(history[0].origin_from, None);
        assert_eq!(history[0].origin_to, SkillOrigin::Imported);
        assert_eq!(history[1].origin_from, Some(SkillOrigin::Imported));
        assert_eq!(history[1].origin_to, SkillOrigin::User);
        let details = audit_details(&store, AUDIT_ACTIVATE).await;
        assert!(details[0].contains("\"origin_from\":null"));
        assert!(details[0].contains("\"origin_to\":\"imported\""));
        assert!(details[1].contains("\"origin_from\":\"imported\""));
        assert!(details[1].contains("\"origin_to\":\"user\""));

        put(
            &store,
            &revision(
                "tasks-user-body",
                "tasks",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "tb",
                b"tasks user body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "tasks-user-evals",
                "tasks",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "te",
                b"tasks user evals",
            ),
        )
        .await;
        store
            .activate_skill(&activation(
                "act-tasks-user",
                "tasks",
                0,
                "tasks-user-body",
                "tasks-user-evals",
                "result-tasks-user",
                "2026-11-12T00:00:03Z",
            ))
            .await
            .unwrap();
        put(
            &store,
            &revision(
                "tasks-imp-body",
                "tasks",
                SkillOrigin::Imported,
                "1.0.0",
                SkillRevisionKind::Body,
                "tib",
                b"tasks imported body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "tasks-imp-evals",
                "tasks",
                SkillOrigin::Imported,
                "1.0.0",
                SkillRevisionKind::Evals,
                "tie",
                b"tasks imported evals",
            ),
        )
        .await;
        store
            .activate_skill(&activation(
                "act-tasks-imp",
                "tasks",
                1,
                "tasks-imp-body",
                "tasks-imp-evals",
                "result-tasks-imp",
                "2026-11-12T00:00:04Z",
            ))
            .await
            .unwrap();
        let reverse = store.list_skill_activations("tasks").await.unwrap();
        assert_eq!(reverse[1].origin_from, Some(SkillOrigin::User));
        assert_eq!(reverse[1].origin_to, SkillOrigin::Imported);
        assert_eq!(
            store
                .get_skill_pointer("tasks")
                .await
                .unwrap()
                .unwrap()
                .origin,
            SkillOrigin::Imported
        );

        put(
            &store,
            &revision(
                "user-body-2",
                "notes",
                SkillOrigin::User,
                "1.1.0",
                SkillRevisionKind::Body,
                "ub2",
                b"user body two",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "user-evals-2",
                "notes",
                SkillOrigin::User,
                "1.1.0",
                SkillRevisionKind::Evals,
                "ue2",
                b"user evals two",
            ),
        )
        .await;
        let mixed = store
            .activate_skill(&activation(
                "act-mixed",
                "notes",
                2,
                "user-body-2",
                "user-evals-2",
                "result-mixed",
                "2026-11-12T00:00:05Z",
            ))
            .await
            .unwrap_err();
        assert_eq!(mixed.to_string(), "skill_activation_mixed");
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.body_revision_id, "user-body");
        assert_eq!(pointer.evals_revision_id, "user-evals");
        assert_eq!(pointer.generation, 2);
        assert_eq!(
            store.list_skill_activations("notes").await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn eval_failures_must_all_be_accepted_and_empty_cases_are_refused() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "body-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "b1",
                b"body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "e1",
                b"evals",
            ),
        )
        .await;
        let mut req = activation(
            "act-empty",
            "notes",
            0,
            "body-1",
            "evals-1",
            "result-empty",
            "2026-11-12T00:00:01Z",
        );
        req.binding.cases_json = "[]".to_owned();
        let empty = store.activate_skill(&req).await.unwrap_err();
        assert_eq!(empty.to_string(), "skill_eval_empty");
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());

        req.activation_id = "act-partial".to_owned();
        req.binding.result_id = "result-partial".to_owned();
        req.binding.cases_json =
            r#"[{"id":"a","passed":false},{"id":"b","passed":false}]"#.to_owned();
        req.binding.accepted_json = r#"["a"]"#.to_owned();
        let partial = store.activate_skill(&req).await.unwrap_err();
        assert_eq!(partial.to_string(), "skill_eval_failed");
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());
        assert!(
            store
                .list_skill_activations("notes")
                .await
                .unwrap()
                .is_empty()
        );

        req.activation_id = "act-ok".to_owned();
        req.binding.result_id = "result-ok".to_owned();
        req.binding.accepted_json = r#"["b","a"]"#.to_owned();
        let ok = store.activate_skill(&req).await.unwrap();
        assert_eq!(
            ok,
            SkillActivationOutcome::Activated {
                generation: 1,
                activation_id: "act-ok".to_owned()
            }
        );
        assert_eq!(
            store
                .get_skill_pointer("notes")
                .await
                .unwrap()
                .unwrap()
                .generation,
            1
        );
    }

    #[tokio::test]
    async fn duplicate_result_id_and_activation_id_are_conflicts() {
        let store = Store::open_memory().await.unwrap();
        for (id, semver, kind, bytes) in [
            ("body-1", "1.0.0", SkillRevisionKind::Body, &b"one"[..]),
            ("body-2", "1.1.0", SkillRevisionKind::Body, &b"two"[..]),
            ("evals-1", "1.0.0", SkillRevisionKind::Evals, &b"ev"[..]),
        ] {
            put(
                &store,
                &revision(id, "notes", SkillOrigin::User, semver, kind, "label", bytes),
            )
            .await;
        }
        store
            .activate_skill(&activation(
                "act-1",
                "notes",
                0,
                "body-1",
                "evals-1",
                "result-1",
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap();
        let reused_result = store
            .activate_skill(&activation(
                "act-2",
                "notes",
                1,
                "body-2",
                "evals-1",
                "result-1",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(reused_result, StoreError::Conflict(ref message) if message.contains("result_id")),
            "{reused_result:?}"
        );
        assert!(!matches!(reused_result, StoreError::Sqlx(_)));
        let reused_activation = store
            .activate_skill(&activation(
                "act-1",
                "notes",
                1,
                "body-2",
                "evals-1",
                "result-2",
                "2026-11-12T00:00:03Z",
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(reused_activation, StoreError::Conflict(ref message) if message.contains("activation_id")),
            "{reused_activation:?}"
        );
        let pointer = store.get_skill_pointer("notes").await.unwrap().unwrap();
        assert_eq!(pointer.body_revision_id, "body-1");
        assert_eq!(pointer.generation, 1);
        assert_eq!(
            store.list_skill_activations("notes").await.unwrap().len(),
            1
        );
        let moved = store
            .activate_skill(&activation(
                "act-2",
                "notes",
                1,
                "body-2",
                "evals-1",
                "result-2",
                "2026-11-12T00:00:04Z",
            ))
            .await
            .unwrap();
        assert_eq!(
            moved,
            SkillActivationOutcome::Activated {
                generation: 2,
                activation_id: "act-2".to_owned()
            }
        );
    }

    async fn activate_pair(
        store: &Store,
        skill_id: &str,
        body_id: &str,
        evals_id: &str,
        activation_id: &str,
        result_id: &str,
    ) {
        put(
            store,
            &revision(
                body_id,
                skill_id,
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "body",
                body_id.as_bytes(),
            ),
        )
        .await;
        put(
            store,
            &revision(
                evals_id,
                skill_id,
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "evals",
                evals_id.as_bytes(),
            ),
        )
        .await;
        store
            .activate_skill(&activation(
                activation_id,
                skill_id,
                0,
                body_id,
                evals_id,
                result_id,
                "2026-11-12T00:00:01Z",
            ))
            .await
            .unwrap();
    }

    async fn row_count(store: &Store, table: &str) -> i64 {
        let sql = match table {
            "skill_activation" => "SELECT COUNT(*) FROM skill_activation",
            "skill_eval_result" => "SELECT COUNT(*) FROM skill_eval_result",
            other => panic!("unexpected table {other}"),
        };
        sqlx::query_scalar(sql)
            .fetch_one(test_hooks::pool(store))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn idempotent_activate_rejects_another_skills_ids() {
        let store = Store::open_memory().await.unwrap();
        activate_pair(&store, "alpha", "body-a", "evals-a", "act-a", "result-a").await;
        activate_pair(&store, "beta", "body-b", "evals-b", "act-b", "result-b").await;

        let stolen_activation = store
            .activate_skill(&activation(
                "act-a",
                "beta",
                1,
                "body-b",
                "evals-b",
                "result-fresh",
                "2026-11-12T00:00:02Z",
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(stolen_activation, StoreError::Conflict(ref message) if message.contains("activation_id")),
            "{stolen_activation:?}"
        );

        let stolen_result = store
            .activate_skill(&activation(
                "act-fresh",
                "beta",
                1,
                "body-b",
                "evals-b",
                "result-a",
                "2026-11-12T00:00:03Z",
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(stolen_result, StoreError::Conflict(ref message) if message.contains("result_id")),
            "{stolen_result:?}"
        );

        let activations = row_count(&store, "skill_activation").await;
        let results = row_count(&store, "skill_eval_result").await;
        let repeat = store
            .activate_skill(&activation(
                "act-new",
                "beta",
                0,
                "body-b",
                "evals-b",
                "result-new",
                "2026-11-12T00:00:04Z",
            ))
            .await
            .unwrap();
        assert_eq!(repeat, SkillActivationOutcome::Unchanged { generation: 1 });
        assert_eq!(row_count(&store, "skill_activation").await, activations);
        assert_eq!(row_count(&store, "skill_eval_result").await, results);
        let pointer = store.get_skill_pointer("beta").await.unwrap().unwrap();
        assert_eq!(pointer.generation, 1);
        assert_eq!(pointer.body_revision_id, "body-b");
    }

    #[tokio::test]
    async fn idempotent_rollback_rejects_another_skills_activation_id() {
        let store = Store::open_memory().await.unwrap();
        activate_pair(&store, "alpha", "body-a", "evals-a", "act-a", "result-a").await;
        activate_pair(&store, "beta", "body-b", "evals-b", "act-b", "result-b").await;

        let stolen = store
            .rollback_skill(&SkillRollback {
                activation_id: "act-a".to_owned(),
                skill_id: "beta".to_owned(),
                expected_generation: 1,
                source_activation_id: "act-b".to_owned(),
                created_at: "2026-11-12T00:00:02Z".to_owned(),
                actor: "host".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(stolen, StoreError::Conflict(ref message) if message.contains("activation_id")),
            "{stolen:?}"
        );

        let activations = row_count(&store, "skill_activation").await;
        let results = row_count(&store, "skill_eval_result").await;
        let repeat = store
            .rollback_skill(&SkillRollback {
                activation_id: "act-new".to_owned(),
                skill_id: "beta".to_owned(),
                expected_generation: 0,
                source_activation_id: "act-b".to_owned(),
                created_at: "2026-11-12T00:00:03Z".to_owned(),
                actor: "host".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(repeat, SkillActivationOutcome::Unchanged { generation: 1 });
        assert_eq!(row_count(&store, "skill_activation").await, activations);
        assert_eq!(row_count(&store, "skill_eval_result").await, results);
        assert_eq!(store.list_skill_activations("beta").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn duplicate_accepted_id_is_a_conflict() {
        let store = Store::open_memory().await.unwrap();
        put(
            &store,
            &revision(
                "body-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "b1",
                b"body",
            ),
        )
        .await;
        put(
            &store,
            &revision(
                "evals-1",
                "notes",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Evals,
                "e1",
                b"evals",
            ),
        )
        .await;
        let mut req = activation(
            "act-dup",
            "notes",
            0,
            "body-1",
            "evals-1",
            "result-dup",
            "2026-11-12T00:00:01Z",
        );
        req.binding.cases_json = r#"[{"id":"a","passed":false}]"#.to_owned();
        req.binding.accepted_json = r#"["a","a"]"#.to_owned();
        let err = store.activate_skill(&req).await.unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message.contains("duplicated")),
            "{err:?}"
        );
        assert!(store.get_skill_pointer("notes").await.unwrap().is_none());
        assert!(
            store
                .list_skill_activations("notes")
                .await
                .unwrap()
                .is_empty()
        );
    }

    fn pair_row(
        revision_id: &str,
        skill_id: &str,
        semver: &str,
        kind: SkillRevisionKind,
        digest: &str,
        content: &[u8],
    ) -> NewSkillRevision {
        NewSkillRevision {
            revision_id: revision_id.to_owned(),
            skill_id: skill_id.to_owned(),
            origin: SkillOrigin::Imported,
            semver: semver.to_owned(),
            kind,
            content_digest: digest.to_owned(),
            content: content.to_vec(),
            created_at: "2026-11-12T00:00:00Z".to_owned(),
        }
    }

    #[tokio::test]
    async fn install_pair_adopts_same_digest_and_rolls_back_a_conflict() {
        let store = Store::open_memory().await.unwrap();
        let body = pair_row(
            "body-1",
            "notes",
            "0.1.0",
            SkillRevisionKind::Body,
            "digest-body",
            b"body-v1",
        );
        let evals = pair_row(
            "evals-1",
            "notes",
            "0.1.0",
            SkillRevisionKind::Evals,
            &content_digest_of(b"evals-v1"),
            b"evals-v1",
        );
        let inserted = store.install_skill_pair(&body, &evals).await.unwrap();
        assert!(matches!(inserted.0, SkillRevisionInsert::Inserted { .. }));
        let mut bumped = body.clone();
        bumped.revision_id = "body-2".to_owned();
        bumped.semver = "0.2.0".to_owned();
        bumped.content = b"body-v1-with-a-different-version-line".to_vec();
        let mut evals_bumped = evals.clone();
        evals_bumped.revision_id = "evals-2".to_owned();
        evals_bumped.semver = "0.2.0".to_owned();
        let again = store
            .install_skill_pair(&bumped, &evals_bumped)
            .await
            .unwrap();
        assert_eq!(
            again.0,
            SkillRevisionInsert::Existing {
                revision_id: "body-1".to_owned()
            }
        );
        assert_eq!(revision_count(&store).await, 2);
        assert_eq!(
            store
                .get_skill_revision("body-1")
                .await
                .unwrap()
                .unwrap()
                .content,
            b"body-v1"
        );

        put(
            &store,
            &revision(
                "taken",
                "other",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "other-digest",
                b"other",
            ),
        )
        .await;
        let fresh = pair_row(
            "body-new",
            "fresh",
            "1.0.0",
            SkillRevisionKind::Body,
            "fresh-body",
            b"fresh body",
        );
        let clash = pair_row(
            "taken",
            "fresh",
            "1.0.0",
            SkillRevisionKind::Evals,
            &content_digest_of(b"fresh evals"),
            b"fresh evals",
        );
        let err = store.install_skill_pair(&fresh, &clash).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
        assert!(
            store
                .get_skill_revision("body-new")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn semver_hit_with_another_digest_conflicts_before_digest_dedup() {
        let store = Store::open_memory().await.unwrap();
        let evals_a = content_digest_of(b"evals-a");
        let evals_b = content_digest_of(b"evals-b");
        store
            .install_skill_pair(
                &pair_row(
                    "b1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"A",
                ),
                &pair_row(
                    "e1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Evals,
                    &evals_a,
                    b"evals-a",
                ),
            )
            .await
            .unwrap();
        store
            .install_skill_pair(
                &pair_row(
                    "b2",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Body,
                    "digest-b",
                    b"B",
                ),
                &pair_row(
                    "e2",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Evals,
                    &evals_b,
                    b"evals-b",
                ),
            )
            .await
            .unwrap();
        let err = store
            .install_skill_pair(
                &pair_row(
                    "b3",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"A-again",
                ),
                &pair_row(
                    "e3",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Evals,
                    &evals_b,
                    b"evals-b",
                ),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::SkillRevisionConflict), "{err:?}");
        assert_eq!(revision_count(&store).await, 4);
        assert_eq!(
            store
                .get_skill_revision("b2")
                .await
                .unwrap()
                .unwrap()
                .content,
            b"B"
        );
        assert!(store.get_skill_revision("b3").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn same_manifest_digest_and_different_bytes_reuse_the_body_row() {
        let store = Store::open_memory().await.unwrap();
        let evals = content_digest_of(b"evals");
        let first = store
            .install_skill_pair(
                &pair_row(
                    "b1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"version: 1.0.0\nhello\n",
                ),
                &pair_row(
                    "e1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Evals,
                    &evals,
                    b"evals",
                ),
            )
            .await
            .unwrap();
        assert!(matches!(first.0, SkillRevisionInsert::Inserted { .. }));
        let again = store
            .install_skill_pair(
                &pair_row(
                    "b2",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"version: 2.0.0\nhello\n",
                ),
                &pair_row(
                    "e2",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Evals,
                    &evals,
                    b"evals",
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            again.0,
            SkillRevisionInsert::Existing {
                revision_id: "b1".to_owned()
            }
        );
        assert_eq!(
            again.1,
            SkillRevisionInsert::Existing {
                revision_id: "e1".to_owned()
            }
        );
        assert_eq!(revision_count(&store).await, 2);
        assert_eq!(
            store
                .get_skill_revision("b1")
                .await
                .unwrap()
                .unwrap()
                .content,
            b"version: 1.0.0\nhello\n"
        );
    }

    /// v1 known limitation. A version-only reuse does not store the new semver,
    /// so the same semver with a different digest inserts another row. Pointers
    /// and pins use revision ids. The semver label can name two bodies.
    #[tokio::test]
    async fn known_limitation_version_reuse_lets_the_same_semver_insert_another_body() {
        let store = Store::open_memory().await.unwrap();
        let evals = content_digest_of(b"evals");
        store
            .install_skill_pair(
                &pair_row(
                    "b1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"A",
                ),
                &pair_row(
                    "e1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Evals,
                    &evals,
                    b"evals",
                ),
            )
            .await
            .unwrap();
        let reused = store
            .install_skill_pair(
                &pair_row(
                    "b2",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"A",
                ),
                &pair_row(
                    "e2",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Evals,
                    &evals,
                    b"evals",
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            reused.0,
            SkillRevisionInsert::Existing {
                revision_id: "b1".to_owned()
            }
        );
        let inserted = store
            .install_skill_pair(
                &pair_row(
                    "b3",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Body,
                    "digest-b",
                    b"B",
                ),
                &pair_row(
                    "e3",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Evals,
                    &evals,
                    b"evals",
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            inserted.0,
            SkillRevisionInsert::Inserted {
                revision_id: "b3".to_owned()
            }
        );
        assert_eq!(revision_count(&store).await, 3);
        assert_eq!(
            store
                .get_skill_revision("b3")
                .await
                .unwrap()
                .unwrap()
                .content,
            b"B"
        );
    }

    /// v1 known limitation, eval side. Reusing an eval row does not store the
    /// new semver, so a later different eval at that semver inserts. It does
    /// not return [`StoreError::SkillRevisionConflict`].
    #[tokio::test]
    async fn known_limitation_eval_semver_reuse_inserts_another_eval_row() {
        let store = Store::open_memory().await.unwrap();
        let evals_v1 = content_digest_of(b"E1");
        let evals_v2 = content_digest_of(b"E2");
        store
            .install_skill_pair(
                &pair_row(
                    "b1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Body,
                    "digest-b1",
                    b"B1",
                ),
                &pair_row(
                    "e1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Evals,
                    &evals_v1,
                    b"E1",
                ),
            )
            .await
            .unwrap();
        let reused = store
            .install_skill_pair(
                &pair_row(
                    "b2",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Body,
                    "digest-b2",
                    b"B2",
                ),
                &pair_row(
                    "e-reuse",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Evals,
                    &evals_v1,
                    b"E1",
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            reused.0,
            SkillRevisionInsert::Inserted {
                revision_id: "b2".to_owned()
            }
        );
        assert_eq!(
            reused.1,
            SkillRevisionInsert::Existing {
                revision_id: "e1".to_owned()
            }
        );
        assert!(store.get_skill_revision("e-reuse").await.unwrap().is_none());
        let inserted = store
            .install_skill_pair(
                &pair_row(
                    "b2-again",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Body,
                    "digest-b2",
                    b"B2",
                ),
                &pair_row(
                    "e2",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Evals,
                    &evals_v2,
                    b"E2",
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            inserted.0,
            SkillRevisionInsert::Existing {
                revision_id: "b2".to_owned()
            }
        );
        assert_eq!(
            inserted.1,
            SkillRevisionInsert::Inserted {
                revision_id: "e2".to_owned()
            }
        );
        assert_eq!(revision_count(&store).await, 4);
        assert_eq!(
            store
                .get_skill_revision("e2")
                .await
                .unwrap()
                .unwrap()
                .content,
            b"E2"
        );
        assert_eq!(
            store
                .get_skill_revision("e1")
                .await
                .unwrap()
                .unwrap()
                .semver,
            "1.0.0"
        );
    }

    #[tokio::test]
    async fn install_pair_dedupe_rejects_a_revision_id_owned_by_another_row() {
        let store = Store::open_memory().await.unwrap();
        let evals = content_digest_of(b"evals");
        store
            .install_skill_pair(
                &pair_row(
                    "b1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"A",
                ),
                &pair_row(
                    "e1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Evals,
                    &evals,
                    b"evals",
                ),
            )
            .await
            .unwrap();
        let again = store
            .install_skill_pair(
                &pair_row(
                    "b1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"A",
                ),
                &pair_row(
                    "e1",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Evals,
                    &evals,
                    b"evals",
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            again.0,
            SkillRevisionInsert::Existing {
                revision_id: "b1".to_owned()
            }
        );
        assert_eq!(
            again.1,
            SkillRevisionInsert::Existing {
                revision_id: "e1".to_owned()
            }
        );
        put(
            &store,
            &revision(
                "taken",
                "other",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "other",
                b"other",
            ),
        )
        .await;

        let err = store
            .install_skill_pair(
                &pair_row(
                    "taken",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"A",
                ),
                &pair_row(
                    "e-new",
                    "notes",
                    "1.0.0",
                    SkillRevisionKind::Evals,
                    &evals,
                    b"evals",
                ),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message.contains("revision_id")),
            "{err:?}"
        );

        let err = store
            .install_skill_pair(
                &pair_row(
                    "b-new",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Body,
                    "digest-a",
                    b"A-later",
                ),
                &pair_row(
                    "taken",
                    "notes",
                    "2.0.0",
                    SkillRevisionKind::Evals,
                    &evals,
                    b"evals",
                ),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message.contains("revision_id")),
            "{err:?}"
        );
        assert_eq!(revision_count(&store).await, 3);
        assert!(store.get_skill_revision("e-new").await.unwrap().is_none());
        assert!(store.get_skill_revision("b-new").await.unwrap().is_none());
        assert_eq!(
            store
                .get_skill_revision("b1")
                .await
                .unwrap()
                .unwrap()
                .content,
            b"A"
        );
    }

    #[tokio::test]
    async fn dedupe_hit_rejects_a_revision_id_owned_by_another_row() {
        let store = Store::open_memory().await.unwrap();
        let first = revision(
            "rev-body-1",
            "notes",
            SkillOrigin::User,
            "1.0.0",
            SkillRevisionKind::Body,
            "digest-a",
            b"hello skill",
        );
        put(&store, &first).await;
        put(
            &store,
            &revision(
                "taken",
                "other",
                SkillOrigin::User,
                "1.0.0",
                SkillRevisionKind::Body,
                "other",
                b"other",
            ),
        )
        .await;
        let mut again = first.clone();
        again.revision_id = "taken".to_owned();
        let err = store.insert_skill_revision(&again).await.unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message.contains("revision_id")),
            "{err:?}"
        );
        assert_eq!(revision_count(&store).await, 2);
        assert_eq!(
            store
                .get_skill_revision("rev-body-1")
                .await
                .unwrap()
                .unwrap()
                .content,
            b"hello skill"
        );
    }
}
