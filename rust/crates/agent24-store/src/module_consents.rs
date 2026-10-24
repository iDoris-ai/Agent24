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
//! `grant`/`deny`/`lookup` storage and durable 6a.3 revocation state. No call
//! gate (6a.2), no UI/export (6a.4) — those are separate slices.
//!
//! Tool identity remains the `(module, op)` string pair in this persistence
//! contract. It stays independent of capability claims and runtime handles.
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
    /// The resource set as stored (K1-6a-G §2.2): empty when the row has
    /// none. Read back as written, not re-validated: a dispatch checks it.
    pub resources: Vec<String>,
    /// The artifact identity the user confirmed; `None` if none was.
    pub artifact: Option<String>,
    /// The write generation (see the migration): increasing, never reused.
    pub generation: i64,
}

/// Most resources one grant may carry (resource contract §2.1).
pub const MAX_GRANT_RESOURCES: usize = 256;

/// What a grant covers beyond its tool's permission summary (K1-6a-G
/// §2.2): the resources the user picked and the artifact identity the card
/// showed. The default is no resources and no artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantScope {
    pub resources: Vec<String>,
    pub artifact: Option<String>,
}

/// Whether `element` is a resource as the resource contract §2.1 defines
/// it: `<kind>:<id>`, split at the first `:`, `kind` matching
/// `[a-z][a-z0-9_]{0,31}` and `id` matching `[A-Za-z0-9_-]{1,128}`.
#[must_use]
pub fn is_resource_element(element: &str) -> bool {
    let Some((kind, id)) = element.split_once(':') else {
        return false;
    };
    let kind = kind.as_bytes();
    let Some((first, rest)) = kind.split_first() else {
        return false;
    };
    if kind.len() > 32
        || !first.is_ascii_lowercase()
        || !rest
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        return false;
    }
    let id = id.as_bytes();
    !id.is_empty()
        && id.len() <= 128
        && id
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-')
}

/// The digest of a resource set (K1-6a-G §2.2), as 64 lowercase hex
/// digits: SHA-256 over each distinct element in byte order, each written
/// as its length (u64, big-endian) then its bytes. Order and repeats do
/// not change it; no two sets share an encoding.
#[must_use]
pub fn resource_set_digest(resources: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut distinct: Vec<&String> = resources.iter().collect();
    distinct.sort();
    distinct.dedup();
    let mut encoded = Vec::new();
    for element in distinct {
        encoded.extend_from_slice(&(element.len() as u64).to_be_bytes());
        encoded.extend_from_slice(element.as_bytes());
    }
    hex_encode(&Sha256::digest(&encoded))
}

/// What the audit records for a dispatched resource set (K1-6a-G §2.2,
/// resource contract §2.4): `rs:<n>:<digest>`, `n` the number of distinct
/// resources and `digest` their [`resource_set_digest`]. Never an ID.
#[must_use]
pub fn resource_set_ref(resources: &[String]) -> String {
    let mut distinct: Vec<&String> = resources.iter().collect();
    distinct.sort();
    distinct.dedup();
    let n = distinct.len();
    let digest = resource_set_digest(resources);
    format!("rs:{n}:{digest}")
}

fn append_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

impl ModuleConsentRecord {
    /// The grant reference (K1-6a-G §2.2): `ga1:` and 64 lowercase hex
    /// digits, the SHA-256 of, in order, `ga1`, the generation (8 bytes,
    /// big-endian), the permission fingerprint, the resource set digest,
    /// the artifact identity (`0` when there is none, else `1` and the
    /// identity), `decided_at` and `expires_at` — each written as its length
    /// (u64, big-endian) then its bytes. The generation makes every
    /// replacement a new reference, even one that changes nothing else.
    #[must_use]
    pub fn authorization_ref(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut encoded = Vec::new();
        append_len_prefixed(&mut encoded, b"ga1");
        append_len_prefixed(&mut encoded, &self.generation.to_be_bytes());
        append_len_prefixed(&mut encoded, self.scope_fingerprint.as_bytes());
        append_len_prefixed(
            &mut encoded,
            resource_set_digest(&self.resources).as_bytes(),
        );
        let artifact = match &self.artifact {
            None => vec![0u8],
            Some(a) => {
                let mut bytes = Vec::with_capacity(1 + a.len());
                bytes.push(1u8);
                bytes.extend_from_slice(a.as_bytes());
                bytes
            }
        };
        append_len_prefixed(&mut encoded, &artifact);
        append_len_prefixed(&mut encoded, self.decided_at.as_bytes());
        append_len_prefixed(&mut encoded, self.expires_at.as_bytes());
        let hex = hex_encode(&Sha256::digest(&encoded));
        format!("ga1:{hex}")
    }
}

fn canonical_resources(resources: &[String]) -> Result<Vec<String>> {
    if resources
        .iter()
        .any(|element| !is_resource_element(element))
    {
        return Err(StoreError::Conflict(
            "a grant's resource is not a resource".to_owned(),
        ));
    }
    // The cap counts what was submitted, not the set it reduces to (§2.1,
    // counterexample 6): 257 elements are refused even if two are equal.
    if resources.len() > MAX_GRANT_RESOURCES {
        return Err(StoreError::Conflict(format!(
            "a grant carries at most {MAX_GRANT_RESOURCES} resources"
        )));
    }
    let mut canonical = resources.to_vec();
    canonical.sort();
    canonical.dedup();
    Ok(canonical)
}

/// A grant, refusal or revocation for `(module, op)` was committed; `op:
/// None` is every tool of the module (K1-6a-G §2.5). It carries no decision:
/// a listener re-reads the store, which stays authoritative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleConsentChange {
    pub module: String,
    pub op: Option<String>,
}

/// A persisted module authorization revocation. `op: None` revokes every
/// tool for the module; `Some(op)` revokes only that tool. This host consent
/// event is intentionally independent of capability claims and epochs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleConsentRevocation {
    pub module: String,
    pub op: Option<String>,
    pub revoked_at: String,
}

/// The outcome of [`Store::lookup_module_consent`]. Outcomes tied to a stored
/// decision or stale/expired record carry that record for audit/debugging;
/// `NotGranted` has none. `Revoked` may have no record when lifecycle
/// invalidation precedes the first consent. [`Self::is_authorized`] is the
/// single authorization predicate for future call-gating code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentLookup {
    /// No row exists for this `(module, op)` at all — the task spec's
    /// "无记录即拒绝".
    NotGranted,
    /// A row exists, matches the current version/fingerprint, has not
    /// expired, and its decision is `Denied`.
    Denied(ModuleConsentRecord),
    /// A previously recorded consent was explicitly revoked (or invalidated
    /// by a module lifecycle change). This differs from `Denied`: denial is
    /// the user's recorded refusal; revocation withdraws an earlier consent.
    Revoked(Option<ModuleConsentRecord>),
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

/// UTC epoch milliseconds of an RFC 3339 instant, by the same parser
/// [`Store::lookup_module_consent`] judges expiry with; `None` if it does
/// not parse.
#[must_use]
pub fn consent_instant_millis(s: &str) -> Option<i64> {
    parse_instant(s).ok().map(|(millis, _)| millis)
}

fn row_to_record(row: &SqliteRow) -> Result<ModuleConsentRecord> {
    let resources = match row.get::<Option<String>, _>("resources") {
        None => Vec::new(),
        Some(text) => serde_json::from_str::<Vec<String>>(&text)
            .map_err(|_| StoreError::Conflict("a stored resource set is malformed".to_owned()))?,
    };
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
        resources,
        artifact: row.get("artifact"),
        generation: row.get("generation"),
    })
}

/// `module`'s current enable epoch; 0 before any (K1-6a-G §2.1 F3).
async fn current_enable_epoch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    module: &str,
) -> Result<i64> {
    let epoch: Option<i64> =
        sqlx::query_scalar("SELECT epoch FROM module_enable_epochs WHERE module = ?")
            .bind(module)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(epoch.unwrap_or(0))
}

/// Set what was last seen of `module`'s enablement; its epoch is left.
async fn record_enablement(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    module: &str,
    enabled: bool,
    entry: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO module_enable_epochs (module, observed_enabled, observed_entry)
         VALUES (?, ?, ?)
         ON CONFLICT (module) DO UPDATE SET
            observed_enabled = excluded.observed_enabled,
            observed_entry = excluded.observed_entry",
    )
    .bind(module)
    .bind(enabled)
    .bind(entry)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Set `module`'s enable epoch to a fresh write generation.
async fn advance_enable_epoch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    module: &str,
) -> Result<i64> {
    let epoch: i64 = sqlx::query_scalar(
        "INSERT INTO module_consent_generations DEFAULT VALUES RETURNING generation",
    )
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO module_enable_epochs (module, epoch) VALUES (?, ?)
         ON CONFLICT (module) DO UPDATE SET epoch = excluded.epoch",
    )
    .bind(module)
    .bind(epoch)
    .execute(&mut **tx)
    .await?;
    Ok(epoch)
}

impl Store {
    /// Returns persisted per-tool authorization decisions for the host's
    /// management surface. The route derives current status with
    /// [`Store::lookup_module_consent`], which applies expiry and revocation.
    pub async fn list_module_consents(&self) -> Result<Vec<ModuleConsentRecord>> {
        let rows = sqlx::query("SELECT * FROM module_consents ORDER BY module, op")
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_record).collect()
    }

    /// Subscribe to every committed grant, refusal and revocation. A
    /// receiver that falls behind gets `Lagged` and must re-read the store
    /// for everything it watches.
    pub fn subscribe_module_consent_changes(
        &self,
    ) -> tokio::sync::broadcast::Receiver<ModuleConsentChange> {
        self.module_consent_changes.subscribe()
    }

    fn announce_module_consent_change(&self, module: &str, op: Option<&str>) {
        let _ = self.module_consent_changes.send(ModuleConsentChange {
            module: module.to_owned(),
            op: op.map(str::to_owned),
        });
    }

    /// Subscribe to post-commit consent revocations. A broadcast channel is
    /// used because multiple independent consumers may need the event; the
    /// durable table remains authoritative if a receiver starts late.
    pub fn subscribe_module_consent_revocations(
        &self,
    ) -> tokio::sync::broadcast::Receiver<ModuleConsentRevocation> {
        self.module_consent_revocations.subscribe()
    }

    /// Persist a revocation for one tool (`op: Some`) or every tool in a
    /// module (`op: None`), then broadcast it. `now` must be RFC 3339; invalid
    /// timestamps fail before any write or notification.
    pub async fn revoke_module_consent(
        &self,
        module: &str,
        op: Option<&str>,
        now: &str,
    ) -> Result<()> {
        self.revoke_module_consent_tracked(module, op, now)
            .await
            .map(|_| ())
    }

    /// [`Self::revoke_module_consent`], returning the revocation's write
    /// generation: the key [`Self::record_revocation_stop`] records the
    /// module's answer under (K1-6a-G §2.5).
    pub async fn revoke_module_consent_tracked(
        &self,
        module: &str,
        op: Option<&str>,
        now: &str,
    ) -> Result<i64> {
        let (_, revoked_at) = parse_instant(now)?;
        let key = op.unwrap_or("*");
        let mut tx = self.pool().begin().await?;
        let generation: i64 = sqlx::query_scalar(
            "INSERT INTO module_consent_generations DEFAULT VALUES RETURNING generation",
        )
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO module_consent_revocations (module, op, revoked_at, generation)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (module, op) DO UPDATE SET
                revoked_at = excluded.revoked_at,
                generation = excluded.generation,
                stop_state = NULL",
        )
        .bind(module)
        .bind(key)
        .bind(&revoked_at)
        .bind(generation)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let _ = self
            .module_consent_revocations
            .send(ModuleConsentRevocation {
                module: module.to_owned(),
                op: op.map(str::to_owned),
                revoked_at,
            });
        self.announce_module_consent_change(module, op);
        Ok(generation)
    }

    /// Records what the module answered to revocation `generation` of
    /// `(module, op)` (K1-6a-G §2.5). A later revocation of the same key
    /// has replaced the row, so nothing is written and `false` returned.
    /// A later revocation of the whole module does not replace a tool's
    /// row: the answer is still recorded here, while the list reads the
    /// newer one ([`Self::revocation_stop_state`]).
    pub async fn record_revocation_stop(
        &self,
        module: &str,
        op: Option<&str>,
        generation: i64,
        stop_state: &str,
    ) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE module_consent_revocations SET stop_state = ?
             WHERE module = ? AND op = ? AND generation = ?",
        )
        .bind(stop_state)
        .bind(module)
        .bind(op.unwrap_or("*"))
        .bind(generation)
        .execute(self.pool())
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// What the module answered to the newest revocation covering
    /// `(module, op)`, the tool's own or the whole module's; `None` when
    /// none covers it or no answer was recorded (read as unconfirmed).
    pub async fn revocation_stop_state(&self, module: &str, op: &str) -> Result<Option<String>> {
        let state: Option<Option<String>> = sqlx::query_scalar(
            "SELECT stop_state FROM module_consent_revocations
             WHERE module = ? AND op IN (?, '*') ORDER BY generation DESC LIMIT 1",
        )
        .bind(module)
        .bind(op)
        .fetch_optional(self.pool())
        .await?;
        Ok(state.flatten())
    }

    /// The grants that expired at or before `now` whose module was not yet
    /// told (K1-6a-G §2.5), and the earliest expiry still ahead. Expiry is
    /// judged as [`Self::lookup_module_consent`] judges it; a grant a
    /// revocation already covers was told by that revocation.
    pub async fn due_expiry_notices(
        &self,
        now: &str,
    ) -> Result<(Vec<ModuleConsentRecord>, Option<String>)> {
        let (now_millis, _) = parse_instant(now)?;
        let rows = sqlx::query(
            "SELECT c.* FROM module_consents c
             WHERE c.decision = ? AND c.expiry_stop_state IS NULL
               AND NOT EXISTS (SELECT 1 FROM module_consent_revocations r
                   WHERE r.module = c.module AND r.op IN (c.op, '*')
                     AND r.generation >= c.generation)",
        )
        .bind(decision_str(ConsentDecision::Granted))
        .fetch_all(self.pool())
        .await?;
        let mut due = Vec::new();
        let mut next: Option<(i64, String)> = None;
        for row in &rows {
            let record = row_to_record(row)?;
            match parse_instant(&record.expires_at) {
                Ok((expiry, _)) if expiry >= now_millis => {
                    if next.as_ref().is_none_or(|(at, _)| expiry < *at) {
                        next = Some((expiry, record.expires_at.clone()));
                    }
                }
                _ => due.push(record),
            }
        }
        Ok((due, next.map(|(_, at)| at)))
    }

    /// Records what the module answered when told grant `generation` of
    /// `(module, op)` expired. A later decision of the same key has replaced
    /// the row, so nothing is written and `false` returned.
    pub async fn record_expiry_stop(
        &self,
        module: &str,
        op: &str,
        generation: i64,
        stop_state: &str,
    ) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE module_consents SET expiry_stop_state = ?
             WHERE module = ? AND op = ? AND generation = ?",
        )
        .bind(stop_state)
        .bind(module)
        .bind(op)
        .bind(generation)
        .execute(self.pool())
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// What the module answered when told `(module, op)`'s grant expired;
    /// `None` when it was not told yet (read as unconfirmed).
    pub async fn expiry_stop_state(&self, module: &str, op: &str) -> Result<Option<String>> {
        let state: Option<Option<String>> = sqlx::query_scalar(
            "SELECT expiry_stop_state FROM module_consents WHERE module = ? AND op = ?",
        )
        .bind(module)
        .bind(op)
        .fetch_optional(self.pool())
        .await?;
        Ok(state.flatten())
    }

    /// Lifecycle invalidation for disable/uninstall/package replacement. The
    /// operation is deliberately best-effort at call sites that already have
    /// an established lifecycle result; failures are returned for logging.
    pub async fn invalidate_module_consents(&self, module: &str) -> Result<()> {
        self.revoke_module_consent(module, None, &Utc::now().to_rfc3339())
            .await
    }

    /// Advance `module`'s enable epoch (K1-6a-G §2.1 F3): every decision
    /// written before is `Stale` from now on. Returns the new epoch, a fresh
    /// write generation, so epochs only grow and are never reused. Callers
    /// advance BEFORE the change it stands for takes effect, so a failed
    /// write leaves the module as it was rather than an old grant live.
    pub async fn advance_module_enable_epoch(&self, module: &str) -> Result<i64> {
        let mut tx = self.pool().begin().await?;
        let epoch = advance_enable_epoch(&mut tx, module).await?;
        tx.commit().await?;
        self.announce_module_consent_change(module, None);
        Ok(epoch)
    }

    /// Record what is seen of `module`'s enablement now: `enabled`, and
    /// `entry`, the digest of its os.json entry and enable mark (K1-6a-G
    /// §2.1 F3). Enabled now, after last seeing it disabled, never, or with
    /// another entry, advances the epoch in the same transaction. Returns
    /// whether it did.
    pub async fn observe_module_enablement(
        &self,
        module: &str,
        enabled: bool,
        entry: &str,
    ) -> Result<bool> {
        // BEGIN IMMEDIATE: what was seen is read under the write lock, so a
        // commit elsewhere in between cannot fail the upgrade to a write.
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let seen: Option<(bool, Option<String>)> = sqlx::query_as(
            "SELECT observed_enabled, observed_entry FROM module_enable_epochs WHERE module = ?",
        )
        .bind(module)
        .fetch_optional(&mut *tx)
        .await?;
        let unchanged = matches!(&seen, Some((true, Some(last))) if last == entry);
        let advance = enabled && !unchanged;
        if advance {
            advance_enable_epoch(&mut tx, module).await?;
        }
        record_enablement(&mut tx, module, enabled, entry).await?;
        tx.commit().await?;
        if advance {
            self.announce_module_consent_change(module, None);
        }
        Ok(advance)
    }

    /// Record what is seen of `module`'s enablement, as
    /// [`Self::observe_module_enablement`] does, but never advance: for a
    /// caller that advanced before the change it now records (an enable
    /// through a route), so a grant made in between still holds.
    pub async fn record_module_enablement(
        &self,
        module: &str,
        enabled: bool,
        entry: &str,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        record_enablement(&mut tx, module, enabled, entry).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The sole write path: a fresh grant/deny for `(module, op)` always
    /// REPLACES whatever row was there (migration doc comment) — there is no
    /// in-place field mutation of an existing consent row anywhere in this
    /// module, so "approved A's scope, silently kept B's" cannot arise
    /// through this crate's own writes.
    async fn upsert_module_consent(
        &self,
        summary: &ToolPermissionSummary,
        scope: &GrantScope,
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
        let canonical = canonical_resources(&scope.resources)?;
        let scope_fingerprint = summary.scope_fingerprint();
        #[allow(
            clippy::expect_used,
            reason = "a Vec<String> always serializes to a JSON array of strings"
        )]
        let resources =
            serde_json::to_string(&canonical).expect("a resource set always serializes");
        let mut tx = self.pool().begin().await?;
        let generation: i64 = sqlx::query_scalar(
            "INSERT INTO module_consent_generations DEFAULT VALUES RETURNING generation",
        )
        .fetch_one(&mut *tx)
        .await?;
        let enable_epoch = current_enable_epoch(&mut tx, &summary.module).await?;
        let row = sqlx::query(
            "INSERT INTO module_consents
                (module, op, module_version, scope_fingerprint, source, risk,
                 readable, writable, external, decision, decided_at, expires_at,
                 resources, artifact, generation, enable_epoch)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                expires_at = excluded.expires_at,
                resources = excluded.resources,
                artifact = excluded.artifact,
                generation = excluded.generation,
                enable_epoch = excluded.enable_epoch,
                expiry_stop_state = NULL
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
        .bind(&resources)
        .bind(&scope.artifact)
        .bind(generation)
        .bind(enable_epoch)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let record = row_to_record(&row)?;
        self.announce_module_consent_change(&summary.module, Some(&summary.op));
        Ok(record)
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
        self.grant_module_consent_scoped(summary, &GrantScope::default(), decided_at, expires_at)
            .await
    }

    /// [`Self::grant_module_consent`] for `scope`'s resources and artifact
    /// (K1-6a-G §2.2). The resources are stored as a set; an element that
    /// is not a resource (contract §2.1), or more than
    /// [`MAX_GRANT_RESOURCES`] submitted (repeats counted), fails before
    /// anything is written or announced.
    pub async fn grant_module_consent_scoped(
        &self,
        summary: &ToolPermissionSummary,
        scope: &GrantScope,
        decided_at: &str,
        expires_at: &str,
    ) -> Result<ModuleConsentRecord> {
        self.upsert_module_consent(
            summary,
            scope,
            ConsentDecision::Granted,
            decided_at,
            expires_at,
        )
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
        self.upsert_module_consent(
            summary,
            &GrantScope::default(),
            ConsentDecision::Denied,
            decided_at,
            expires_at,
        )
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
        // One read transaction, so the grant and the revocations come from
        // the same snapshot: a revoke-then-regrant committed between two
        // separate reads would pair the old grant with the new revocation.
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query("SELECT * FROM module_consents WHERE module = ? AND op = ?")
            .bind(module)
            .bind(op)
            .fetch_optional(&mut *tx)
            .await?;
        let revocations = sqlx::query(
            "SELECT generation FROM module_consent_revocations
             WHERE module = ? AND op IN (?, '*')",
        )
        .bind(module)
        .bind(op)
        .fetch_all(&mut *tx)
        .await?;
        let enable_epoch = current_enable_epoch(&mut tx, module).await?;
        tx.commit().await?;
        let record = row.as_ref().map(row_to_record).transpose()?;
        if let Some(record) = &record {
            // A row whose `decided_at` does not parse was not written by
            // `upsert_module_consent`; it fails closed, as `expires_at` does.
            if parse_instant(&record.decided_at).is_err() {
                return Ok(ConsentLookup::Revoked(Some(record.clone())));
            }
            let revoked = revocations
                .iter()
                .any(|row| row.get::<i64, _>("generation") >= record.generation);
            if revoked {
                return Ok(ConsentLookup::Revoked(Some(record.clone())));
            }
        } else if !revocations.is_empty() {
            // Revoking before a consent exists remains observable and
            // fail-closed; a later consent row supersedes it by generation.
            return Ok(ConsentLookup::Revoked(None));
        }
        let (Some(record), Some(row)) = (record, row) else {
            return Ok(ConsentLookup::NotGranted);
        };
        // K1-6a-G §2.1 F3: a decision holds only in the enable epoch it was
        // written in; the module has been (re-)enabled since.
        if record.module_version != current_module_version
            || record.scope_fingerprint != current_scope_fingerprint
            || row.get::<i64, _>("enable_epoch") != enable_epoch
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

    /// K1-6a-G §2.5: an expired grant is due for a notice until its answer
    /// is recorded, against that grant only; a refusal, or a grant a
    /// revocation covers, never is. The next expiry ahead is given.
    #[tokio::test]
    async fn an_expired_grant_is_due_for_one_notice() {
        let store = Store::open_memory().await.unwrap();
        let millis = |at: &str| consent_instant_millis(at).unwrap();
        let granted = store
            .grant_module_consent(
                &summary("docs"),
                "2026-10-24T00:00:00Z",
                "2026-10-24T01:00:00Z",
            )
            .await
            .unwrap();
        let mut refused = summary("docs");
        refused.op = "other".to_owned();
        store
            .deny_module_consent(&refused, "2026-10-24T00:00:00Z", "2026-10-24T00:30:00Z")
            .await
            .unwrap();
        let (due, next) = store
            .due_expiry_notices("2026-10-24T01:00:00Z")
            .await
            .unwrap();
        assert!(due.is_empty(), "valid until the instant itself");
        assert_eq!(millis(&next.unwrap()), millis("2026-10-24T01:00:00Z"));
        let (due, next) = store
            .due_expiry_notices("2026-10-24T02:00:00Z")
            .await
            .unwrap();
        assert_eq!(next, None);
        assert_eq!(due.len(), 1);
        assert_eq!(
            (due[0].op.as_str(), due[0].generation),
            ("read_doc", granted.generation)
        );
        assert_eq!(
            store
                .expiry_stop_state("documenting", "read_doc")
                .await
                .unwrap(),
            None
        );
        assert!(
            store
                .record_expiry_stop("documenting", "read_doc", granted.generation, "unconfirmed")
                .await
                .unwrap()
        );
        assert!(
            store
                .due_expiry_notices("2026-10-24T02:00:00Z")
                .await
                .unwrap()
                .0
                .is_empty()
        );
        assert_eq!(
            store
                .expiry_stop_state("documenting", "read_doc")
                .await
                .unwrap()
                .as_deref(),
            Some("unconfirmed")
        );
        let regranted = store
            .grant_module_consent(
                &summary("docs"),
                "2026-10-24T02:00:00Z",
                "2026-10-24T03:00:00Z",
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .expiry_stop_state("documenting", "read_doc")
                .await
                .unwrap(),
            None
        );
        assert!(
            !store
                .record_expiry_stop("documenting", "read_doc", granted.generation, "confirmed")
                .await
                .unwrap()
        );
        let (due, _) = store
            .due_expiry_notices("2026-10-24T04:00:00Z")
            .await
            .unwrap();
        assert_eq!(due[0].generation, regranted.generation);
        store
            .revoke_module_consent("documenting", None, "2026-10-24T03:30:00Z")
            .await
            .unwrap();
        assert!(
            store
                .due_expiry_notices("2026-10-24T04:00:00Z")
                .await
                .unwrap()
                .0
                .is_empty()
        );
    }

    /// K1-6a-G §2.5: a stop answer is recorded against the revocation it
    /// answers, never a later one of the same key, which starts with none.
    #[tokio::test]
    async fn a_stop_answer_belongs_to_its_own_revocation() {
        let store = Store::open_memory().await.unwrap();
        let first = store
            .revoke_module_consent_tracked("m", Some("t"), "2026-10-24T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(store.revocation_stop_state("m", "t").await.unwrap(), None);
        assert!(
            store
                .record_revocation_stop("m", Some("t"), first, "confirmed")
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .revocation_stop_state("m", "t")
                .await
                .unwrap()
                .as_deref(),
            Some("confirmed")
        );
        let second = store
            .revoke_module_consent_tracked("m", Some("t"), "2026-10-24T00:00:01Z")
            .await
            .unwrap();
        assert_eq!(store.revocation_stop_state("m", "t").await.unwrap(), None);
        assert!(
            !store
                .record_revocation_stop("m", Some("t"), first, "confirmed")
                .await
                .unwrap()
        );
        let whole = store
            .revoke_module_consent_tracked("m", None, "2026-10-24T00:00:02Z")
            .await
            .unwrap();
        assert!(whole > second);
        assert!(
            store
                .record_revocation_stop("m", None, whole, "not_running")
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .revocation_stop_state("m", "t")
                .await
                .unwrap()
                .as_deref(),
            Some("not_running")
        );
        assert_eq!(
            store.revocation_stop_state("other", "t").await.unwrap(),
            None
        );
    }

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

    // ADR-K1-03 §3 / 6a.3 counterexamples: explicit revocation is a durable
    // state distinct from a user's original refusal, and observers must be
    // notified so a later slice can stop queued and in-flight calls.
    #[tokio::test]
    async fn revoking_a_tool_is_immediate_persistent_and_distinct_from_deny() {
        let store = store().await;
        let s = summary("doc body");
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        store
            .revoke_module_consent(&s.module, Some(&s.op), "2026-10-08T01:00:00Z")
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
        assert!(matches!(lookup, ConsentLookup::Revoked(_)));
        assert!(!lookup.is_authorized());

        store
            .deny_module_consent(&s, "2026-10-08T02:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let denied = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T02:00:01Z",
            )
            .await
            .unwrap();
        assert!(matches!(denied, ConsentLookup::Denied(_)));
        assert!(!denied.is_authorized());

        store
            .grant_module_consent(&s, "2026-10-08T03:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let regranted = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T03:00:01Z",
            )
            .await
            .unwrap();
        assert!(matches!(regranted, ConsentLookup::Granted(_)));
        assert!(regranted.is_authorized());
    }

    #[tokio::test]
    async fn revocation_survives_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("consents.db");
        let s = summary("doc body");
        {
            let store = Store::open(&db).await.unwrap();
            store
                .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
                .await
                .unwrap();
            store
                .revoke_module_consent(&s.module, Some(&s.op), "2026-10-08T01:00:00Z")
                .await
                .unwrap();
        }
        let reopened = Store::open(&db).await.unwrap();
        let lookup = reopened
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T01:00:01Z",
            )
            .await
            .unwrap();
        assert!(matches!(lookup, ConsentLookup::Revoked(_)));
    }

    #[tokio::test]
    async fn lifecycle_revocation_is_visible_before_a_consent_row_exists() {
        let store = store().await;
        let s = summary("doc body");
        store.invalidate_module_consents(&s.module).await.unwrap();
        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2099-01-01T00:00:00Z",
            )
            .await
            .unwrap();
        assert!(matches!(lookup, ConsentLookup::Revoked(None)));
        assert!(!lookup.is_authorized());
    }

    /// K1-6a-G §2.5: a grant and a refusal are announced like a
    /// revocation, so an in-flight call can see its grant replaced.
    #[tokio::test]
    async fn grants_refusals_and_revocations_are_all_announced() {
        let store = store().await;
        let mut changes = store.subscribe_module_consent_changes();
        let s = summary("reads one file");
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        store
            .deny_module_consent(&s, "2026-10-08T00:00:01Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        store
            .revoke_module_consent(&s.module, None, "2026-10-08T00:00:02Z")
            .await
            .unwrap();
        let tool = ModuleConsentChange {
            module: s.module.clone(),
            op: Some(s.op.clone()),
        };
        assert_eq!(changes.try_recv().unwrap(), tool);
        assert_eq!(changes.try_recv().unwrap(), tool);
        assert_eq!(
            changes.try_recv().unwrap(),
            ModuleConsentChange {
                module: s.module.clone(),
                op: None
            }
        );
        assert!(changes.try_recv().is_err(), "one announcement per write");
        // A write that fails announces nothing.
        assert!(
            store
                .grant_module_consent(&s, "not a time", "2026-11-08T00:00:00Z")
                .await
                .is_err()
        );
        assert!(changes.try_recv().is_err());
    }

    /// The announcement follows the commit: a write the database refuses
    /// announces nothing, so no listener re-reads a grant that never changed.
    #[tokio::test]
    async fn a_write_the_database_refuses_announces_nothing() {
        let store = store().await;
        let mut changes = store.subscribe_module_consent_changes();
        let s = summary("reads one file");
        for table in ["module_consents", "module_consent_revocations"] {
            sqlx::query(&format!("DROP TABLE {table}"))
                .execute(store.pool())
                .await
                .unwrap();
        }
        let (at, until) = ("2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z");
        assert!(store.grant_module_consent(&s, at, until).await.is_err());
        assert!(store.deny_module_consent(&s, at, until).await.is_err());
        assert!(
            store
                .revoke_module_consent(&s.module, None, at)
                .await
                .is_err()
        );
        assert!(changes.try_recv().is_err());
    }

    #[tokio::test]
    async fn module_revocation_covers_each_tool_and_broadcasts_scope() {
        let store = store().await;
        let first = summary("doc body");
        let mut second = first.clone();
        second.op = "write_doc".to_owned();
        store
            .grant_module_consent(&first, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        store
            .grant_module_consent(&second, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let mut events = store.subscribe_module_consent_revocations();

        store
            .revoke_module_consent(&first.module, None, "2026-10-08T01:00:00Z")
            .await
            .unwrap();
        let event = events.recv().await.unwrap();
        assert_eq!(event.module, first.module);
        assert_eq!(event.op, None);
        for s in [&first, &second] {
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
            assert!(matches!(lookup, ConsentLookup::Revoked(_)));
        }
    }

    // ── K1-6a-G §2.2: resource sets, artifact identity, write generations ──

    async fn lookup_at(store: &Store, s: &ToolPermissionSummary, now: &str) -> ConsentLookup {
        store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                now,
            )
            .await
            .unwrap()
    }

    fn scope(resources: &[&str], artifact: Option<&str>) -> GrantScope {
        GrantScope {
            resources: resources.iter().map(|r| (*r).to_owned()).collect(),
            artifact: artifact.map(str::to_owned),
        }
    }

    #[test]
    fn a_resource_is_a_kind_and_an_id() {
        let kind_32 = format!("a{}", "b".repeat(31));
        let id_128 = "x".repeat(128);
        for good in [
            "doc:a".to_owned(),
            "doc_2:A-b_9".to_owned(),
            format!("{kind_32}:{id_128}"),
            "doc:a-b".to_owned(),
        ] {
            assert!(is_resource_element(&good), "{good}");
        }
        for bad in [
            String::new(),
            "doc".to_owned(),
            "doc:".to_owned(),
            ":a".to_owned(),
            "*".to_owned(),
            "doc:*".to_owned(),
            "Doc:a".to_owned(),
            "2doc:a".to_owned(),
            "doc-x:a".to_owned(),
            "doc:a b".to_owned(),
            "doc:a:b".to_owned(),
            "doc:é".to_owned(),
            format!("a{}:x", "b".repeat(32)),
            format!("doc:{}", "x".repeat(129)),
        ] {
            assert!(!is_resource_element(&bad), "{bad}");
        }
    }

    /// Golden values computed independently (Python: sha256 over
    /// `struct.pack(">Q", len) + bytes` of each sorted distinct element).
    #[test]
    fn the_resource_digest_is_of_the_set() {
        let set = |xs: &[&str]| xs.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            resource_set_digest(&[]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            resource_set_digest(&set(&["doc:b", "doc:a", "doc:b"])),
            "d871e5572f0cf6cfbcc2b5cf608ff1a53d1ab7e3f7813888ecad989874894f65"
        );
        assert_eq!(
            resource_set_digest(&set(&["doc:a", "doc:b"])),
            resource_set_digest(&set(&["doc:b", "doc:a"]))
        );
        assert_eq!(
            resource_set_digest(&set(&["ab", "c"])),
            "601d5476e2ccfe2c87a2bba7a322659734a05749d5b5aa781f513e4912db0d5f"
        );
        assert_eq!(
            resource_set_digest(&set(&["a", "bc"])),
            "3fafa1cf2f19a7c1129beb20cf0983f73a489a221fc0dd2f16d1be292d089205"
        );
    }

    #[tokio::test]
    async fn a_grant_keeps_its_resources_as_a_set_and_its_artifact() {
        let store = store().await;
        let s = summary("doc body");
        let written = store
            .grant_module_consent_scoped(
                &s,
                &scope(&["doc:b", "doc:a", "doc:b"], Some("sha256:abc")),
                "2026-10-08T00:00:00Z",
                "2026-11-08T00:00:00Z",
            )
            .await
            .unwrap();
        assert_eq!(written.resources, ["doc:a", "doc:b"]);
        assert_eq!(written.artifact.as_deref(), Some("sha256:abc"));
        let ConsentLookup::Granted(read) = lookup_at(&store, &s, "2026-10-08T00:00:01Z").await
        else {
            panic!("the grant is current");
        };
        assert_eq!(read, written);
        assert_eq!(store.list_module_consents().await.unwrap(), [written]);
    }

    #[tokio::test]
    async fn a_plain_grant_or_refusal_has_no_resources_and_no_artifact() {
        let store = store().await;
        let s = summary("doc body");
        let granted = store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        assert!(granted.resources.is_empty());
        assert_eq!(granted.artifact, None);
        let denied = store
            .deny_module_consent(&s, "2026-10-08T00:00:01Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        assert!(denied.resources.is_empty());
        assert_eq!(denied.artifact, None);
    }

    /// A set that is not one the contract allows is refused whole: nothing
    /// is written, nothing announced, and an earlier grant stays as it was.
    #[tokio::test]
    async fn a_bad_resource_set_is_refused_before_anything_is_written() {
        let store = store().await;
        let s = summary("doc body");
        let kept = store
            .grant_module_consent_scoped(
                &s,
                &scope(&["doc:kept"], None),
                "2026-10-08T00:00:00Z",
                "2026-11-08T00:00:00Z",
            )
            .await
            .unwrap();
        let mut changes = store.subscribe_module_consent_changes();
        let too_many: Vec<String> = (0..=MAX_GRANT_RESOURCES)
            .map(|i| format!("doc:{i}"))
            .collect();
        for resources in [
            vec!["doc:a".to_owned(), "*".to_owned()],
            vec!["doc:".to_owned()],
            too_many,
        ] {
            let refused = store
                .grant_module_consent_scoped(
                    &s,
                    &GrantScope {
                        resources,
                        artifact: None,
                    },
                    "2026-10-08T00:00:01Z",
                    "2026-11-08T00:00:00Z",
                )
                .await;
            assert!(refused.is_err());
        }
        assert!(changes.try_recv().is_err(), "nothing was announced");
        let ConsentLookup::Granted(read) = lookup_at(&store, &s, "2026-10-08T00:00:02Z").await
        else {
            panic!("the earlier grant stands");
        };
        assert_eq!(read, kept);
    }

    /// The cap counts submitted resources: 256 distinct ones pass, but 257
    /// entries are refused even when one repeats.
    #[tokio::test]
    async fn the_resource_cap_counts_submitted_resources() {
        let store = store().await;
        let s = summary("doc body");
        let scope = |resources: Vec<String>| GrantScope {
            resources,
            artifact: None,
        };
        let full: Vec<String> = (0..MAX_GRANT_RESOURCES)
            .map(|i| format!("doc:{i}"))
            .collect();
        let written = store
            .grant_module_consent_scoped(
                &s,
                &scope(full.clone()),
                "2026-10-08T00:00:00Z",
                "2026-11-08T00:00:00Z",
            )
            .await
            .unwrap();
        assert_eq!(written.resources.len(), MAX_GRANT_RESOURCES);

        // Counterexample 6: 257 submitted elements are refused even when one
        // repeats, and the earlier grant is left as it was.
        let mut over = full;
        over.push("doc:0".to_owned());
        let refused = store
            .grant_module_consent_scoped(
                &s,
                &scope(over),
                "2026-10-09T00:00:00Z",
                "2026-11-09T00:00:00Z",
            )
            .await;
        assert!(
            matches!(refused, Err(StoreError::Conflict(_))),
            "{refused:?}"
        );
        let lookup = store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-10T00:00:00Z",
            )
            .await
            .unwrap();
        let ConsentLookup::Granted(record) = lookup else {
            panic!("the earlier grant must stand: {lookup:?}");
        };
        assert_eq!(record.generation, written.generation);
    }

    /// A row whose `decided_at` does not parse was not written by this
    /// module; with no revocation at all it still fails closed.
    #[tokio::test]
    async fn a_garbage_decided_at_never_makes_a_lookup_granted() {
        let store = store().await;
        let s = summary("doc body");
        let fp = s.scope_fingerprint();
        sqlx::query(
            "INSERT INTO module_consents
                (module, op, module_version, scope_fingerprint, source, risk,
                 readable, writable, external, decision, decided_at, expires_at)
             VALUES (?, ?, ?, ?, 'first_party', 'low', ?, NULL, NULL, 'granted',
                     'garbage', '2026-11-08T00:00:00.000Z')",
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
            matches!(lookup, ConsentLookup::Revoked(Some(_))),
            "{lookup:?}"
        );
        assert!(!lookup.is_authorized());
    }

    /// The grant row and the revocations are read from one snapshot. The
    /// reader's only connection runs `after_release` the first time it goes
    /// back to the pool; there another connection revokes and grants anew.
    /// Read in two statements, the lookup would pair the old grant with the
    /// new revocation and answer `Revoked`; in one transaction the
    /// connection is held until both reads are done, and the answer is the
    /// grant as it stood.
    #[tokio::test]
    async fn a_lookup_reads_the_grant_and_its_revocations_together() {
        use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join("consents.db"))
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let writer_pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .unwrap();
        sqlx::migrate!("./migrations")
            .run(&writer_pool)
            .await
            .unwrap();
        let writer = crate::test_hooks::from_pool(writer_pool);
        let s = summary("doc body");
        let first = store_grant(&writer, &s).await;

        let fired = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let done_tx = Arc::new(std::sync::Mutex::new(Some(done_tx)));
        let reader_pool = {
            let (writer, s, fired, done_tx) = (
                writer.clone(),
                s.clone(),
                Arc::clone(&fired),
                Arc::clone(&done_tx),
            );
            SqlitePoolOptions::new()
                .max_connections(1)
                .min_connections(0)
                .idle_timeout(None)
                .max_lifetime(None)
                .after_release(move |_, _| {
                    let (writer, s, fired, done_tx) = (
                        writer.clone(),
                        s.clone(),
                        Arc::clone(&fired),
                        Arc::clone(&done_tx),
                    );
                    Box::pin(async move {
                        if !fired.swap(true, Ordering::SeqCst) {
                            writer
                                .revoke_module_consent(
                                    &s.module,
                                    Some(&s.op),
                                    "2026-10-08T01:00:00Z",
                                )
                                .await
                                .unwrap();
                            store_grant(&writer, &s).await;
                            writer.advance_module_enable_epoch(&s.module).await.unwrap();
                            if let Some(tx) = done_tx.lock().unwrap().take() {
                                let _ = tx.send(());
                            }
                        }
                        Ok(true)
                    })
                })
                .connect_lazy_with(options)
        };
        let reader = crate::test_hooks::from_pool(reader_pool);

        let lookup = reader
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-08T02:00:00Z",
            )
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), done_rx)
            .await
            .expect("the writes between the reads ran")
            .unwrap();
        let ConsentLookup::Granted(record) = lookup else {
            panic!("one snapshot must give the grant as it stood: {lookup:?}");
        };
        assert_eq!(record.generation, first.generation);
        // The epoch advanced between the reads is not paired with the
        // grant from before it (K1-6a-G §2.1 F3), and is there after.
        assert!(matches!(
            writer
                .lookup_module_consent(
                    &s.module,
                    &s.op,
                    &s.module_version,
                    &s.scope_fingerprint(),
                    "2026-10-08T02:00:00Z",
                )
                .await
                .unwrap(),
            ConsentLookup::Stale(_)
        ));
    }

    async fn store_grant(store: &Store, s: &ToolPermissionSummary) -> ModuleConsentRecord {
        store
            .grant_module_consent(s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap()
    }

    /// A stored set is read back as written (a dispatch validates it), but
    /// one that is not a list of strings fails the read rather than being
    /// taken as some other set.
    #[tokio::test]
    async fn a_stored_resource_set_is_read_as_written_or_not_at_all() {
        let store = store().await;
        let s = summary("doc body");
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let set_resources = |value: &'static str| {
            let store = store.clone();
            async move {
                sqlx::query("UPDATE module_consents SET resources = ?")
                    .bind(value)
                    .execute(store.pool())
                    .await
                    .unwrap();
            }
        };
        set_resources(r#"["*","doc:a"]"#).await;
        let ConsentLookup::Granted(read) = lookup_at(&store, &s, "2026-10-08T00:00:01Z").await
        else {
            panic!("the grant is current");
        };
        assert_eq!(read.resources, ["*", "doc:a"]);
        for malformed in [r#"{"doc":"a"}"#, r#"["doc:a",3]"#, "doc:a", ""] {
            set_resources(malformed).await;
            let read = store
                .lookup_module_consent(
                    &s.module,
                    &s.op,
                    &s.module_version,
                    &s.scope_fingerprint(),
                    "2026-10-08T00:00:01Z",
                )
                .await;
            assert!(read.is_err(), "{malformed}");
            assert!(store.list_module_consents().await.is_err(), "{malformed}");
        }
    }

    /// Every write takes a new generation, even one identical to the last
    /// in scope and time (the same-millisecond refusal-then-grant of §4 3a).
    #[tokio::test]
    async fn every_write_takes_a_new_generation() {
        let store = store().await;
        let s = summary("doc body");
        let at = "2026-10-08T00:00:00Z";
        let first = store
            .grant_module_consent(&s, at, "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let denied = store
            .deny_module_consent(&s, at, "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        store
            .revoke_module_consent(&s.module, None, at)
            .await
            .unwrap();
        let again = store
            .grant_module_consent(&s, at, "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        assert!(0 < first.generation, "{}", first.generation);
        assert!(first.generation < denied.generation);
        // The revocation took one in between.
        assert!(denied.generation + 1 < again.generation);
        assert_eq!(
            (&first.decided_at, &first.scope_fingerprint),
            (&again.decided_at, &again.scope_fingerprint)
        );
    }

    /// PR-Daemon B1: granting again replaces the resources and the artifact;
    /// a narrower grant does not keep the wider set, and a refusal keeps
    /// neither.
    #[tokio::test]
    async fn a_new_decision_replaces_the_resources_and_the_artifact() {
        let store = store().await;
        let s = summary("doc body");
        let grant = |resources: &[&str], artifact: &str| GrantScope {
            resources: resources.iter().map(|r| (*r).to_owned()).collect(),
            artifact: Some(artifact.to_owned()),
        };
        let read_back = || async {
            match store
                .lookup_module_consent(
                    &s.module,
                    &s.op,
                    &s.module_version,
                    &s.scope_fingerprint(),
                    "2026-10-10T00:00:00Z",
                )
                .await
                .unwrap()
            {
                ConsentLookup::Granted(record) | ConsentLookup::Denied(record) => record,
                other => panic!("{other:?}"),
            }
        };
        store
            .grant_module_consent_scoped(
                &s,
                &grant(&["doc:a", "doc:b"], "sha256:old"),
                "2026-10-08T00:00:00Z",
                "2026-11-08T00:00:00Z",
            )
            .await
            .unwrap();
        let narrowed = store
            .grant_module_consent_scoped(
                &s,
                &grant(&["doc:c"], "sha256:new"),
                "2026-10-09T00:00:00Z",
                "2026-11-09T00:00:00Z",
            )
            .await
            .unwrap();
        for record in [narrowed, read_back().await] {
            assert_eq!(record.resources, ["doc:c"]);
            assert_eq!(record.artifact.as_deref(), Some("sha256:new"));
        }

        let refused = store
            .deny_module_consent(&s, "2026-10-09T01:00:00Z", "2026-11-09T01:00:00Z")
            .await
            .unwrap();
        for record in [refused, read_back().await] {
            assert!(record.resources.is_empty(), "{record:?}");
            assert_eq!(record.artifact, None);
        }
    }

    fn ref_record() -> ModuleConsentRecord {
        ModuleConsentRecord {
            module: "documenting".to_owned(),
            op: "read_doc".to_owned(),
            module_version: "1.0.0".to_owned(),
            scope_fingerprint: "fp".to_owned(),
            source: ConsentSource::FirstParty,
            risk: HostRiskLevel::Low,
            readable: None,
            writable: None,
            external: None,
            decision: ConsentDecision::Granted,
            decided_at: "2026-10-08T00:00:00.000Z".to_owned(),
            expires_at: "2026-11-08T00:00:00.000Z".to_owned(),
            resources: vec!["doc:b".to_owned(), "doc:a".to_owned()],
            artifact: Some("sha256:abc".to_owned()),
            generation: 7,
        }
    }

    /// K1-6a-G §2.2: the reference is `ga1:` and a digest, 68 bytes, an
    /// audit ref; the golden values were computed independently of this
    /// code.
    #[test]
    fn the_authorization_ref_is_ga1_and_the_digest_of_the_decision() {
        let record = ref_record();
        let reference = record.authorization_ref();
        assert_eq!(
            reference,
            "ga1:a2b5a7a344a01c6064abc85d6431fdd54e5992d235a81cb086fdec0904bc53c2"
        );
        assert_eq!(reference.len(), 68);
        assert!(crate::AuthorizationRef::new(reference).is_ok());

        let bare = ModuleConsentRecord {
            resources: Vec::new(),
            artifact: None,
            ..ref_record()
        };
        assert_eq!(
            bare.authorization_ref(),
            "ga1:194ba100fb90e92a4885d2a0f68497ebf4736fb2867f3374efc9a38e1d6d1633"
        );
        // No artifact is not the empty artifact.
        let empty_artifact = ModuleConsentRecord {
            artifact: Some(String::new()),
            ..bare
        };
        assert_eq!(
            empty_artifact.authorization_ref(),
            "ga1:1c34c3b6fa3b5e5470449828e57c95c1f6ddbd8d43479cd8c0d149ffbe9a8326"
        );
    }

    /// Every field the reference covers changes it; the resources count as
    /// a set; what it does not cover (the summary text) does not.
    #[test]
    fn every_part_of_the_decision_changes_the_ref() {
        let base = ref_record();
        let changed = [
            ModuleConsentRecord {
                generation: 8,
                ..ref_record()
            },
            ModuleConsentRecord {
                scope_fingerprint: "fq".to_owned(),
                ..ref_record()
            },
            ModuleConsentRecord {
                resources: vec!["doc:a".to_owned()],
                ..ref_record()
            },
            ModuleConsentRecord {
                artifact: None,
                ..ref_record()
            },
            ModuleConsentRecord {
                artifact: Some("sha256:abd".to_owned()),
                ..ref_record()
            },
            ModuleConsentRecord {
                decided_at: "2026-10-08T00:00:00.001Z".to_owned(),
                ..ref_record()
            },
            ModuleConsentRecord {
                expires_at: "2026-11-08T00:00:00.001Z".to_owned(),
                ..ref_record()
            },
        ];
        for record in &changed {
            assert_ne!(
                record.authorization_ref(),
                base.authorization_ref(),
                "{record:?}"
            );
        }
        let same = ModuleConsentRecord {
            resources: vec!["doc:a".to_owned(), "doc:b".to_owned(), "doc:a".to_owned()],
            readable: Some("other text".to_owned()),
            ..ref_record()
        };
        assert_eq!(same.authorization_ref(), base.authorization_ref());
    }

    /// K1-6a-G §4 case 3a: refusing and granting the same scope again within
    /// one millisecond still gives a new reference.
    #[tokio::test]
    async fn a_replaced_grant_has_a_new_ref_within_the_same_instant() {
        let store = store().await;
        let s = summary("doc body");
        let at = "2026-10-08T00:00:00Z";
        let until = "2026-11-08T00:00:00Z";
        let first = store.grant_module_consent(&s, at, until).await.unwrap();
        store.deny_module_consent(&s, at, until).await.unwrap();
        let again = store.grant_module_consent(&s, at, until).await.unwrap();
        assert_eq!(first.decided_at, again.decided_at);
        assert_ne!(first.authorization_ref(), again.authorization_ref());
    }

    /// The audit's resource reference is a count and the set's digest.
    #[test]
    fn the_resource_ref_is_a_count_and_the_set_digest() {
        assert_eq!(
            resource_set_ref(&[]),
            "rs:0:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let set = ["doc:b", "doc:a", "doc:b"].map(str::to_owned);
        let reference = resource_set_ref(&set);
        assert_eq!(
            reference,
            "rs:2:d871e5572f0cf6cfbcc2b5cf608ff1a53d1ab7e3f7813888ecad989874894f65"
        );
        assert!(!reference.contains("doc"));
        assert!(crate::ResourceRef::new(reference).is_ok());
    }

    async fn lookup_now(store: &Store, s: &ToolPermissionSummary) -> ConsentLookup {
        store
            .lookup_module_consent(
                &s.module,
                &s.op,
                &s.module_version,
                &s.scope_fingerprint(),
                "2026-10-09T00:00:00Z",
            )
            .await
            .unwrap()
    }

    async fn grant_now(store: &Store, s: &ToolPermissionSummary) {
        store
            .grant_module_consent(s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
    }

    /// K1-6a-G §2.1 F3: a grant holds only in the enable epoch it was
    /// written in; one written after the module is re-enabled holds.
    #[tokio::test]
    async fn a_grant_from_before_a_re_enable_is_stale() {
        let store = store().await;
        let s = summary("doc body");
        grant_now(&store, &s).await;
        assert!(lookup_now(&store, &s).await.is_authorized());
        let first = store.advance_module_enable_epoch(&s.module).await.unwrap();
        assert!(matches!(
            lookup_now(&store, &s).await,
            ConsentLookup::Stale(_)
        ));
        grant_now(&store, &s).await;
        assert!(lookup_now(&store, &s).await.is_authorized());
        // A refusal is stale the same way, not still a refusal.
        store
            .deny_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let second = store.advance_module_enable_epoch(&s.module).await.unwrap();
        assert!(second > first);
        assert!(matches!(
            lookup_now(&store, &s).await,
            ConsentLookup::Stale(_)
        ));
        // Another module's epoch is its own.
        let other = ToolPermissionSummary {
            module: "other".to_owned(),
            ..summary("doc body")
        };
        grant_now(&store, &other).await;
        store.advance_module_enable_epoch(&s.module).await.unwrap();
        assert!(lookup_now(&store, &other).await.is_authorized());
    }

    /// What a start sees advances the epoch only when the module is enabled
    /// now and was last seen disabled, never, or with another entry.
    #[tokio::test]
    async fn an_observation_advances_the_epoch_only_on_a_change_to_enabled() {
        let store = store().await;
        let s = summary("doc body");
        let observe = |enabled: bool, entry: &'static str| {
            let store = store.clone();
            async move {
                store
                    .observe_module_enablement("documenting", enabled, entry)
                    .await
                    .unwrap()
            }
        };
        assert!(observe(true, "e1").await, "never seen");
        grant_now(&store, &s).await;
        assert!(!observe(true, "e1").await, "the same");
        assert!(lookup_now(&store, &s).await.is_authorized());
        assert!(observe(true, "e2").await, "another entry");
        assert!(matches!(
            lookup_now(&store, &s).await,
            ConsentLookup::Stale(_)
        ));
        grant_now(&store, &s).await;
        assert!(!observe(false, "e2").await, "disabled does not advance");
        assert!(lookup_now(&store, &s).await.is_authorized());
        assert!(
            observe(true, "e2").await,
            "enabled after disabled, same entry"
        );
        assert!(matches!(
            lookup_now(&store, &s).await,
            ConsentLookup::Stale(_)
        ));
        assert!(!observe(false, "e3").await);
        assert_eq!(last_observed(&store).await, (false, Some("e3".into())));
        assert!(!observe(false, "e3").await);
    }

    /// What was last seen of `documenting`: `(observed_enabled,
    /// observed_entry)`.
    async fn last_observed(store: &Store) -> (bool, Option<String>) {
        sqlx::query_as(
            "SELECT observed_enabled, observed_entry FROM module_enable_epochs
             WHERE module = 'documenting'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap()
    }

    /// An observation's advance and record land together or not at all.
    #[tokio::test]
    async fn an_observation_that_cannot_be_recorded_does_not_advance() {
        let store = store().await;
        let s = summary("doc body");
        assert!(
            store
                .observe_module_enablement(&s.module, false, "e1")
                .await
                .is_ok_and(|advanced| !advanced)
        );
        grant_now(&store, &s).await;
        sqlx::query(
            "CREATE TRIGGER no_record BEFORE UPDATE OF observed_entry
             ON module_enable_epochs WHEN NEW.observed_entry = 'e2'
             BEGIN SELECT RAISE(ABORT, 'no record'); END",
        )
        .execute(store.pool())
        .await
        .unwrap();
        assert!(
            store
                .observe_module_enablement(&s.module, true, "e2")
                .await
                .is_err()
        );
        assert!(lookup_now(&store, &s).await.is_authorized());
        assert_eq!(last_observed(&store).await, (false, Some("e1".into())));
    }

    /// Observations racing on one module, the daemon's other writes beside
    /// them, neither fail nor advance more than once (review: a deferred
    /// transaction read first and failed its upgrade to a write).
    #[tokio::test]
    async fn racing_observations_advance_once_and_none_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("race.db")).await.unwrap();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (store, barrier) = (store.clone(), barrier.clone());
                tokio::spawn(async move {
                    barrier.wait().await;
                    if i % 2 == 0 {
                        store
                            .observe_module_enablement("documenting", true, "e1")
                            .await
                            .map(Some)
                    } else {
                        store
                            .advance_module_enable_epoch(&format!("other{i}"))
                            .await
                            .map(|_| None)
                    }
                })
            })
            .collect();
        let mut advanced = 0;
        for handle in handles {
            if handle.await.unwrap().unwrap() == Some(true) {
                advanced += 1;
            }
        }
        assert_eq!(advanced, 1);
    }

    /// An advance tells in-flight calls to look again (K1-6a-G §2.5); an
    /// observation that changes nothing says nothing.
    #[tokio::test]
    async fn an_advance_is_announced_to_the_module() {
        let store = store().await;
        let mut changes = store.subscribe_module_consent_changes();
        store
            .advance_module_enable_epoch("documenting")
            .await
            .unwrap();
        let change = changes.try_recv().unwrap();
        assert_eq!((change.module.as_str(), change.op), ("documenting", None));
        assert!(
            store
                .observe_module_enablement("documenting", false, "e1")
                .await
                .unwrap()
                .eq(&false)
        );
        assert!(changes.try_recv().is_err());
        assert!(
            store
                .observe_module_enablement("documenting", true, "e1")
                .await
                .unwrap()
        );
        assert_eq!(changes.try_recv().unwrap().module, "documenting");
    }

    /// Rows from before migration 0020 keep their meaning across it: a
    /// grant holds and a revoked one stays revoked until the first
    /// observation reaches the module, which makes the grant stale and
    /// leaves the revocation in force (review C2).
    #[tokio::test]
    async fn rows_from_before_migration_0020_keep_their_meaning() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let mut before = sqlx::migrate!("./migrations");
        before.migrations = std::borrow::Cow::Owned(
            before
                .migrations
                .iter()
                .filter(|m| m.version < 20)
                .cloned()
                .collect(),
        );
        before.run(&pool).await.unwrap();
        let read = summary("doc body");
        let write = ToolPermissionSummary::new(
            "documenting",
            "write_doc",
            "1.0.0",
            ConsentSource::FirstParty,
            Some("doc body".to_owned()),
            None,
            None,
            HostRiskLevel::Low,
        );
        for (generation, s) in [(1_i64, &read), (2, &write)] {
            sqlx::query("INSERT INTO module_consent_generations DEFAULT VALUES")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO module_consents
                    (module, op, module_version, scope_fingerprint, source, risk,
                     readable, decision, decided_at, expires_at, generation)
                 VALUES (?, ?, '1.0.0', ?, 'first_party', 'low', 'doc body', 'granted',
                         '2026-10-08T00:00:00.000Z', '2026-11-08T00:00:00.000Z', ?)",
            )
            .bind(&s.module)
            .bind(&s.op)
            .bind(s.scope_fingerprint())
            .bind(generation)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO module_consent_generations DEFAULT VALUES")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO module_consent_revocations (module, op, revoked_at, generation)
             VALUES ('documenting', 'write_doc', '2026-10-08T01:00:00.000Z', 3)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let store = crate::test_hooks::from_pool(pool);
        assert!(lookup_now(&store, &read).await.is_authorized());
        assert!(matches!(
            lookup_now(&store, &write).await,
            ConsentLookup::Revoked(_)
        ));
        assert!(
            store
                .observe_module_enablement("documenting", true, "e1")
                .await
                .unwrap()
        );
        assert!(matches!(
            lookup_now(&store, &read).await,
            ConsentLookup::Stale(_)
        ));
        assert!(matches!(
            lookup_now(&store, &write).await,
            ConsentLookup::Revoked(_)
        ));
    }

    /// An advance leaves what was last observed: a grant made after it
    /// holds across a start that sees the same entry (review G1).
    #[tokio::test]
    async fn an_advance_leaves_the_last_observation() {
        let store = store().await;
        let s = summary("doc body");
        assert!(
            store
                .observe_module_enablement(&s.module, true, "e1")
                .await
                .unwrap()
        );
        store.advance_module_enable_epoch(&s.module).await.unwrap();
        grant_now(&store, &s).await;
        assert!(
            !store
                .observe_module_enablement(&s.module, true, "e1")
                .await
                .unwrap()
        );
        assert!(lookup_now(&store, &s).await.is_authorized());
    }

    /// An enable advances before it is written and records its new entry
    /// after, without advancing again: a grant made in between holds, and
    /// so across a start that sees that entry (review: observing the new
    /// entry advanced a second time).
    #[tokio::test]
    async fn an_enable_recorded_after_its_advance_is_not_advanced_again() {
        let store = store().await;
        let s = summary("doc body");
        assert!(
            store
                .observe_module_enablement(&s.module, true, "e1")
                .await
                .unwrap()
        );
        store.advance_module_enable_epoch(&s.module).await.unwrap();
        grant_now(&store, &s).await;
        store
            .record_module_enablement(&s.module, true, "e2")
            .await
            .unwrap();
        assert!(lookup_now(&store, &s).await.is_authorized());
        assert!(
            !store
                .observe_module_enablement(&s.module, true, "e2")
                .await
                .unwrap()
        );
        assert!(lookup_now(&store, &s).await.is_authorized());
    }

    /// A grant from before any observation (epoch 0, as every row from
    /// before migration 0020) holds until the first one reaches its module.
    #[tokio::test]
    async fn a_grant_from_before_any_observation_holds_until_one() {
        let store = store().await;
        let s = summary("doc body");
        grant_now(&store, &s).await;
        assert!(lookup_now(&store, &s).await.is_authorized());
        store
            .observe_module_enablement(&s.module, true, "e1")
            .await
            .unwrap();
        assert!(matches!(
            lookup_now(&store, &s).await,
            ConsentLookup::Stale(_)
        ));
    }

    /// PR-Daemon B2: revoking the same tool again takes a new generation, so
    /// it covers the grant made since the first revocation.
    #[tokio::test]
    async fn a_second_revocation_of_a_tool_covers_the_grant_since_the_first() {
        let store = store().await;
        let s = summary("doc body");
        let lookup = || async {
            store
                .lookup_module_consent(
                    &s.module,
                    &s.op,
                    &s.module_version,
                    &s.scope_fingerprint(),
                    "2026-10-10T00:00:00Z",
                )
                .await
                .unwrap()
        };
        store_grant(&store, &s).await;
        store
            .revoke_module_consent(&s.module, Some(&s.op), "2026-10-08T01:00:00Z")
            .await
            .unwrap();
        store_grant(&store, &s).await;
        assert!(matches!(lookup().await, ConsentLookup::Granted(_)));
        store
            .revoke_module_consent(&s.module, Some(&s.op), "2026-10-08T02:00:00Z")
            .await
            .unwrap();
        let after = lookup().await;
        assert!(
            matches!(after, ConsentLookup::Revoked(Some(_))),
            "{after:?}"
        );
    }

    /// K1-6a-G §4 case 16: a revocation covers the grants written before it
    /// and none written after, whatever the clock said at each write.
    #[tokio::test]
    async fn a_revocation_covers_earlier_grants_whatever_the_clock_says() {
        let store = store().await;
        let s = summary("doc body");
        store
            .grant_module_consent(&s, "2026-10-08T10:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        // The clock went back an hour before the revocation.
        store
            .revoke_module_consent(&s.module, Some(&s.op), "2026-10-08T09:00:00Z")
            .await
            .unwrap();
        assert!(matches!(
            lookup_at(&store, &s, "2026-10-08T10:00:01Z").await,
            ConsentLookup::Revoked(Some(_))
        ));
        // And further back before a new grant, which still stands.
        store
            .grant_module_consent(&s, "2026-10-08T08:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        assert!(matches!(
            lookup_at(&store, &s, "2026-10-08T10:00:01Z").await,
            ConsentLookup::Granted(_)
        ));
        // A whole-module revocation, with the clock still behind, covers it.
        store
            .revoke_module_consent(&s.module, None, "2026-10-08T07:00:00Z")
            .await
            .unwrap();
        assert!(matches!(
            lookup_at(&store, &s, "2026-10-08T10:00:01Z").await,
            ConsentLookup::Revoked(Some(_))
        ));
    }

    /// Rows from before generations existed carry 0 on both sides; the old
    /// revocation still covers the old grant.
    #[tokio::test]
    async fn a_revocation_from_before_generations_covers_a_grant_from_then() {
        let store = store().await;
        let s = summary("doc body");
        store
            .grant_module_consent(&s, "2026-10-08T10:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        sqlx::query("UPDATE module_consents SET generation = 0")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO module_consent_revocations (module, op, revoked_at)
             VALUES (?, '*', '2026-10-08T09:00:00.000Z')",
        )
        .bind(&s.module)
        .execute(store.pool())
        .await
        .unwrap();
        assert!(matches!(
            lookup_at(&store, &s, "2026-10-08T10:00:01Z").await,
            ConsentLookup::Revoked(Some(_))
        ));
    }

    /// A generation is never handed out twice, across reopening the store.
    #[tokio::test]
    async fn generations_keep_increasing_across_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("consents.db");
        let s = summary("doc body");
        let before = {
            let store = Store::open(&db).await.unwrap();
            store
                .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
                .await
                .unwrap()
                .generation
        };
        let store = Store::open(&db).await.unwrap();
        let after = store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap()
            .generation;
        assert!(before < after, "{before} then {after}");
    }
}
