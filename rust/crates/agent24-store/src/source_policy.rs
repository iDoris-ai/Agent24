//! K1-6b.1 (ADR-K1-02 §6 slice 6b.1) — source tags & persistent run
//! metadata, storage side. See `migrations/0016_run_source_tags.sql` for the
//! schema.
//!
//! This module defines the kernel-private `SourceRef`/`PolicySnapshot`
//! contract: the label a trusted host attaches to a block of content
//! entering a run's context (canonical source id, revision digest,
//! processing mode, policy version, optional authorization reference), and
//! persists/reads it back associated with a run and one of its messages.
//!
//! ## Model-call coverage (K1-6b.2)
//!
//! The run source policy is merged at these production call sites:
//!
//! - `RunManager::run_loop` in `agent24-agent/src/lib.rs`: every run-loop model
//!   call, including approval continuation and restart recovery (which reuse
//!   the same loop), reads the persisted policy.
//! - REST `/api/v1/chat` in `agent24d/src/routes.rs`: reads the transient/run
//!   policy before its model call; missing policy is `LocalOnly`.
//! - Explorer child calls in `agent24-agent/src/subagent.rs`: reads the parent
//!   run policy; missing store or read failure is `LocalOnly`.
//! - Session compaction in `RouterSummarizer::summarize` in
//!   `agent24-agent/src/lib.rs`: uses `merge_source_policy` with `LocalOnly`.
//!   `Summarizer` currently has no run/message provenance, and a session can
//!   span runs, so compaction stays fail-closed until that provenance is
//!   available.
//!
//! 6b.3 explicitly defers the HTTP/MCP/module/Exec egress gates. This list is
//! the complete set of production model-router call sites covered by 6b.2;
//! test-only calls and the router's own unit tests are not policy consumers.
//! Thread text cannot widen persisted policy.
//!
//! **Fail-closed by construction** (ADR-K1-02 §0): `SourceMode` defaults to
//! `LocalOnly`; a run with no tags folds to `LocalOnly`
//! ([`PolicySnapshot::fail_closed`]); and a stored row this crate cannot
//! confidently decode — wrong `schema_version`, or `tag_json` that fails to
//! parse as the CURRENT shape — degrades to a `LocalOnly` tag rather than
//! erroring or defaulting to whatever text happens to be on disk.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store};

/// Current on-disk shape version for a [`SourceRef`] row. A reader that
/// finds a different value treats the row as unreadable and degrades it to
/// `LocalOnly` WITHOUT attempting to parse `tag_json` — see module docs.
pub const SOURCE_TAG_SCHEMA_VERSION: i64 = 2;

/// Every [`SourceRef::tool_result`] id starts with this, followed by the
/// result's origin.
pub const TOOL_RESULT_PREFIX: &str = "tool_result:";

/// What kind of content entering the run this tag describes. Informational
/// only in this slice (nothing branches on it yet); an unrecognized value on
/// read is a decode failure, which already forces the tag to `LocalOnly` via
/// [`SourceRef`]'s fail-closed read path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// The run's own prompt / user-authored turn.
    UserInput,
    /// A file, document, or other resource the user (or a future retrieval
    /// step) selected into context. No Agent24 entry point populates this
    /// yet (ID-2 is unimplemented) — modeled ahead of that wiring so the
    /// contract does not need to change shape when it lands.
    SelectedMaterial,
    /// A recalled assertion. This identifies the retrieval path; it is not
    /// itself a sensitivity classification.
    MemoryRecall,
    /// Earlier turns of the run's session (K1-6b.4c), classified by the
    /// session's source floor.
    SessionContext,
    /// A prompt a kernel-created schedule (a self-wake) carries from the
    /// run that created it (K1-6b.4d).
    ScheduleOrigin,
    /// What a tool call returned into the run's context (K1-6b.5f). The
    /// classification is the host's for that kind of tool, never the tool's
    /// or module's own claim.
    ToolResult,
}

/// Sensitivity classification is independent from processing mode and source
/// kind. Missing/invalid persisted classifications are never assumed ordinary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceClassification {
    Ordinary,
    Restricted,
    #[default]
    Unknown,
}

/// Processing mode. `LocalOnly` is the only mode [`SourceRef::user_input`]
/// and [`SourceRef::selected_material`] — the only tag constructors this
/// slice provides — ever produce; `CloudAuthorized` is modeled for the
/// future out-of-process authorization flow (ADR-K1-02 §2.3). Nothing in
/// this slice grants it: [`PolicySnapshot::from_tags`]'s fold can still
/// report an effective `CloudAuthorized` mode, but only by reflecting
/// `SourceRef`s a caller already persisted with that mode directly — this
/// slice's own write path never does so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceMode {
    #[default]
    LocalOnly,
    CloudAuthorized,
}

/// A trusted-host label for one block of content entering agent context
/// (ADR-K1-02 §2.1). Not model/module/tool-writable: every value here is
/// constructed by host code (`agent24-agent`'s run entry, in this slice) and
/// only ever read by the host, never echoed back to or trusted from a
/// model/tool response.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceRef {
    /// Canonical, caller-assigned source id — stable across revisions of the
    /// same underlying source.
    pub source_id: String,
    pub kind: SourceKind,
    /// `Unknown` is the safe default for old or incomplete persisted tags.
    #[serde(default)]
    pub classification: SourceClassification,
    /// Revision/content digest, when the source has one. `None` for a
    /// source with no meaningful revision concept (e.g. a one-shot prompt).
    pub revision_digest: Option<String>,
    pub mode: SourceMode,
    /// Which policy ruleset was in effect when this tag was created. `0`
    /// means "no policy authority wired yet" — this slice never has one to
    /// reference; a future slice that adds a real policy-version source
    /// must mint from it here instead of overloading this value's meaning.
    pub policy_version: i64,
    /// Reference to an external authorization record, when `mode` is
    /// `CloudAuthorized`. Always `None` in this slice (see [`SourceMode`]).
    pub authorization_ref: Option<String>,
    /// Caller-supplied timestamp (ISO 8601) — this crate never reads a
    /// clock, matching `decision_log.rs`'s rule.
    pub created_at: String,
}

impl SourceRef {
    /// A persisted-memory hit with no usable provenance. Its stable marker is
    /// a category, not an assertion identifier, and folds to LocalOnly.
    pub fn unknown_memory(created_at: impl Into<String>) -> Self {
        Self {
            source_id: "unknown:memory".to_owned(),
            kind: SourceKind::MemoryRecall,
            classification: SourceClassification::Unknown,
            revision_digest: None,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at: created_at.into(),
        }
    }

    /// Earlier turns of a session entering a run's context. Its
    /// classification is the session's floor as returned by
    /// [`Store::merge_session_floor`].
    pub fn session_floor(
        session_id: &str,
        classification: SourceClassification,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            source_id: format!("session_floor:{session_id}"),
            kind: SourceKind::SessionContext,
            classification,
            revision_digest: None,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at: created_at.into(),
        }
    }

    /// The prompt of a run started by a kernel-created schedule. Its
    /// classification is the one recorded when the schedule was created
    /// ([`Store::record_schedule_origin`]).
    pub fn schedule_origin(
        schedule_id: &str,
        classification: SourceClassification,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            source_id: format!("schedule_origin:{schedule_id}"),
            kind: SourceKind::ScheduleOrigin,
            classification,
            revision_digest: None,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at: created_at.into(),
        }
    }

    /// What one tool call returned (K1-6b.5f, ADR-K1-02 §2.1). `origin` names
    /// where the content came from as the host knows it (`module:{id}`,
    /// `tool:{name}`), so the egress gate can recognise content a module
    /// sent back to that same module.
    pub fn tool_result(
        origin: &str,
        tool_call_id: &str,
        classification: SourceClassification,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            source_id: format!("{TOOL_RESULT_PREFIX}{origin}:{tool_call_id}"),
            kind: SourceKind::ToolResult,
            classification,
            revision_digest: None,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at: created_at.into(),
        }
    }

    /// Results from `origin` that an earlier turn of `session_id` took in
    /// (K1-6b.5f-2), entering this run through session context. Always
    /// `Unknown`: only non-ordinary origins are recorded
    /// ([`Store::tag_tool_result`]). The id keeps the
    /// `tool_result:{origin}:` prefix, so the same-source exception still
    /// recognises a module's own results.
    pub fn session_tool_result(
        origin: &str,
        session_id: &str,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            source_id: format!("{TOOL_RESULT_PREFIX}{origin}:session:{session_id}"),
            kind: SourceKind::ToolResult,
            classification: SourceClassification::Unknown,
            revision_digest: None,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at: created_at.into(),
        }
    }

    /// A recalled assertion entering a run's context. Its classification is
    /// the one stored with the assertion ([`AssertionSourceRef::classification_of`]);
    /// the tag kind only names the retrieval path (ADR-K1-02 §6 item 4).
    pub fn recalled_memory(
        assertion_id: &str,
        classification: SourceClassification,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            source_id: format!("memory_assertion:{assertion_id}"),
            kind: SourceKind::MemoryRecall,
            classification,
            revision_digest: None,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at: created_at.into(),
        }
    }

    /// A fresh, unclassified tag for user input at run entry. Always
    /// `LocalOnly` for the model routing default, and `Ordinary` as the separate
    /// sensitivity classification (ADR-K1-02 §2.1.1). Local routing does not
    /// label ordinary user input as restricted.
    pub fn user_input(run_id: &str, created_at: impl Into<String>) -> Self {
        SourceRef {
            source_id: format!("user_input:{run_id}"),
            kind: SourceKind::UserInput,
            classification: SourceClassification::Ordinary,
            revision_digest: None,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at: created_at.into(),
        }
    }

    /// A fresh, unclassified tag for a selected material. No current entry
    /// point calls this (see [`SourceKind::SelectedMaterial`]'s docs); kept
    /// public so the contract already covers it when one lands. Also
    /// `LocalOnly` by default for the same reason as [`Self::user_input`].
    pub fn selected_material(
        material_id: &str,
        revision_digest: Option<String>,
        created_at: impl Into<String>,
    ) -> Self {
        SourceRef {
            source_id: format!("selected_material:{material_id}"),
            kind: SourceKind::SelectedMaterial,
            classification: SourceClassification::Unknown,
            revision_digest,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at: created_at.into(),
        }
    }

    /// The fail-closed tag constructed when a stored row cannot be
    /// confidently decoded (unknown/mismatched `schema_version`, or
    /// `tag_json` that fails to parse). Preserves the row's own
    /// `source_id`/`created_at` columns — those are read directly, never
    /// parsed out of `tag_json` — but everything else is reset to the safe
    /// default.
    fn fail_closed_unknown(source_id: String, created_at: String) -> Self {
        SourceRef {
            source_id,
            kind: SourceKind::MemoryRecall,
            classification: SourceClassification::Unknown,
            revision_digest: None,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at,
        }
    }
}

/// A run's effective source policy, folded from every tag persisted against
/// it. `effective_mode` is the upper bound for model context routing; it is
/// not the payload/source set for an arbitrary action (ADR-K1-02 §2.1/§2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySnapshot {
    pub schema_version: i64,
    pub effective_mode: SourceMode,
    pub sources: Vec<SourceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSourceTag {
    pub seq: i64,
    pub source: SourceRef,
}

impl PolicySnapshot {
    /// The fail-closed snapshot for a run with no persisted tags. A missing
    /// source is NOT "no restriction" — it is treated as the most sensitive
    /// case (ADR-K1-02 §0).
    pub fn fail_closed() -> Self {
        PolicySnapshot {
            schema_version: SOURCE_TAG_SCHEMA_VERSION,
            effective_mode: SourceMode::LocalOnly,
            sources: Vec::new(),
        }
    }

    /// Fold a run's persisted tags into its effective snapshot: the
    /// strictest mode among all tags wins (a single `LocalOnly` tag makes
    /// the whole snapshot `LocalOnly`), matching ADR-K1-02 §2.1's "模式取
    /// 所有输入...中最严格者". An empty set is [`Self::fail_closed`], not an
    /// unrestricted snapshot.
    pub fn from_tags(sources: Vec<SourceRef>) -> Self {
        if sources.is_empty()
            || sources.iter().any(|s| {
                s.mode == SourceMode::LocalOnly
                    || s.classification != SourceClassification::Ordinary
            })
        {
            return PolicySnapshot {
                schema_version: SOURCE_TAG_SCHEMA_VERSION,
                effective_mode: SourceMode::LocalOnly,
                sources,
            };
        }
        PolicySnapshot {
            schema_version: SOURCE_TAG_SCHEMA_VERSION,
            effective_mode: SourceMode::CloudAuthorized,
            sources,
        }
    }
}

/// Decode one `run_source_tags` row. Fails closed to
/// [`SourceRef::fail_closed_unknown`] whenever the row's `schema_version`
/// does not match [`SOURCE_TAG_SCHEMA_VERSION`] — `tag_json` is not even
/// parsed in that case, since an old/newer schema's JSON shape is not
/// guaranteed compatible with the current `SourceRef` struct — or when
/// `tag_json` fails to parse as the current shape, or decodes to a
/// different `source_id` than the row's own column (defense in depth
/// against a hand-edited or corrupted row).
fn decode_tag_row(row: &SqliteRow) -> StoredSourceTag {
    let seq = match row.try_get::<i64, _>("seq") {
        Ok(seq) if seq >= 0 => seq,
        _ => -1,
    };
    let source_id = match row.try_get::<String, _>("source_id") {
        Ok(source_id) => source_id,
        Err(_) => return fail_closed_tag(seq, String::new(), String::new()),
    };
    let created_at = match row.try_get::<String, _>("created_at") {
        Ok(created_at) => created_at,
        Err(_) => return fail_closed_tag(seq, source_id, String::new()),
    };
    let schema_version = match row.try_get::<i64, _>("schema_version") {
        Ok(version) => version,
        Err(_) => return fail_closed_tag(seq, source_id, created_at),
    };
    if seq < 0 || schema_version != SOURCE_TAG_SCHEMA_VERSION {
        return fail_closed_tag(seq, source_id, created_at);
    }
    let tag_json = match row.try_get::<String, _>("tag_json") {
        Ok(tag_json) => tag_json,
        Err(_) => return fail_closed_tag(seq, source_id, created_at),
    };
    let source = match serde_json::from_str::<SourceRef>(&tag_json) {
        Ok(parsed) if parsed.source_id == source_id => parsed,
        _ => SourceRef::fail_closed_unknown(source_id, created_at),
    };
    StoredSourceTag { seq, source }
}

fn fail_closed_tag(seq: i64, source_id: String, created_at: String) -> StoredSourceTag {
    StoredSourceTag {
        seq,
        source: SourceRef::fail_closed_unknown(source_id, created_at),
    }
}

/// Current shape of [`AssertionSourceRef`] in `mem_assertions.source_ref`.
/// A stored value with any other version reads as `Unknown`.
pub const ASSERTION_SOURCE_REF_SCHEMA_VERSION: i64 = 1;

/// The provenance a memory assertion is written with (ADR-K1-02 §5 item 10),
/// stored in the same row and transaction as its body. Only the host's
/// retain path builds one; recall reads its classification back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionSourceRef {
    pub schema_version: i64,
    /// The strictest classification among the sources of the context the
    /// assertion was written from.
    pub classification: SourceClassification,
    /// Those sources' ids, for audit only; nothing decides on them.
    pub source_ids: Vec<String>,
}

impl AssertionSourceRef {
    /// Fold the writing context's sources as `(source_id, classification)`.
    /// No sources is no evidence, so the result is `Unknown` (ADR-K1-02
    /// §2.1.1 item 4).
    pub fn from_sources(sources: impl IntoIterator<Item = (String, SourceClassification)>) -> Self {
        let (source_ids, classifications): (Vec<_>, Vec<_>) = sources.into_iter().unzip();
        AssertionSourceRef {
            schema_version: ASSERTION_SOURCE_REF_SCHEMA_VERSION,
            classification: strictest_classification(classifications),
            source_ids,
        }
    }

    /// This provenance, raised to at least `floor` (never lowered).
    #[must_use]
    pub fn at_least(&self, floor: SourceClassification) -> Self {
        Self {
            classification: strictest_classification([self.classification, floor]),
            ..self.clone()
        }
    }

    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "schema_version": self.schema_version,
            "classification": self.classification,
            "source_ids": self.source_ids,
        })
    }

    /// The classification of a stored `source_ref`. NULL, a value that does
    /// not parse as the current shape, or another schema version reads as
    /// `Unknown`, never `Ordinary`.
    pub fn classification_of(stored: Option<&serde_json::Value>) -> SourceClassification {
        stored
            .and_then(|value| serde_json::from_value::<Self>(value.clone()).ok())
            .filter(|parsed| parsed.schema_version == ASSERTION_SOURCE_REF_SCHEMA_VERSION)
            .map_or(SourceClassification::Unknown, |parsed| {
                parsed.classification
            })
    }
}

fn classification_column(classification: SourceClassification) -> &'static str {
    match classification {
        SourceClassification::Ordinary => "ordinary",
        SourceClassification::Unknown => "unknown",
        SourceClassification::Restricted => "restricted",
    }
}

/// A stored floor this crate does not recognize reads as `Unknown`.
fn classification_from_column(raw: &str) -> SourceClassification {
    match raw {
        "ordinary" => SourceClassification::Ordinary,
        "restricted" => SourceClassification::Restricted,
        _ => SourceClassification::Unknown,
    }
}

/// Strictest classification: `Restricted` over `Unknown` over `Ordinary`;
/// an empty input is `Unknown`.
pub fn strictest_classification(
    classifications: impl IntoIterator<Item = SourceClassification>,
) -> SourceClassification {
    let mut strictest = None;
    for classification in classifications {
        strictest = Some(match (strictest, classification) {
            (_, SourceClassification::Restricted) | (Some(SourceClassification::Restricted), _) => {
                SourceClassification::Restricted
            }
            (_, SourceClassification::Unknown) | (Some(SourceClassification::Unknown), _) => {
                SourceClassification::Unknown
            }
            _ => SourceClassification::Ordinary,
        });
    }
    strictest.unwrap_or(SourceClassification::Unknown)
}

impl Store {
    /// Persist (or replace) one source tag for a run's message. Idempotent
    /// by `(run_id, seq, source_id)` — re-tagging the same source on the
    /// same message overwrites rather than duplicating.
    pub async fn tag_run_source(
        &self,
        run_id: &str,
        seq: i64,
        tag: &SourceRef,
        now: &str,
    ) -> Result<()> {
        let mut conn = self.pool().acquire().await?;
        insert_run_source_tag(&mut conn, run_id, seq, tag, now).await
    }

    /// K1-6b.5f-2: persist the tag of a tool result and, for a run in a
    /// session, the result's origin as one of the session's
    /// ([`Self::session_tool_result_origins`]) when it is not ordinary.
    /// `session` is `(session_id, origin)`. One transaction, so a later turn
    /// never misses an origin whose result this run's model saw.
    pub async fn tag_tool_result(
        &self,
        run_id: &str,
        seq: i64,
        tag: &SourceRef,
        session: Option<(&str, &str)>,
        now: &str,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        insert_run_source_tag(&mut tx, run_id, seq, tag, now).await?;
        if let Some((session_id, origin)) = session
            && tag.classification != SourceClassification::Ordinary
        {
            sqlx::query(
                "INSERT INTO session_tool_result_origins (session_id, origin, created_at) \
                 VALUES (?, ?, ?) ON CONFLICT(session_id, origin) DO NOTHING",
            )
            .bind(session_id)
            .bind(origin)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Every origin of a non-ordinary tool result that some turn of
    /// `session_id` took in (K1-6b.5f-2), in name order. Empty for a session
    /// with none.
    pub async fn session_tool_result_origins(&self, session_id: &str) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT origin FROM session_tool_result_origins WHERE session_id = ? ORDER BY origin",
        )
        .bind(session_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Every tag persisted against a run, in `(seq, source_id)` order.
    /// Empty for a run with none — callers needing the fail-closed default
    /// use [`Store::run_policy_snapshot`] instead of treating an empty
    /// result as "unrestricted".
    pub async fn list_run_source_tags(&self, run_id: &str) -> Result<Vec<StoredSourceTag>> {
        if crate::test_hooks::source_tag_reads_fail(self) {
            return Err(sqlx::Error::Protocol("injected source tag read failure".into()).into());
        }
        let rows = sqlx::query(
            "SELECT seq, source_id, schema_version, tag_json, created_at \
             FROM run_source_tags WHERE run_id = ? ORDER BY seq ASC, source_id ASC",
        )
        .bind(run_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(decode_tag_row).collect())
    }

    /// K1-6b.4c (ADR-K1-02 §2.2): merge one turn's own classification into
    /// its session's floor and return the floor after the merge. Read and
    /// write share one `BEGIN IMMEDIATE` transaction, so concurrent turns can
    /// never lower it. A turn with prior context but no stored floor counts
    /// that context as `Unknown`.
    pub async fn merge_session_floor(
        &self,
        session_id: &str,
        own: SourceClassification,
        has_prior_context: bool,
        now: &str,
    ) -> Result<SourceClassification> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT classification FROM session_source_floor WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?;
        let prior = match stored.as_deref() {
            Some(raw) => Some(classification_from_column(raw)),
            None => has_prior_context.then_some(SourceClassification::Unknown),
        };
        let merged = strictest_classification(prior.into_iter().chain([own]));
        sqlx::query(
            "INSERT INTO session_source_floor (session_id, classification, updated_at) \
             VALUES (?, ?, ?) \
             ON CONFLICT(session_id) DO UPDATE SET \
                classification = excluded.classification, \
                updated_at = excluded.updated_at",
        )
        .bind(session_id)
        .bind(classification_column(merged))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(merged)
    }

    /// K1-6b.4d (ADR-K1-02 §2.2): record, before the schedule itself is
    /// written, the classification a kernel-created schedule carries from the
    /// run that created it.
    pub async fn record_schedule_origin(
        &self,
        schedule_id: &str,
        origin_run_id: &str,
        classification: SourceClassification,
        now: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO schedule_source_origin (schedule_id, origin_run_id, classification, created_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(schedule_id)
        .bind(origin_run_id)
        .bind(classification_column(classification))
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The classification recorded for a kernel-created schedule; `None` for
    /// a schedule the user created. A self-wake from before 0024 reads as
    /// `Unknown` (migration 0026).
    pub async fn schedule_origin_classification(
        &self,
        schedule_id: &str,
    ) -> Result<Option<SourceClassification>> {
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT classification FROM schedule_source_origin WHERE schedule_id = ?",
        )
        .bind(schedule_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(stored.as_deref().map(classification_from_column))
    }

    /// The run's fail-closed effective policy (ADR-K1-02 §2.1), folded from
    /// every persisted tag. `LocalOnly` when the run has none.
    pub async fn run_policy_snapshot(&self, run_id: &str) -> Result<PolicySnapshot> {
        let tags = self.list_run_source_tags(run_id).await?;
        Ok(PolicySnapshot::from_tags(
            tags.into_iter().map(|tag| tag.source).collect(),
        ))
    }
}

async fn insert_run_source_tag(
    conn: &mut sqlx::SqliteConnection,
    run_id: &str,
    seq: i64,
    tag: &SourceRef,
    now: &str,
) -> Result<()> {
    let tag_json = serde_json::to_string(tag)?;
    sqlx::query(
        "INSERT INTO run_source_tags (run_id, seq, source_id, schema_version, tag_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(run_id, seq, source_id) DO UPDATE SET \
            schema_version = excluded.schema_version, \
            tag_json = excluded.tag_json, \
            created_at = excluded.created_at",
    )
    .bind(run_id)
    .bind(seq)
    .bind(&tag.source_id)
    .bind(SOURCE_TAG_SCHEMA_VERSION)
    .bind(&tag_json)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    async fn store_with_run(run_id: &str) -> Store {
        let store = Store::open_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at) \
             VALUES (?, 'queued', '{}', '{}', '2026-10-08T00:00:00Z')",
        )
        .bind(run_id)
        .execute(store.pool())
        .await
        .unwrap();
        store
    }

    #[test]
    fn source_ref_round_trips_through_serde() {
        let tag = SourceRef::user_input("run_1", "2026-10-08T00:00:00Z");
        let json = serde_json::to_string(&tag).unwrap();
        let back: SourceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(tag, back);

        let material = SourceRef::selected_material(
            "doc_42",
            Some("sha256:abc".to_owned()),
            "2026-10-08T00:00:01Z",
        );
        let json = serde_json::to_string(&material).unwrap();
        let back: SourceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(material, back);
        assert_eq!(tag.classification, SourceClassification::Ordinary);
        assert_eq!(material.classification, SourceClassification::Unknown);
    }

    #[test]
    fn user_input_is_ordinary_while_legacy_classification_is_unknown() {
        let current = serde_json::to_value(SourceRef::user_input("run_1", "t")).unwrap();
        assert_eq!(current["classification"], "ordinary");

        let mut legacy = current;
        legacy.as_object_mut().unwrap().remove("classification");
        let decoded: SourceRef = serde_json::from_value(legacy).unwrap();
        let decoded = serde_json::to_value(decoded).unwrap();
        assert_eq!(decoded["classification"], "unknown");
    }

    #[test]
    fn policy_snapshot_fold_is_strictest_wins() {
        let local = SourceRef::user_input("run_1", "t");
        let mut cloud = SourceRef::selected_material("doc_1", None, "t");
        cloud.classification = SourceClassification::Ordinary;
        cloud.mode = SourceMode::CloudAuthorized;

        // All-cloud folds to cloud.
        let snap = PolicySnapshot::from_tags(vec![cloud.clone()]);
        assert_eq!(snap.effective_mode, SourceMode::CloudAuthorized);

        // One LocalOnly tag forces the whole snapshot LocalOnly, even mixed
        // with a cloud-authorized one.
        let snap = PolicySnapshot::from_tags(vec![cloud, local]);
        assert_eq!(snap.effective_mode, SourceMode::LocalOnly);

        let mut unknown = SourceRef::selected_material("legacy", None, "t");
        unknown.mode = SourceMode::CloudAuthorized;
        assert_eq!(
            PolicySnapshot::from_tags(vec![unknown]).effective_mode,
            SourceMode::LocalOnly,
            "unknown classification must keep model context local"
        );
    }

    #[tokio::test]
    async fn missing_source_tags_default_to_local_only() {
        // Reverse: before this fix, an untagged run had no way to report a
        // policy at all; `PolicySnapshot::fail_closed()` is the explicit
        // fail-closed answer a caller gets instead.
        let store = store_with_run("run_untagged").await;
        let snap = store.run_policy_snapshot("run_untagged").await.unwrap();
        assert_eq!(snap, PolicySnapshot::fail_closed());
        assert_eq!(snap.effective_mode, SourceMode::LocalOnly);
        assert!(snap.sources.is_empty());
    }

    #[tokio::test]
    async fn tag_persists_and_is_associated_with_run_and_message() {
        let store = store_with_run("run_tagged").await;
        let tag = SourceRef::user_input("run_tagged", "2026-10-08T00:00:00Z");
        store
            .tag_run_source("run_tagged", 0, &tag, "2026-10-08T00:00:00Z")
            .await
            .unwrap();
        let tags = store.list_run_source_tags("run_tagged").await.unwrap();
        assert_eq!(tags[0].seq, 0);
        assert_eq!(tags[0].source, tag);
    }

    #[tokio::test]
    async fn unknown_schema_version_degrades_to_local_only_without_parsing() {
        // Reverse: before the schema_version check existed, any stored row
        // was handed straight to `serde_json::from_str`, so a future schema
        // change (or a hand-tampered row) that happened to still parse would
        // silently carry forward whatever `mode` it claimed — including
        // `cloud_authorized` — instead of failing closed.
        let store = store_with_run("run_old_schema").await;
        // Simulate a pre-migration (or future, incompatible) row: a
        // `schema_version` this crate does not recognize, carrying a
        // `cloud_authorized` claim that a naive reader would trust.
        let smuggled_cloud = serde_json::json!({
            "source_id": "doc_legacy",
            "kind": "selected_material",
            "revision_digest": null,
            "mode": "cloud_authorized",
            "policy_version": 1,
            "authorization_ref": "grant_123",
            "created_at": "2026-01-01T00:00:00Z",
        })
        .to_string();
        sqlx::query(
            "INSERT INTO run_source_tags (run_id, seq, source_id, schema_version, tag_json, created_at) \
             VALUES (?, 0, 'doc_legacy', 0, ?, '2026-01-01T00:00:00Z')",
        )
        .bind("run_old_schema")
        .bind(&smuggled_cloud)
        .execute(store.pool())
        .await
        .unwrap();

        let tags = store.list_run_source_tags("run_old_schema").await.unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].source.mode, SourceMode::LocalOnly);
        assert_eq!(tags[0].source.classification, SourceClassification::Unknown);
        assert_eq!(tags[0].source.source_id, "doc_legacy");

        let snap = store.run_policy_snapshot("run_old_schema").await.unwrap();
        assert_eq!(snap.effective_mode, SourceMode::LocalOnly);
    }

    #[tokio::test]
    async fn malformed_tag_json_degrades_to_local_only() {
        let store = store_with_run("run_malformed").await;
        sqlx::query(
            "INSERT INTO run_source_tags (run_id, seq, source_id, schema_version, tag_json, created_at) \
             VALUES (?, 0, 'doc_bad', ?, 'not valid json', '2026-01-01T00:00:00Z')",
        )
        .bind("run_malformed")
        .bind(SOURCE_TAG_SCHEMA_VERSION)
        .execute(store.pool())
        .await
        .unwrap();

        let tags = store.list_run_source_tags("run_malformed").await.unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].source.mode, SourceMode::LocalOnly);
        assert_eq!(tags[0].source.classification, SourceClassification::Unknown);
    }

    #[tokio::test]
    async fn unreadable_columns_degrade_without_panicking_and_keep_message_seq() {
        let store = store_with_run("run_bad_columns").await;
        sqlx::query(
            "INSERT INTO run_source_tags (run_id, seq, source_id, schema_version, tag_json, created_at) \
             VALUES (?, 7, 'doc_bad_columns', 'unknown', 'ignored', '2026-01-01T00:00:00Z')",
        )
        .bind("run_bad_columns")
        .execute(store.pool())
        .await
        .unwrap();

        let tags = store.list_run_source_tags("run_bad_columns").await.unwrap();
        assert_eq!(tags[0].seq, 7);
        assert_eq!(tags[0].source.mode, SourceMode::LocalOnly);
        assert_eq!(tags[0].source.classification, SourceClassification::Unknown);
    }

    #[tokio::test]
    async fn a_tag_whose_payload_names_another_source_is_unknown() {
        // Reverse: without the source_id cross-check, a row could carry an
        // `Ordinary` payload copied from another source and be trusted.
        let store = store_with_run("run_mismatch").await;
        let ordinary = SourceRef::user_input("run_mismatch", "2026-10-08T00:00:00Z");
        sqlx::query(
            "INSERT INTO run_source_tags (run_id, seq, source_id, schema_version, tag_json, created_at) \
             VALUES (?, 0, 'memory_assertion:a1', ?, ?, '2026-10-08T00:00:00Z')",
        )
        .bind("run_mismatch")
        .bind(SOURCE_TAG_SCHEMA_VERSION)
        .bind(serde_json::to_string(&ordinary).unwrap())
        .execute(store.pool())
        .await
        .unwrap();

        let tags = store.list_run_source_tags("run_mismatch").await.unwrap();
        assert_eq!(tags[0].source.source_id, "memory_assertion:a1");
        assert_eq!(tags[0].source.classification, SourceClassification::Unknown);
    }

    #[tokio::test]
    async fn tags_survive_restart_recovery() {
        // Reverse: before persistence was wired, a tag lived only in the
        // in-memory call that created it — closing and reopening the SAME
        // on-disk database would find nothing. This proves the tag is
        // readable by a FRESH `Store` handle over the same file, which is
        // what a daemon restart actually does (`Store::open` re-runs
        // migrations and reconnects; it shares no process state with the
        // handle that wrote the row).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source-tags-restart.db");
        let store = Store::open(&path).await.unwrap();
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at) \
             VALUES ('run_restart', 'queued', '{}', '{}', '2026-10-08T00:00:00Z')",
        )
        .execute(store.pool())
        .await
        .unwrap();
        let tag = SourceRef::user_input("run_restart", "2026-10-08T00:00:00Z");
        store
            .tag_run_source("run_restart", 0, &tag, "2026-10-08T00:00:00Z")
            .await
            .unwrap();
        store.pool().close().await;

        let reopened = Store::open(&path).await.unwrap();
        let tags = reopened.list_run_source_tags("run_restart").await.unwrap();
        assert_eq!(tags[0].seq, 0);
        assert_eq!(tags[0].source, tag);
        let snap = reopened.run_policy_snapshot("run_restart").await.unwrap();
        assert_eq!(snap.effective_mode, SourceMode::LocalOnly);
    }

    #[test]
    fn strictest_classification_orders_restricted_over_unknown_over_ordinary() {
        use SourceClassification::{Ordinary, Restricted, Unknown};
        assert_eq!(strictest_classification([]), Unknown);
        assert_eq!(strictest_classification([Ordinary]), Ordinary);
        assert_eq!(strictest_classification([Ordinary, Ordinary]), Ordinary);
        assert_eq!(strictest_classification([Ordinary, Unknown]), Unknown);
        assert_eq!(
            strictest_classification([Unknown, Restricted, Ordinary]),
            Restricted
        );
        assert_eq!(strictest_classification([Restricted, Unknown]), Restricted);
    }

    #[test]
    fn assertion_source_ref_folds_its_writing_context() {
        use SourceClassification::{Ordinary, Restricted, Unknown};
        let none = AssertionSourceRef::from_sources([]);
        assert_eq!(none.classification, Unknown);
        assert!(none.source_ids.is_empty());
        let ordinary = AssertionSourceRef::from_sources([("user_input:r".to_owned(), Ordinary)]);
        assert_eq!(ordinary.classification, Ordinary);
        assert_eq!(ordinary.source_ids, vec!["user_input:r".to_owned()]);
        assert_eq!(ordinary.schema_version, ASSERTION_SOURCE_REF_SCHEMA_VERSION);
        let mixed = AssertionSourceRef::from_sources([
            ("user_input:r".to_owned(), Ordinary),
            ("memory_assertion:a".to_owned(), Restricted),
        ]);
        assert_eq!(mixed.classification, Restricted);
    }

    #[test]
    fn assertion_source_ref_round_trips_through_its_stored_value() {
        use SourceClassification::{Ordinary, Restricted};
        for classification in [Ordinary, Restricted, SourceClassification::Unknown] {
            let written = AssertionSourceRef::from_sources([("s".to_owned(), classification)]);
            let stored = written.to_value();
            assert_eq!(
                serde_json::from_value::<AssertionSourceRef>(stored.clone()).unwrap(),
                written
            );
            assert_eq!(
                AssertionSourceRef::classification_of(Some(&stored)),
                classification
            );
        }
    }

    #[test]
    fn unreadable_assertion_source_refs_are_unknown_never_ordinary() {
        let unknown = SourceClassification::Unknown;
        let ordinary_v1 =
            AssertionSourceRef::from_sources([("s".to_owned(), SourceClassification::Ordinary)])
                .to_value();
        let mut other_version = ordinary_v1.clone();
        other_version["schema_version"] =
            serde_json::json!(ASSERTION_SOURCE_REF_SCHEMA_VERSION + 1);
        let mut extra_field = ordinary_v1.clone();
        extra_field["trusted"] = serde_json::json!(true);
        let mut bad_classification = ordinary_v1.clone();
        bad_classification["classification"] = serde_json::json!("public");
        for stored in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("ordinary")),
            // The shape P1a's writer test used: no schema version.
            Some(serde_json::json!({"classification": "ordinary"})),
            Some(other_version),
            Some(extra_field),
            Some(bad_classification),
        ] {
            assert_eq!(
                AssertionSourceRef::classification_of(stored.as_ref()),
                unknown,
                "{stored:?}"
            );
        }
    }

    #[test]
    fn a_recalled_memory_tag_carries_the_stored_classification() {
        for classification in [
            SourceClassification::Ordinary,
            SourceClassification::Restricted,
            SourceClassification::Unknown,
        ] {
            let tag = SourceRef::recalled_memory("a-1", classification, "t");
            assert_eq!(tag.classification, classification);
            assert_eq!(tag.kind, SourceKind::MemoryRecall);
            assert_eq!(tag.mode, SourceMode::LocalOnly);
            assert_eq!(tag.source_id, "memory_assertion:a-1");
        }
    }

    #[tokio::test]
    async fn a_session_floor_starts_at_its_first_turn_and_never_lowers() {
        use SourceClassification::{Ordinary, Restricted, Unknown};
        let store = Store::open_memory().await.unwrap();
        let now = "2026-10-25T00:00:00Z";
        let merge = |own, prior| store.merge_session_floor("s1", own, prior, now);
        assert_eq!(merge(Ordinary, false).await.unwrap(), Ordinary);
        assert_eq!(merge(Ordinary, true).await.unwrap(), Ordinary);
        assert_eq!(merge(Restricted, true).await.unwrap(), Restricted);
        // Reverse: an ordinary later turn, with or without prior context.
        assert_eq!(merge(Ordinary, true).await.unwrap(), Restricted);
        assert_eq!(merge(Ordinary, false).await.unwrap(), Restricted);
        assert_eq!(merge(Unknown, true).await.unwrap(), Restricted);
        // Another session is unaffected.
        let other = store
            .merge_session_floor("s2", Ordinary, false, now)
            .await
            .unwrap();
        assert_eq!(other, Ordinary);
    }

    #[tokio::test]
    async fn prior_context_without_a_stored_floor_is_unknown() {
        let store = Store::open_memory().await.unwrap();
        let floor = store
            .merge_session_floor("legacy", SourceClassification::Ordinary, true, "t")
            .await
            .unwrap();
        assert_eq!(floor, SourceClassification::Unknown);
    }

    #[tokio::test]
    async fn concurrent_floor_merges_never_lose_the_strictest() {
        // A file database: `open_memory` has one connection, which would
        // serialize the merges and hide a missing write lock.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("floor.db")).await.unwrap();
        let merges = (0..64).map(|i| {
            let store = store.clone();
            tokio::spawn(async move {
                let own = if i == 7 {
                    SourceClassification::Restricted
                } else {
                    SourceClassification::Ordinary
                };
                store
                    .merge_session_floor("s", own, false, "t")
                    .await
                    .unwrap()
            })
        });
        for merge in merges.collect::<Vec<_>>() {
            merge.await.unwrap();
        }
        let floor = store
            .merge_session_floor("s", SourceClassification::Ordinary, true, "t")
            .await
            .unwrap();
        assert_eq!(floor, SourceClassification::Restricted);
    }

    #[test]
    fn an_unrecognized_stored_floor_reads_as_unknown() {
        assert_eq!(
            classification_from_column("ORDINARY"),
            SourceClassification::Unknown
        );
        assert_eq!(
            classification_from_column(""),
            SourceClassification::Unknown
        );
        for c in [
            SourceClassification::Ordinary,
            SourceClassification::Unknown,
            SourceClassification::Restricted,
        ] {
            assert_eq!(classification_from_column(classification_column(c)), c);
        }
    }

    #[tokio::test]
    async fn a_session_floor_tag_round_trips_with_its_classification() {
        let store = store_with_run("run_floor").await;
        let tag = SourceRef::session_floor("s1", SourceClassification::Restricted, "t");
        store
            .tag_run_source("run_floor", 2, &tag, "t")
            .await
            .unwrap();
        let tags = store.list_run_source_tags("run_floor").await.unwrap();
        assert_eq!(tags[0].source, tag);
        assert_eq!(tags[0].source.kind, SourceKind::SessionContext);
    }

    /// K1-6b.5f-2: a session records the origins of the non-ordinary tool
    /// results its runs took in, once each; a result outside a session, or an
    /// ordinary one, records nothing.
    #[tokio::test]
    async fn a_session_records_the_origins_of_its_non_ordinary_tool_results() {
        use SourceClassification::{Ordinary, Unknown};
        let store = store_with_run("run_tools").await;
        for (seq, origin, classification, session) in [
            (1, "tool:shell_exec", Unknown, Some("s1")),
            (2, "tool:shell_exec", Unknown, Some("s1")),
            (3, "module:docs", Unknown, Some("s1")),
            (4, "tool:fs_read", Ordinary, Some("s1")),
            (5, "tool:mcp_x", Unknown, None),
        ] {
            let tag = SourceRef::tool_result(origin, &format!("tc_{seq}"), classification, "t");
            store
                .tag_tool_result(
                    "run_tools",
                    seq,
                    &tag,
                    session.map(|sid| (sid, origin)),
                    "t",
                )
                .await
                .unwrap();
        }
        assert_eq!(
            store.list_run_source_tags("run_tools").await.unwrap().len(),
            5
        );
        assert_eq!(
            store.session_tool_result_origins("s1").await.unwrap(),
            ["module:docs", "tool:shell_exec"]
        );
        assert!(
            store
                .session_tool_result_origins("s2")
                .await
                .unwrap()
                .is_empty()
        );

        let inherited = SourceRef::session_tool_result("module:docs", "s1", "t");
        assert_eq!(inherited.source_id, "tool_result:module:docs:session:s1");
        assert_eq!(inherited.classification, Unknown);
        assert_eq!(inherited.kind, SourceKind::ToolResult);
    }

    /// Codex 6b.5f-2 r1 C1: a session whose runs called tools before 0031
    /// carries one legacy origin; a session without tool calls, and a tool
    /// call outside any session, record none.
    #[tokio::test]
    async fn a_session_that_called_tools_before_0031_carries_a_legacy_origin() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.migrations = std::borrow::Cow::Owned(
            migrator
                .migrations
                .iter()
                .filter(|m| m.version <= 30)
                .cloned()
                .collect(),
        );
        migrator.run(&pool).await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO sessions (id, title, channel, created_at, updated_at) VALUES \
                ('s_tools', 't', 'api', 't', 't'), ('s_quiet', 't', 'api', 't', 't'); \
             INSERT INTO runs (id, session_id, status, input, usage, created_at) VALUES \
                ('r1', 's_tools', 'completed', '{}', '{}', 't'), \
                ('r2', 's_tools', 'completed', '{}', '{}', 't'), \
                ('r3', 's_quiet', 'completed', '{}', '{}', 't'), \
                ('r4', NULL, 'completed', '{}', '{}', 't'); \
             INSERT INTO tool_calls (id, run_id, tool, input, status, started_at) VALUES \
                ('tc1', 'r1', 'shell_exec', '{}', 'ok', 't'), \
                ('tc2', 'r2', 'docs.find', '{}', 'ok', 't'), \
                ('tc3', 'r4', 'shell_exec', '{}', 'ok', 't');",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let store = crate::test_hooks::from_pool(pool);
        assert_eq!(
            store.session_tool_result_origins("s_tools").await.unwrap(),
            ["tool:legacy"]
        );
        assert!(
            store
                .session_tool_result_origins("s_quiet")
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// K1-6b.5f-3 (David 2026-10-27, B): every session run present at the
    /// upgrade that carries no session floor gets an `unknown` one, which
    /// reads back as the session-context tag the kernel itself would write.
    #[tokio::test]
    async fn a_session_run_without_a_floor_before_0032_gets_an_unknown_one() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.migrations = std::borrow::Cow::Owned(
            migrator
                .migrations
                .iter()
                .filter(|m| m.version <= 31)
                .cloned()
                .collect(),
        );
        migrator.run(&pool).await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO sessions (id, title, channel, created_at, updated_at) VALUES \
                ('s1', 't', 'api', 't', 't'); \
             INSERT INTO runs (id, session_id, status, input, usage, created_at) VALUES \
                ('r_gap', 's1', 'completed', '{}', '{}', 't'), \
                ('r_untagged', 's1', 'completed', '{}', '{}', 't'), \
                ('r_floor', 's1', 'completed', '{}', '{}', 't'), \
                ('r_alone', NULL, 'completed', '{}', '{}', 't');",
        )
        .execute(&pool)
        .await
        .unwrap();
        let store = crate::test_hooks::from_pool(pool);
        for run in ["r_gap", "r_floor", "r_alone"] {
            store
                .tag_run_source(run, 0, &SourceRef::user_input(run, "t"), "t")
                .await
                .unwrap();
        }
        let floor = SourceRef::session_floor("s1", SourceClassification::Ordinary, "t");
        store
            .tag_run_source("r_floor", 1, &floor, "t")
            .await
            .unwrap();
        sqlx::migrate!("./migrations")
            .run(store.pool())
            .await
            .unwrap();

        for run in ["r_gap", "r_untagged"] {
            let floors: Vec<_> = store
                .list_run_source_tags(run)
                .await
                .unwrap()
                .into_iter()
                .filter(|tag| tag.source.kind == SourceKind::SessionContext)
                .collect();
            let [backfilled] = floors.as_slice() else {
                panic!("{run}: one session floor expected, got {floors:?}");
            };
            assert_eq!(
                backfilled.source,
                SourceRef::session_floor(
                    "s1",
                    SourceClassification::Unknown,
                    backfilled.source.created_at.clone()
                ),
                "{run}"
            );
        }
        // A floor the kernel wrote is kept as it was; a run without a
        // session has none to miss.
        let kept: Vec<_> = store
            .list_run_source_tags("r_floor")
            .await
            .unwrap()
            .into_iter()
            .map(|tag| tag.source)
            .filter(|source| source.kind == SourceKind::SessionContext)
            .collect();
        assert_eq!(kept, [floor]);
        assert!(
            store
                .list_run_source_tags("r_alone")
                .await
                .unwrap()
                .iter()
                .all(|tag| tag.source.kind != SourceKind::SessionContext)
        );
    }

    /// K1-6b.5f-2: the tag and the session's origin are written together or
    /// not at all, so a later turn never misses an origin whose result was
    /// tagged.
    #[tokio::test]
    async fn a_tool_result_tag_is_not_kept_without_its_session_origin() {
        let store = store_with_run("run_tools").await;
        sqlx::raw_sql(
            "CREATE TRIGGER no_origin BEFORE INSERT ON session_tool_result_origins \
             BEGIN SELECT RAISE(ABORT, 'no'); END;",
        )
        .execute(store.pool())
        .await
        .unwrap();
        let tag = SourceRef::tool_result(
            "tool:shell_exec",
            "tc_1",
            SourceClassification::Unknown,
            "t",
        );
        assert!(
            store
                .tag_tool_result("run_tools", 1, &tag, Some(("s1", "tool:shell_exec")), "t")
                .await
                .is_err()
        );
        assert!(
            store
                .list_run_source_tags("run_tools")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_schedule_origin_round_trips_and_user_schedules_have_none() {
        let store = Store::open_memory().await.unwrap();
        store
            .record_schedule_origin("wake_1", "run_1", SourceClassification::Restricted, "t")
            .await
            .unwrap();
        assert_eq!(
            store
                .schedule_origin_classification("wake_1")
                .await
                .unwrap(),
            Some(SourceClassification::Restricted)
        );
        assert_eq!(
            store
                .schedule_origin_classification("user_sched")
                .await
                .unwrap(),
            None
        );
        // An origin is recorded once; a second write for the same schedule is
        // an error rather than a silent overwrite.
        assert!(
            store
                .record_schedule_origin("wake_1", "run_2", SourceClassification::Ordinary, "t")
                .await
                .is_err()
        );
    }

    /// A self-wake left from before 0024 carries `unknown`, not "created by
    /// the user" (0026), and so does one deleted since whose run is left; a
    /// user's schedule still has no origin, and one recorded between 0024
    /// and 0026 keeps its own.
    #[tokio::test]
    async fn a_self_wake_from_before_0024_reads_as_unknown() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let up_to = |version: i64| {
            let mut migrator = sqlx::migrate!("./migrations");
            migrator.migrations = std::borrow::Cow::Owned(
                migrator
                    .migrations
                    .iter()
                    .filter(|m| m.version <= version)
                    .cloned()
                    .collect(),
            );
            migrator
        };
        let schedule = |id: &'static str, name: &'static str| {
            sqlx::query(
                "INSERT INTO schedules (id, name, enabled, spec, action, delivery) \
                 VALUES (?, ?, 1, '{}', '{}', '[]')",
            )
            .bind(id)
            .bind(name)
        };
        up_to(18).run(&pool).await.unwrap();
        schedule("wake_old", "self-wake")
            .execute(&pool)
            .await
            .unwrap();
        // Not self-wakes: the prefix is `wake_` exactly (#1278 PR-Daemon F1).
        for id in ["sch_user", "wakeful", "WAKE_up"] {
            schedule(id, "self-wake").execute(&pool).await.unwrap();
        }
        // A self-wake that fired and was then deleted: only its run is left.
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at, schedule_id) \
             VALUES ('run_gone', 'completed', '{}', '{}', '2026-10-08T00:00:00Z', 'wake_gone')",
        )
        .execute(&pool)
        .await
        .unwrap();
        up_to(25).run(&pool).await.unwrap();
        schedule("wake_new", "self-wake")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO schedule_source_origin (schedule_id, origin_run_id, classification, created_at) \
             VALUES ('wake_new', 'run_1', 'restricted', 't')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let store = crate::test_hooks::from_pool(pool);
        let origin = |id: &'static str| store.schedule_origin_classification(id);
        assert_eq!(
            origin("wake_old").await.unwrap(),
            Some(SourceClassification::Unknown)
        );
        assert_eq!(
            origin("wake_gone").await.unwrap(),
            Some(SourceClassification::Unknown)
        );
        for id in ["sch_user", "wakeful", "WAKE_up"] {
            assert_eq!(origin(id).await.unwrap(), None, "{id}");
        }
        assert_eq!(
            origin("wake_new").await.unwrap(),
            Some(SourceClassification::Restricted)
        );
    }

    #[test]
    fn at_least_raises_but_never_lowers_a_classification() {
        use SourceClassification::{Ordinary, Restricted, Unknown};
        let ordinary = AssertionSourceRef::from_sources([("s".to_owned(), Ordinary)]);
        assert_eq!(ordinary.at_least(Restricted).classification, Restricted);
        assert_eq!(ordinary.at_least(Unknown).classification, Unknown);
        assert_eq!(ordinary.at_least(Ordinary).classification, Ordinary);
        let restricted = AssertionSourceRef::from_sources([("s".to_owned(), Restricted)]);
        assert_eq!(restricted.at_least(Ordinary).classification, Restricted);
        assert_eq!(
            restricted.at_least(Ordinary).source_ids,
            restricted.source_ids
        );
    }
}
