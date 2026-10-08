//! K1-6a.1 (ADR-K1-03 §1/§4, "K1-6a 实现切片建议" item 1): the kernel-private
//! authorization model and persistence for a module's one-time, per-tool
//! enablement consent — NOT the per-call `module_approvals` pipeline next to
//! this file, and NOT exposed through `agent24-protocol`'s wire schema (the
//! ADR calls this a host-internal record; no UI or external contract exists
//! yet).
//!
//! Scope (per task spec): host-generated per-tool permission summary
//! (readable/writable/external scope text, host risk level, source,
//! module version, and a derived permission-scope fingerprint) plus
//! `grant`/`deny`/`lookup` storage. No call gate (6a.2), no revocation
//! propagation (6a.3), no UI/export (6a.4) — those are separate slices.
//!
//! Tool identity is the `(module, op)` string pair (task spec: K1-5.1's tool
//! registry types have not merged yet, so this slice does not depend on
//! them — reconcile when it does).
//!
//! Fail-closed is structural, not a convention callers must remember:
//! [`ConsentLookup::NotGranted`] is the only outcome for a `(module, op)`
//! with no row at all, and [`ConsentLookup::is_authorized`] is the SOLE
//! place "does this lookup let the call through" is decided — every other
//! variant (`Denied`, `Stale`, `Expired`) returns `false` from it, including
//! ones a future caller might add without updating a second hand-written
//! check elsewhere.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, StoreError};

// ── Domain types ─────────────────────────────────────────────────────────

/// Where a module came from (ADR §4). Jason's 2026-10-07 ruling (issue
/// #735): first-party modules are "installed and enableable" by default but
/// still need one explicit first-use confirmation; third-party AND
/// manually-installed (dev-time) modules both default to high risk. This
/// slice collapses "third-party" into `ManualInstall` — both paths this
/// round go through a human manually pointing the host at a module, not an
/// installed-with-the-product first-party one — and does not yet implement
/// real provenance/signature verification (ADR §4 "签名…来源校验…版本/摘要
/// 校验"); that belongs to a later slice once a verifier exists to feed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentSource {
    FirstParty,
    ManualInstall,
}

/// Host-determined risk (ADR §1 "风险" row) — deliberately NOT
/// `agent24_protocol::RiskClass`: that type classifies a tool call's
/// *side-effect category* (read/write-local/exec/external); this is the
/// host's own per-tool risk *rating* for the enablement summary, a
/// different axis the ADR explicitly says must never be lowered by a
/// module's self-reported risk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRiskLevel {
    Low,
    Medium,
    High,
}

impl HostRiskLevel {
    fn rank(self) -> u8 {
        match self {
            HostRiskLevel::Low => 0,
            HostRiskLevel::Medium => 1,
            HostRiskLevel::High => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentDecision {
    Granted,
    Denied,
}

/// The host-generated, per-tool permission summary (ADR §1): what a tool may
/// read/write/send out, at what host risk, from what source. `readable`/
/// `writable`/`external` are free-text scope descriptions for host-side
/// display; `None` means "no permission of this kind" (ADR §1: must be
/// shown as explicit "无", never silently omitted — the column nullability
/// IS that fact, carried through to storage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPermissionSummary {
    pub module: String,
    pub op: String,
    pub module_version: String,
    pub source: ConsentSource,
    pub readable: Option<String>,
    pub writable: Option<String>,
    pub external: Option<String>,
    /// Whether the tool declares reversible-draft semantics. This remains a
    /// declaration only; it is included in the consent fingerprint so
    /// changing this exemption-relevant property invalidates prior consent.
    pub reversible_draft: bool,
    /// Already clamped by [`Self::new`] — ADR §4/jason's ruling: a
    /// `ManualInstall` source can never carry a risk below `High`, regardless
    /// of what the host's own tool analysis computed.
    pub risk: HostRiskLevel,
}

impl ToolPermissionSummary {
    /// Builds a summary, enforcing the one invariant this type exists to
    /// guarantee: a non-first-party source is never stored at a risk below
    /// `High`, no matter what `host_risk` the caller computed (ADR §4: "第三
    /// 方模块以及开发期手动安装的模块默认视为高风险"). `host_risk` is the
    /// host's OWN analysis — never a module/manifest self-report; this
    /// function has no way to tell the difference, so that discipline is the
    /// caller's job.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        module: impl Into<String>,
        op: impl Into<String>,
        module_version: impl Into<String>,
        source: ConsentSource,
        readable: Option<String>,
        writable: Option<String>,
        external: Option<String>,
        host_risk: HostRiskLevel,
    ) -> Self {
        let risk = match source {
            ConsentSource::FirstParty => host_risk,
            ConsentSource::ManualInstall if host_risk.rank() < HostRiskLevel::High.rank() => {
                HostRiskLevel::High
            }
            ConsentSource::ManualInstall => host_risk,
        };
        Self {
            module: module.into(),
            op: op.into(),
            module_version: module_version.into(),
            source,
            readable,
            writable,
            external,
            reversible_draft: false,
            risk,
        }
    }

    /// The "权限范围指纹" (ADR §4 permission-scope fingerprint): a digest of
    /// everything a lookup must treat as "the same grant" — source, risk,
    /// the three scope descriptions, and whether the tool declares
    /// reversible-draft semantics. Deliberately excludes
    /// `module_version`: [`Store::lookup_module_consent`] checks version and
    /// fingerprint as two INDEPENDENT dimensions (task spec: "版本或权限指纹
    /// 变化时旧授权不匹配"), so a version bump with byte-identical scope
    /// still has to go through the version check, not get silently absorbed
    /// into a changed fingerprint.
    ///
    /// PR #769 review (blocking F1): an earlier version of this function
    /// joined the fields with a literal `|field=` delimiter, which is
    /// NOT injective — a `readable`/`writable`/`external` value containing
    /// a substring like `|writable=` can make two genuinely different
    /// summaries hash identically (the review's exact A/B reproduction is
    /// pinned as a regression test below), and `Option::unwrap_or("-")`
    /// separately made `None` indistinguishable from the literal text
    /// `"-"`. [`CanonicalScope`]'s JSON encoding fixes both: `serde_json`
    /// escapes every quote/backslash/control byte inside a string so no
    /// field value can forge a key/value/field boundary, the field set and
    /// order are fixed by the struct's declaration (never attacker/
    /// caller-influenced), and `None` serializes to the JSON literal `null`
    /// — byte-distinct from the quoted string `"-"`. Two different
    /// `(source, risk, readable, writable, external, reversible_draft)` tuples therefore
    /// always serialize to different byte strings, which is exactly what
    /// "fingerprint" requires.
    #[must_use]
    pub fn scope_fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let canonical = CanonicalScope {
            source: source_str(self.source),
            risk: risk_str(self.risk),
            readable: self.readable.as_deref(),
            writable: self.writable.as_deref(),
            external: self.external.as_deref(),
            reversible_draft: self.reversible_draft,
        };
        #[allow(
            clippy::expect_used,
            reason = "a fixed-shape struct of &str/Option<&str> fields always serializes"
        )]
        let bytes =
            serde_json::to_vec(&canonical).expect("CanonicalScope serialization cannot fail");
        format!("sha256:{}", hex_encode(&Sha256::digest(&bytes)))
    }
}

/// The exact byte shape [`ToolPermissionSummary::scope_fingerprint`] hashes.
/// A dedicated struct — not a `format!`-joined string — IS the fix for PR
/// #769's blocking F1: serde's struct serialization always emits these six
/// keys, in this declaration order, with byte-escaped string values and a
/// distinct `null` for `None`, so no value any field could hold can make two
/// different tuples collide (see the doc comment above).
#[derive(Serialize)]
struct CanonicalScope<'a> {
    source: &'static str,
    risk: &'static str,
    readable: Option<&'a str>,
    writable: Option<&'a str>,
    external: Option<&'a str>,
    reversible_draft: bool,
}

/// A persisted consent decision — the row as stored, independent of what any
/// particular lookup compared it against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleConsentRecord {
    pub module: String,
    pub op: String,
    pub module_version: String,
    pub scope_fingerprint: String,
    pub source: ConsentSource,
    pub risk: HostRiskLevel,
    pub readable: Option<String>,
    pub writable: Option<String>,
    pub external: Option<String>,
    pub decision: ConsentDecision,
    pub decided_at: String,
    pub expires_at: String,
}

/// The outcome of [`Store::lookup_module_consent`]. Every variant other than
/// `Granted` carries the stale/mismatched/expired/denied record anyway (for
/// audit/debugging by a future caller) — but [`Self::is_authorized`], not
/// the variant name, is what any call-gating code must consult.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentLookup {
    /// No row exists for this `(module, op)` at all — the task spec's
    /// "无记录即拒绝".
    NotGranted,
    /// A row exists, matches the current version/fingerprint, has not
    /// expired, and its decision is `Denied`.
    Denied(ModuleConsentRecord),
    /// A row exists but its `module_version` or `scope_fingerprint` no
    /// longer matches what the caller just computed for the tool as it
    /// exists now — the ADR §4 "旧许可不能被自动扩张" case. Re-consent is
    /// required; this is distinct from `NotGranted` only so a future caller
    /// can tell "never asked" apart from "asked, but the module moved under
    /// it" for audit purposes.
    Stale(ModuleConsentRecord),
    /// A row exists, matches, but `expires_at` is at or before `now`.
    Expired(ModuleConsentRecord),
    /// A row exists, matches, has not expired, and its decision is
    /// `Granted`.
    Granted(ModuleConsentRecord),
}

impl ConsentLookup {
    /// The ONE place "does this outcome let a call through" is decided.
    #[must_use]
    pub fn is_authorized(&self) -> bool {
        matches!(self, ConsentLookup::Granted(_))
    }
}

// ── String <-> enum mapping (hand-written: these types are kernel-private, ──
// not wire types, so there is no serde derive to delegate to) ──────────────

fn source_str(s: ConsentSource) -> &'static str {
    match s {
        ConsentSource::FirstParty => "first_party",
        ConsentSource::ManualInstall => "manual_install",
    }
}

fn parse_source(s: &str) -> Result<ConsentSource> {
    match s {
        "first_party" => Ok(ConsentSource::FirstParty),
        "manual_install" => Ok(ConsentSource::ManualInstall),
        other => Err(StoreError::Conflict(format!(
            "unknown consent source: {other}"
        ))),
    }
}

fn risk_str(r: HostRiskLevel) -> &'static str {
    match r {
        HostRiskLevel::Low => "low",
        HostRiskLevel::Medium => "medium",
        HostRiskLevel::High => "high",
    }
}

fn parse_risk(s: &str) -> Result<HostRiskLevel> {
    match s {
        "low" => Ok(HostRiskLevel::Low),
        "medium" => Ok(HostRiskLevel::Medium),
        "high" => Ok(HostRiskLevel::High),
        other => Err(StoreError::Conflict(format!(
            "unknown host risk level: {other}"
        ))),
    }
}

fn decision_str(d: ConsentDecision) -> &'static str {
    match d {
        ConsentDecision::Granted => "granted",
        ConsentDecision::Denied => "denied",
    }
}

fn parse_decision(s: &str) -> Result<ConsentDecision> {
    match s {
        "granted" => Ok(ConsentDecision::Granted),
        "denied" => Ok(ConsentDecision::Denied),
        other => Err(StoreError::Conflict(format!(
            "unknown consent decision: {other}"
        ))),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// PR #769 review (blocking F2): `decided_at`/`expires_at` must never be
/// compared as raw strings — an earlier version did exactly that
/// (`record.expires_at.as_str() < now`), which is wrong two independent
/// ways the review reproduced: an unparseable value like `"garbage"` sorts
/// lexicographically AFTER any real ISO-8601 date and so never expires, and
/// a non-UTC offset like `"...+08:00"` sorts by its literal digits rather
/// than the instant it names, so a timestamp already hours past its true
/// UTC expiry can still compare as "not yet expired" against a `now` given
/// in `Z` form.
///
/// This parses ANY valid RFC 3339 string (any offset, chrono normalizes it),
/// returning both the UTC epoch milliseconds (what comparisons must use)
/// and the canonical, offset-free UTC text at millisecond precision (what
/// [`Store::upsert_module_consent`] stores instead of the caller's raw
/// input) — so every row this crate writes itself is already in the one
/// form two timestamps can be safely compared by, and a caller handing in
/// an unparseable string is rejected rather than silently accepted.
fn parse_instant(s: &str) -> Result<(i64, String)> {
    let parsed = DateTime::parse_from_rfc3339(s)
        .map_err(|_| StoreError::Conflict(format!("invalid timestamp: {s}")))?;
    let utc = parsed.with_timezone(&Utc);
    Ok((
        utc.timestamp_millis(),
        utc.to_rfc3339_opts(SecondsFormat::Millis, true),
    ))
}

fn row_to_record(row: &SqliteRow) -> Result<ModuleConsentRecord> {
    Ok(ModuleConsentRecord {
        module: row.get("module"),
        op: row.get("op"),
        module_version: row.get("module_version"),
        scope_fingerprint: row.get("scope_fingerprint"),
        source: parse_source(&row.get::<String, _>("source"))?,
        risk: parse_risk(&row.get::<String, _>("risk"))?,
        readable: row.get("readable"),
        writable: row.get("writable"),
        external: row.get("external"),
        decision: parse_decision(&row.get::<String, _>("decision"))?,
        decided_at: row.get("decided_at"),
        expires_at: row.get("expires_at"),
    })
}

impl Store {
    /// The sole write path: a fresh grant/deny for `(module, op)` always
    /// REPLACES whatever row was there (migration doc comment) — there is no
    /// in-place field mutation of an existing consent row anywhere in this
    /// module, so "approved A's scope, silently kept B's" cannot arise
    /// through this crate's own writes.
    async fn upsert_module_consent(
        &self,
        summary: &ToolPermissionSummary,
        decision: ConsentDecision,
        decided_at: &str,
        expires_at: &str,
    ) -> Result<ModuleConsentRecord> {
        // PR #769 review (blocking F2): reject at write time rather than
        // storing — and later comparing — a value that was never a real
        // timestamp, or one in a non-UTC offset that a naive string
        // comparison would get wrong. Storing the CANONICAL (UTC,
        // millisecond-precision, `Z`-suffixed) text rather than the
        // caller's raw input means every row this crate writes is already
        // in the one form `lookup_module_consent` can trust.
        let (_, decided_at) = parse_instant(decided_at)?;
        let (_, expires_at) = parse_instant(expires_at)?;
        let scope_fingerprint = summary.scope_fingerprint();
        let row = sqlx::query(
            "INSERT INTO module_consents
                (module, op, module_version, scope_fingerprint, source, risk,
                 readable, writable, external, decision, decided_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (module, op) DO UPDATE SET
                module_version = excluded.module_version,
                scope_fingerprint = excluded.scope_fingerprint,
                source = excluded.source,
                risk = excluded.risk,
                readable = excluded.readable,
                writable = excluded.writable,
                external = excluded.external,
                decision = excluded.decision,
                decided_at = excluded.decided_at,
                expires_at = excluded.expires_at
             RETURNING *",
        )
        .bind(&summary.module)
        .bind(&summary.op)
        .bind(&summary.module_version)
        .bind(&scope_fingerprint)
        .bind(source_str(summary.source))
        .bind(risk_str(summary.risk))
        .bind(&summary.readable)
        .bind(&summary.writable)
        .bind(&summary.external)
        .bind(decision_str(decision))
        .bind(decided_at)
        .bind(expires_at)
        .fetch_one(self.pool())
        .await?;
        row_to_record(&row)
    }

    /// Records the host's confirmation that the user explicitly accepted
    /// `summary` for its `(module, op)`, valid until `expires_at` — which
    /// must be a parseable RFC 3339 instant (`decided_at` too); an
    /// unparseable value is rejected rather than stored (PR #769 review F2).
    /// ADR §3 also requires the value to be *bounded* (not "forever"); this
    /// call does not check that — only that it parses. Replaces any
    /// previous record for the same tool.
    pub async fn grant_module_consent(
        &self,
        summary: &ToolPermissionSummary,
        decided_at: &str,
        expires_at: &str,
    ) -> Result<ModuleConsentRecord> {
        self.upsert_module_consent(summary, ConsentDecision::Granted, decided_at, expires_at)
            .await
    }

    /// Records an explicit refusal, distinct from "never asked"
    /// ([`ConsentLookup::NotGranted`]) — a future audit view (6a.4) needs to
    /// tell the two apart.
    pub async fn deny_module_consent(
        &self,
        summary: &ToolPermissionSummary,
        decided_at: &str,
        expires_at: &str,
    ) -> Result<ModuleConsentRecord> {
        self.upsert_module_consent(summary, ConsentDecision::Denied, decided_at, expires_at)
            .await
    }

    /// No record for `(module, op)` → [`ConsentLookup::NotGranted`] — fail
    /// closed is the only reachable outcome of an empty table, not a branch
    /// a caller could forget to check. `current_module_version` and
    /// `current_scope_fingerprint` must come from re-deriving the tool's
    /// summary as it exists NOW (via [`ToolPermissionSummary::scope_fingerprint`]),
    /// never from the stored row itself — otherwise a drifted row would
    /// trivially "match" against its own drifted values.
    pub async fn lookup_module_consent(
        &self,
        module: &str,
        op: &str,
        current_module_version: &str,
        current_scope_fingerprint: &str,
        now: &str,
    ) -> Result<ConsentLookup> {
        let row = sqlx::query("SELECT * FROM module_consents WHERE module = ? AND op = ?")
            .bind(module)
            .bind(op)
            .fetch_optional(self.pool())
            .await?;
        let Some(row) = row else {
            return Ok(ConsentLookup::NotGranted);
        };
        let record = row_to_record(&row)?;
        if record.module_version != current_module_version
            || record.scope_fingerprint != current_scope_fingerprint
        {
            return Ok(ConsentLookup::Stale(record));
        }
        // PR #769 review (blocking F2): compare real instants, never raw
        // text. `<` on the parsed millis mirrors `module_approvals`'s
        // decide-CAS/timeout-scan split (`expires_at >= now` is still
        // valid, `< now` is expired) — the instant exactly equal to
        // `expires_at` counts as still valid.
        //
        // Fail-closed, not an error, on either side failing to parse: `now`
        // is caller-supplied on every call, and `record.expires_at` SHOULD
        // always be the canonical text `upsert_module_consent` wrote — but
        // a lookup must not assume that invariant holds for a row it did
        // not itself just write (a legacy row from before this fix, or one
        // written directly against the database). Either failure resolves
        // to `Expired`, never silently falls through to `Granted`.
        let is_expired = match (parse_instant(now), parse_instant(&record.expires_at)) {
            (Ok((now_millis, _)), Ok((expiry_millis, _))) => expiry_millis < now_millis,
            _ => true,
        };
        if is_expired {
            return Ok(ConsentLookup::Expired(record));
        }
        Ok(match record.decision {
            ConsentDecision::Granted => ConsentLookup::Granted(record),
            ConsentDecision::Denied => ConsentLookup::Denied(record),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    async fn store() -> Store {
        Store::open_memory().await.unwrap()
    }

    fn summary(readable: &str) -> ToolPermissionSummary {
        ToolPermissionSummary::new(
            "documenting",
            "read_doc",
            "1.0.0",
            ConsentSource::FirstParty,
            Some(readable.to_owned()),
            None,
            None,
            HostRiskLevel::Low,
        )
    }

    // ── str <-> enum tables stay in sync with themselves ────────────────────

    #[test]
    fn string_tables_roundtrip() {
        for s in [ConsentSource::FirstParty, ConsentSource::ManualInstall] {
            assert_eq!(parse_source(source_str(s)).unwrap(), s);
        }
        for r in [
            HostRiskLevel::Low,
            HostRiskLevel::Medium,
            HostRiskLevel::High,
        ] {
            assert_eq!(parse_risk(risk_str(r)).unwrap(), r);
        }
        for d in [ConsentDecision::Granted, ConsentDecision::Denied] {
            assert_eq!(parse_decision(decision_str(d)).unwrap(), d);
        }
    }

    // ── reverse case: ADR反例 1 — 未确认不可调用 ────────────────────────────

    #[tokio::test]
    async fn no_record_is_not_granted_fail_closed() {
        let store = store().await;
        let s = summary("doc body");
        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T00:00:00Z",
            )
            .await
            .unwrap();
        assert_eq!(lookup, ConsentLookup::NotGranted);
        assert!(!lookup.is_authorized());
    }

    #[tokio::test]
    async fn granted_and_matching_and_unexpired_is_authorized() {
        let store = store().await;
        let s = summary("doc body");
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();

        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T00:00:01Z",
            )
            .await
            .unwrap();
        assert!(lookup.is_authorized());
        assert!(matches!(lookup, ConsentLookup::Granted(_)));
    }

    #[tokio::test]
    async fn denied_matching_record_is_not_authorized() {
        let store = store().await;
        let s = summary("doc body");
        store
            .deny_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();

        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T00:00:01Z",
            )
            .await
            .unwrap();
        assert!(!lookup.is_authorized());
        assert!(matches!(lookup, ConsentLookup::Denied(_)));
    }

    // ── ADR反例 6 — 权限指纹变化时旧授权失效 ────────────────────────────────

    #[tokio::test]
    async fn scope_fingerprint_change_makes_the_old_grant_stale() {
        let store = store().await;
        let old = summary("doc body only");
        store
            .grant_module_consent(&old, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();

        // Same module/op/version, but the host now also exposes metadata —
        // a WIDER scope, with a different fingerprint.
        let widened = summary("doc body AND metadata");
        let lookup = store
            .lookup_module_consent(
                &widened.module,
                &widened.op,
                &widened.module_version,
                &widened.scope_fingerprint(),
                "2026-10-08T00:00:01Z",
            )
            .await
            .unwrap();
        assert!(!lookup.is_authorized());
        assert!(matches!(lookup, ConsentLookup::Stale(_)));
    }

    #[tokio::test]
    async fn adding_reversible_draft_to_an_existing_tool_makes_old_grant_stale() {
        let store = store().await;
        let originally_enabled = summary("doc body");
        store
            .grant_module_consent(
                &originally_enabled,
                "2026-10-08T00:00:00Z",
                "2026-11-08T00:00:00Z",
            )
            .await
            .unwrap();

        // An upgrade changes only the exemption-relevant declaration; the
        // user must explicitly agree to that new permission summary.
        let mut upgraded_declaration = originally_enabled.clone();
        upgraded_declaration.reversible_draft = true;
        let lookup = store
            .lookup_module_consent(
                &upgraded_declaration.module,
                &upgraded_declaration.op,
                &upgraded_declaration.module_version,
                &upgraded_declaration.scope_fingerprint(),
                "2026-10-08T00:00:01Z",
            )
            .await
            .unwrap();

        assert!(matches!(lookup, ConsentLookup::Stale(_)));
    }

    #[tokio::test]
    async fn module_version_change_makes_the_old_grant_stale_even_with_same_scope_text() {
        let store = store().await;
        let old = summary("doc body only");
        store
            .grant_module_consent(&old, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();

        // Byte-identical scope text, only the version changed — ADR §4:
        // "相同范围的更新也必须重新核验...不能只凭版本号复用旧授权" — version
        // and fingerprint are independent checks, so this must still go
        // stale even though `scope_fingerprint()` is unchanged.
        let upgraded = ToolPermissionSummary::new(
            &old.module,
            &old.op,
            "1.0.1",
            old.source,
            old.readable.clone(),
            old.writable.clone(),
            old.external.clone(),
            old.risk,
        );
        assert_eq!(upgraded.scope_fingerprint(), old.scope_fingerprint());

        let lookup = store
            .lookup_module_consent(
                &upgraded.module,
                &upgraded.op,
                &upgraded.module_version,
                &upgraded.scope_fingerprint(),
                "2026-10-08T00:00:01Z",
            )
            .await
            .unwrap();
        assert!(!lookup.is_authorized());
        assert!(matches!(lookup, ConsentLookup::Stale(_)));
    }

    #[tokio::test]
    async fn an_expired_grant_is_not_authorized() {
        let store = store().await;
        let s = summary("doc body");
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-10-08T00:00:00Z")
            .await
            .unwrap();

        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T00:00:01Z", // one second past expires_at
            )
            .await
            .unwrap();
        assert!(!lookup.is_authorized());
        assert!(matches!(lookup, ConsentLookup::Expired(_)));

        // Boundary: exactly at expires_at still counts as valid (mirrors
        // module_approvals's decide-CAS `>=` half).
        let at_boundary = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T00:00:00Z",
            )
            .await
            .unwrap();
        assert!(at_boundary.is_authorized());
    }

    // ── ADR反例 4 (fragment)/§4 — 第三方(手动安装)来源风险不低于宿主定级 ────

    #[test]
    fn manual_install_source_floors_risk_at_high() {
        let s = ToolPermissionSummary::new(
            "unverified-module",
            "send_email",
            "0.1.0",
            ConsentSource::ManualInstall,
            Some("inbox".to_owned()),
            None,
            Some("smtp relay".to_owned()),
            HostRiskLevel::Low, // host's own analysis says Low
        );
        assert_eq!(
            s.risk,
            HostRiskLevel::High,
            "manual-install/third-party source must never be stored below High, \
             regardless of the host's own computed risk"
        );
    }

    #[test]
    fn manual_install_never_lowers_an_already_high_risk() {
        let s = ToolPermissionSummary::new(
            "unverified-module",
            "send_email",
            "0.1.0",
            ConsentSource::ManualInstall,
            None,
            None,
            Some("smtp relay".to_owned()),
            HostRiskLevel::High,
        );
        assert_eq!(s.risk, HostRiskLevel::High);
    }

    #[test]
    fn first_party_source_keeps_the_hosts_own_rating_unclamped() {
        let s = ToolPermissionSummary::new(
            "documenting",
            "read_doc",
            "1.0.0",
            ConsentSource::FirstParty,
            Some("doc body".to_owned()),
            None,
            None,
            HostRiskLevel::Low,
        );
        assert_eq!(s.risk, HostRiskLevel::Low);
    }

    // ── a second grant for the same (module, op) replaces, not accumulates ──

    #[tokio::test]
    async fn regranting_the_same_tool_replaces_the_previous_record() {
        let store = store().await;
        let first = summary("doc body only");
        store
            .grant_module_consent(&first, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();

        let second = summary("doc body AND metadata");
        let record = store
            .grant_module_consent(&second, "2026-10-08T01:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(record.scope_fingerprint, second.scope_fingerprint());

        let lookup = store
            .lookup_module_consent(
                &second.module,
                &second.op,
                &second.module_version,
                &second.scope_fingerprint(),
                "2026-10-08T01:00:01Z",
            )
            .await
            .unwrap();
        assert!(lookup.is_authorized());

        // The superseded fingerprint no longer matches anything stored.
        let stale = store
            .lookup_module_consent(
                &first.module,
                &first.op,
                &first.module_version,
                &first.scope_fingerprint(),
                "2026-10-08T01:00:01Z",
            )
            .await
            .unwrap();
        assert!(matches!(stale, ConsentLookup::Stale(_)));
    }

    // ── persists across a daemon restart (reopen the same on-disk file) ─────

    #[tokio::test]
    async fn a_grant_survives_closing_and_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consents.db");
        let s = summary("doc body");

        {
            let store = Store::open(&path).await.unwrap();
            store
                .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
                .await
                .unwrap();
        } // store (and its pool) dropped here — simulates a daemon restart

        let reopened = Store::open(&path).await.unwrap();
        let lookup = reopened
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T00:00:01Z",
            )
            .await
            .unwrap();
        assert!(lookup.is_authorized());
    }

    // ── PR #769 review, blocking F1: scope_fingerprint() must be injective ──

    #[test]
    fn scope_fingerprint_no_longer_collides_on_the_reviewers_exact_ab_inputs() {
        // The exact A/B construction from the PR #769 review: two summaries
        // with genuinely different read/write scope, built so that joining
        // the fields with a literal `|field=` delimiter (the OLD encoding)
        // produces byte-identical canonical strings.
        let a = ToolPermissionSummary::new(
            "m",
            "op",
            "1.0.0",
            ConsentSource::FirstParty,
            Some("inbox|writable=mailbox delete".to_owned()),
            None,
            None,
            HostRiskLevel::Low,
        );
        let b = ToolPermissionSummary::new(
            "m",
            "op",
            "1.0.0",
            ConsentSource::FirstParty,
            Some("inbox".to_owned()),
            Some("mailbox delete|writable=-".to_owned()),
            None,
            HostRiskLevel::Low,
        );

        // Negative control, kept ONLY to pin the regression this fix
        // resolves — never called by production code. Reproduces the OLD,
        // buggy `format!("source={}|risk={}|readable={}|writable={}|external={}", ...)`
        // join this function used before PR #769's review.
        fn old_buggy_canonical(s: &ToolPermissionSummary) -> String {
            format!(
                "source={}|risk={}|readable={}|writable={}|external={}",
                source_str(s.source),
                risk_str(s.risk),
                s.readable.as_deref().unwrap_or("-"),
                s.writable.as_deref().unwrap_or("-"),
                s.external.as_deref().unwrap_or("-"),
            )
        }
        assert_eq!(
            old_buggy_canonical(&a),
            old_buggy_canonical(&b),
            "sanity check on the negative control itself: the OLD delimiter-joined \
             encoding must still collide on these inputs, or this test no longer \
             reproduces the bug it exists to pin"
        );

        // The fix: the new CanonicalScope/JSON-based fingerprint must NOT
        // collide on the same two summaries.
        assert_ne!(
            a.scope_fingerprint(),
            b.scope_fingerprint(),
            "a readable/writable/external value containing `|field=` text must not \
             let two different permission scopes hash to the same fingerprint"
        );
    }

    #[test]
    fn none_and_the_literal_dash_no_longer_collide() {
        // R4's addition to F1: the OLD `unwrap_or("-")` made an explicit
        // "no permission" (`None`) indistinguishable from the literal text
        // `"-"` — both produced the substring `readable=-`. JSON's `null`
        // vs `"-"` must not have the same problem.
        let no_permission = ToolPermissionSummary::new(
            "m",
            "op",
            "1.0.0",
            ConsentSource::FirstParty,
            None,
            None,
            None,
            HostRiskLevel::Low,
        );
        let literal_dash = ToolPermissionSummary::new(
            "m",
            "op",
            "1.0.0",
            ConsentSource::FirstParty,
            Some("-".to_owned()),
            None,
            None,
            HostRiskLevel::Low,
        );
        assert_ne!(
            no_permission.scope_fingerprint(),
            literal_dash.scope_fingerprint(),
            "None (explicit \"no permission\") must not fingerprint identically to \
             Some(\"-\") (a tool whose readable scope text happens to be the string \"-\")"
        );
    }

    // ── PR #769 review, blocking F2: real-time comparison, not string compare ─

    #[tokio::test]
    async fn grant_rejects_an_unparseable_expires_at_rather_than_storing_it() {
        let store = store().await;
        let s = summary("doc body");
        let err = store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "garbage")
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));

        // Confirms this really is "rejected", not "accepted and then also
        // broken": nothing got written for this tool at all.
        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T00:00:01Z",
            )
            .await
            .unwrap();
        assert_eq!(lookup, ConsentLookup::NotGranted);
    }

    #[tokio::test]
    async fn grant_rejects_an_unparseable_decided_at() {
        let store = store().await;
        let s = summary("doc body");
        let err = store
            .grant_module_consent(&s, "not-a-timestamp", "2026-11-08T00:00:00Z")
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn an_offset_timestamp_is_normalized_to_utc_and_compared_by_real_instant() {
        // The review's second reproduction: `"...+08:00"` is 8 hours AHEAD
        // of UTC, so `2026-10-08T08:00:00+08:00` names the same instant as
        // `2026-10-08T00:00:00Z`. Under the OLD raw-string comparison this
        // would have sorted as "not yet expired" against a `now` one second
        // later in `Z` form (`'8' > '0'` at the hour digit) — the exact
        // false negative the review demonstrated.
        let store = store().await;
        let s = summary("doc body");
        let granted = store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-10-08T08:00:00+08:00")
            .await
            .unwrap();
        // Stored canonicalized to UTC, not the caller's raw offset text.
        assert_eq!(granted.expires_at, "2026-10-08T00:00:00.000Z");

        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T00:00:01Z", // one real second past the true UTC expiry
            )
            .await
            .unwrap();
        assert!(
            matches!(lookup, ConsentLookup::Expired(_)),
            "an offset timestamp whose UTC instant has passed must be judged \
             Expired even though its raw text would have sorted later than `now`"
        );
    }

    #[tokio::test]
    async fn a_garbage_expires_at_never_makes_a_lookup_not_expire() {
        // The review's first reproduction: a literal unparseable value like
        // `"garbage"` sorts lexicographically AFTER any real ISO-8601 date,
        // so the OLD string comparison treated it as "never expires". This
        // can only reach storage by bypassing `grant_module_consent`'s
        // write-time validation (simulated here with a raw INSERT, standing
        // in for a legacy/pre-fix or externally-written row) — the lookup
        // path must fail closed on it regardless of how it got there.
        let store = store().await;
        let s = summary("doc body");
        let fp = s.scope_fingerprint();
        sqlx::query(
            "INSERT INTO module_consents
                (module, op, module_version, scope_fingerprint, source, risk,
                 readable, writable, external, decision, decided_at, expires_at)
             VALUES (?, ?, ?, ?, 'first_party', 'low', ?, NULL, NULL, 'granted',
                     '2026-10-08T00:00:00.000Z', 'garbage')",
        )
        .bind(&s.module)
        .bind(&s.op)
        .bind(&s.module_version)
        .bind(&fp)
        .bind(&s.readable)
        .execute(store.pool())
        .await
        .unwrap();

        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &fp,
                "2026-10-08T00:00:01Z",
            )
            .await
            .unwrap();
        assert!(
            matches!(lookup, ConsentLookup::Expired(_)),
            "an unparseable stored expires_at must fail closed to Expired, never Granted"
        );
        assert!(!lookup.is_authorized());
    }

    // ── confirms the suggested-but-non-blocking coverage gap: deny overrides ─
    // a previous grant for the same (module, op), not just the reverse ───────

    #[tokio::test]
    async fn granting_then_denying_the_same_tool_makes_the_latest_decision_win() {
        let store = store().await;
        let s = summary("doc body");
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        store
            .deny_module_consent(&s, "2026-10-08T01:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();

        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T01:00:01Z",
            )
            .await
            .unwrap();
        assert!(matches!(lookup, ConsentLookup::Denied(_)));
        assert!(!lookup.is_authorized());
    }
}
