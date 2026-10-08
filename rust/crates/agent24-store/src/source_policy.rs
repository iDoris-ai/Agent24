//! K1-6b.1 (ADR-K1-02 §6 slice 6b.1) — source tags & persistent run
//! metadata, storage side. See `migrations/0015_run_source_tags.sql` for the
//! schema.
//!
//! This module defines the kernel-private `SourceRef`/`PolicySnapshot`
//! contract: the label a trusted host attaches to a block of content
//! entering a run's context (canonical source id, revision digest,
//! processing mode, policy version, optional authorization reference), and
//! persists/reads it back associated with a run and one of its messages.
//!
//! **Write-only in this slice.** Nothing in Agent24 yet consumes these tags
//! to gate a model call, a tool, or any other output — that is
//! ADR-K1-02 §6 6b.2/6b.3. This slice changes no outbound behavior.
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
pub const SOURCE_TAG_SCHEMA_VERSION: i64 = 1;

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
    /// A fresh, unclassified tag for user input at run entry. Always
    /// `LocalOnly`: nothing upstream of this call classifies or authorizes
    /// user input for cloud processing, so the only safe default is the
    /// fail-closed one (ADR-K1-02 §0).
    pub fn user_input(run_id: &str, created_at: impl Into<String>) -> Self {
        SourceRef {
            source_id: format!("user_input:{run_id}"),
            kind: SourceKind::UserInput,
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
            kind: SourceKind::SelectedMaterial,
            revision_digest: None,
            mode: SourceMode::LocalOnly,
            policy_version: 0,
            authorization_ref: None,
            created_at,
        }
    }
}

/// A run's effective source policy, folded from every tag persisted against
/// it (ADR-K1-02 §2.1: "会话由宿主汇总消息和被引用资料的标签，生成 run 的
/// 有效政策"). Write-only in this slice — nothing consumes `effective_mode`
/// to gate a call yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySnapshot {
    pub schema_version: i64,
    pub effective_mode: SourceMode,
    pub sources: Vec<SourceRef>,
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
        if sources.is_empty() || sources.iter().any(|s| s.mode == SourceMode::LocalOnly) {
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
fn decode_tag_row(row: &SqliteRow) -> SourceRef {
    let source_id: String = row.get("source_id");
    let created_at: String = row.get("created_at");
    let schema_version: i64 = row.get("schema_version");
    if schema_version != SOURCE_TAG_SCHEMA_VERSION {
        return SourceRef::fail_closed_unknown(source_id, created_at);
    }
    let tag_json: String = row.get("tag_json");
    match serde_json::from_str::<SourceRef>(&tag_json) {
        Ok(parsed) if parsed.source_id == source_id => parsed,
        _ => SourceRef::fail_closed_unknown(source_id, created_at),
    }
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
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Every tag persisted against a run, in `(seq, source_id)` order.
    /// Empty for a run with none — callers needing the fail-closed default
    /// use [`Store::run_policy_snapshot`] instead of treating an empty
    /// result as "unrestricted".
    pub async fn list_run_source_tags(&self, run_id: &str) -> Result<Vec<SourceRef>> {
        let rows = sqlx::query(
            "SELECT source_id, schema_version, tag_json, created_at \
             FROM run_source_tags WHERE run_id = ? ORDER BY seq ASC, source_id ASC",
        )
        .bind(run_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(decode_tag_row).collect())
    }

    /// The run's fail-closed effective policy (ADR-K1-02 §2.1), folded from
    /// every persisted tag. `LocalOnly` when the run has none.
    pub async fn run_policy_snapshot(&self, run_id: &str) -> Result<PolicySnapshot> {
        let tags = self.list_run_source_tags(run_id).await?;
        Ok(PolicySnapshot::from_tags(tags))
    }
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
    }

    #[test]
    fn policy_snapshot_fold_is_strictest_wins() {
        let local = SourceRef::user_input("run_1", "t");
        let mut cloud = SourceRef::selected_material("doc_1", None, "t");
        cloud.mode = SourceMode::CloudAuthorized;

        // All-cloud folds to cloud.
        let snap = PolicySnapshot::from_tags(vec![cloud.clone()]);
        assert_eq!(snap.effective_mode, SourceMode::CloudAuthorized);

        // One LocalOnly tag forces the whole snapshot LocalOnly, even mixed
        // with a cloud-authorized one.
        let snap = PolicySnapshot::from_tags(vec![cloud, local]);
        assert_eq!(snap.effective_mode, SourceMode::LocalOnly);
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
        assert_eq!(tags, vec![tag]);
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
        assert_eq!(tags[0].mode, SourceMode::LocalOnly);
        assert_eq!(tags[0].source_id, "doc_legacy");

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
        assert_eq!(tags[0].mode, SourceMode::LocalOnly);
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
        assert_eq!(tags, vec![tag]);
        let snap = reopened.run_policy_snapshot("run_restart").await.unwrap();
        assert_eq!(snap.effective_mode, SourceMode::LocalOnly);
    }
}
