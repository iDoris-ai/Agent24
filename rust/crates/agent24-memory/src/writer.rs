//! MD-4: the governance write-gate (SPEC-MD-ME §2/§3 MD-4; Dense-Mem's "LLM
//! output is a PROPOSAL, not a write").
//!
//! Nothing an LLM (or a tool, or a web page) produces becomes a durable belief by
//! walking straight into the [`crate::assertion`] ledger. It arrives as a
//! [`Candidate`], and a DETERMINISTIC policy ([`WriteGate::policy`]) decides per
//! candidate:
//! - **Commit** — persist as a qualified belief (enters recall). Only the trusted
//!   paths — `UserSaid` WITH an explicit remember, or `System` — AND only with
//!   non-empty `evidence`: no qualified belief without provenance (review #121 B2).
//! - **Hold** — persist as an UNqualified candidate (stored, reviewable, but kept
//!   out of default recall by the MD-3 `qualified` gate). Mid-trust (`UserSaid`
//!   without remember, `Model`, `ToolOutput`), and also a would-be Commit that
//!   lacks evidence (downgraded, not thrown away).
//! - **Reject** — do NOT persist at all. The least-trusted, most poison-prone
//!   sources: `WebFetch`, `Unknown`.
//!
//! **Trust is an INPUT INVARIANT, not something the gate verifies.** `origin`
//! (hence `trust`) and `explicit_remember` are the caller's assertion of
//! provenance; the [`Candidate`] fields are private so they can only be set
//! through the constructor, and the CALLER — a trusted extractor, NOT the LLM
//! whose output it is labeling — is responsible for setting them truthfully. The
//! gate additionally requires evidence for a Commit so a trusted label alone
//! cannot mint an unsubstantiated qualified belief. (Reverse-verifying trust
//! against the evidence events needs an owner-scoped get-by-id on the event log,
//! which does not exist yet; that is a later slice, noted here rather than faked.)
//!
//! Every persisted decision is AUDITED **in the SAME transaction** as the write
//! (review #121 B1: otherwise a belief can land with no governance record). The
//! audit event id is CONTENT-ADDRESSED, so two different-content decisions on the
//! same candidate id are recorded as distinct events, not aliased (review #121
//! M1). The trail is replayable ([`crate::replay`]). [`WriteGate::dry_run`]
//! reports the decisions with NO side effects.
//!
//! NOT here: turn→candidate EXTRACTION (an LLM step; the gate is deterministic and
//! sits after it) and BULK ROLLBACK (a follow-up). Documented boundaries, not
//! silent omissions.

use agent24_core::util::now_iso8601;
use async_trait::async_trait;
use serde_json::Value;

use crate::artifact::checksum;
use crate::assertion::{Assertion, AssertionId, AssertionLedger, Modality};
use crate::event::{EventLog, MemEvent, Origin, Scope, Trust};
use crate::{MemoryError, Result};

/// A proposed assertion, before the gate decides. Its trust-bearing fields are
/// PRIVATE: construct via [`Candidate::new`] (which requires an [`Origin`]) and
/// the builders, so a caller cannot casually stamp a trust label onto a struct it
/// half-filled. The provenance the constructor records is the caller's
/// responsibility to have earned (see the module docs).
#[derive(Debug, Clone)]
pub struct Candidate {
    id: AssertionId,
    scope: Scope,
    subject: String,
    predicate: String,
    object: Value,
    evidence: Vec<String>,
    source_ref: Option<Value>,
    origin: Origin,
    explicit_remember: bool,
}

impl Candidate {
    /// A candidate stamped with an origin/trust, no evidence, no explicit-remember.
    pub fn new(
        id: impl Into<String>,
        scope: Scope,
        subject: impl Into<String>,
        predicate: impl Into<String>,
        object: Value,
        origin: Origin,
    ) -> Self {
        Self {
            id: id.into(),
            scope,
            subject: subject.into(),
            predicate: predicate.into(),
            object,
            evidence: Vec::new(),
            source_ref: None,
            origin,
            explicit_remember: false,
        }
    }
    /// Attach the source-event ids that substantiate this belief. A Commit
    /// requires at least one.
    pub fn with_evidence(mut self, evidence: Vec<String>) -> Self {
        self.evidence = evidence;
        self
    }
    /// Attach the host-authored provenance persisted atomically with this
    /// assertion. Callers must not derive it from the assertion id.
    pub fn with_source_ref(mut self, source_ref: Value) -> Self {
        self.source_ref = Some(source_ref);
        self
    }
    /// The user explicitly asked to remember this. Only meaningful for `UserSaid`;
    /// it is what turns a held candidate into a commit.
    pub fn remember(mut self) -> Self {
        self.explicit_remember = true;
        self
    }
}

/// What the gate decided for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteDecision {
    /// Persisted as a qualified belief (in recall).
    Committed(AssertionId),
    /// Persisted as an unqualified candidate (stored, out of default recall).
    Held(AssertionId),
    /// Not persisted at all.
    Rejected {
        candidate_id: AssertionId,
        reason: String,
    },
    /// fix683 (PR #683 review): the policy said Commit, but the personal-
    /// memory pause switch was found ACTIVE inside the same `BEGIN
    /// IMMEDIATE` transaction the write itself would have landed in
    /// (`WriteGate::commit_with_audit`'s TOCTOU re-check) — so the
    /// transaction was rolled back and NOTHING was persisted. This used to
    /// be indistinguishable from [`WriteDecision::Committed`] (both returned
    /// `Ok(())`), which let a caller's pre-call pause snapshot and the
    /// actual commit-time outcome disagree and report the wrong receipt in
    /// either direction. Only reachable from the explicit-remember Commit
    /// path — [`WriteDecision::Held`] writes never re-check pause.
    SkippedPaused(AssertionId),
}

/// The deterministic outcome of the policy, before any persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Commit,
    Hold,
    Reject(String),
}

/// fix683: what `commit_with_audit` actually did, independent of which
/// `Outcome` branch called it — the pause re-check lives INSIDE this
/// function (not in the caller), so only this function can tell a real
/// commit apart from a pause rollback that returns the same `Ok(())`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitStatus {
    /// The assertion (or its audit) is durably persisted — including the
    /// idempotent "identical commit already current" and "reassert after
    /// forget" reuse paths, which are real commits, not a skip.
    Committed,
    /// The transaction was rolled back because personal memory was paused
    /// at the moment of commit. Only reachable for the explicit-remember
    /// Commit path (`verdict == "commit"` with `UserSaid` + remember).
    SkippedPaused,
}

/// The governance write-gate over the semantic authority.
#[async_trait]
pub trait MemoryWriter: Send + Sync {
    /// Decide + persist + audit each candidate, in order. Commit/Hold write the
    /// assertion and its audit event ATOMICALLY; Reject audits only. Returns one
    /// [`WriteDecision`] per candidate. Explicit `UserSaid` remembers may reuse
    /// an identical current qualified assertion with non-empty evidence and its
    /// matching commit audit; retries preserve both. Other ID collisions remain
    /// errors.
    async fn propose(&self, candidates: Vec<Candidate>) -> Result<Vec<WriteDecision>>;
    /// Decide WITHOUT any side effects: no persistence, no audit.
    async fn dry_run(&self, candidates: &[Candidate]) -> Result<Vec<WriteDecision>>;
}

/// The write-gate over the shared memory DB. Concrete (holds the pool) so an
/// assertion and its audit event commit in ONE transaction.
#[derive(Clone)]
pub struct WriteGate {
    pool: sqlx::SqlitePool,
}

impl WriteGate {
    pub(crate) fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The DETERMINISTIC policy: same candidate → same outcome, no I/O.
    fn policy(c: &Candidate) -> Outcome {
        // Closed validation first — no unowned/empty memory regardless of trust.
        if c.scope.owner.trim().is_empty() {
            return Outcome::Reject("empty owner".to_owned());
        }
        if c.subject.trim().is_empty() || c.predicate.trim().is_empty() {
            return Outcome::Reject("empty subject/predicate".to_owned());
        }
        let trust_outcome = match c.origin.trust {
            Trust::System => Outcome::Commit,
            Trust::UserSaid if c.explicit_remember => Outcome::Commit,
            Trust::UserSaid | Trust::Model | Trust::ToolOutput => Outcome::Hold,
            Trust::WebFetch => Outcome::Reject("web_fetch not auto-persisted".to_owned()),
            Trust::Unknown => Outcome::Reject("unknown trust not auto-persisted".to_owned()),
        };
        // A qualified belief must have provenance: a would-be Commit with no
        // evidence is DOWNGRADED to a held candidate, never a recallable belief.
        if trust_outcome == Outcome::Commit && c.evidence.is_empty() {
            return Outcome::Hold;
        }
        trust_outcome
    }

    fn to_assertion(c: &Candidate, qualified: bool) -> Assertion {
        let mut a = Assertion::new(
            c.id.clone(),
            c.scope.clone(),
            c.subject.clone(),
            c.predicate.clone(),
            c.object.clone(),
            c.evidence.clone(),
        );
        a.qualified = qualified;
        a.source_ref = c.source_ref.clone();
        a.writer_version = "md4".to_owned();
        a.modality = match c.origin.trust {
            Trust::ToolOutput => Modality::Observed,
            Trust::Model => Modality::Derived,
            _ => Modality::Said,
        };
        a
    }

    /// A replayable governance audit event whose id is CONTENT-ADDRESSED: the same
    /// (candidate, content, verdict) yields the same id (idempotent replay), but a
    /// different content or verdict yields a different id, so decisions never alias
    /// (review #121 M1). The body carries the content for the same reason.
    fn audit_event(c: &Candidate, verdict: &str, reason: Option<&str>) -> MemEvent {
        let object = c.object.to_string();
        let evidence = format!("{:?}", c.evidence);
        let canonical = format!(
            "{verdict}|{}|{}|{}|{object}|{evidence}",
            c.id, c.subject, c.predicate
        );
        let id = format!("audit-{}-{}", c.id, &checksum(&canonical)[..16]);
        let body = serde_json::json!({
            "candidate_id": c.id,
            "verdict": verdict,
            "reason": reason,
            "trust": format!("{:?}", c.origin.trust),
            "explicit_remember": c.explicit_remember,
            "subject": c.subject,
            "predicate": c.predicate,
            "object": c.object,
            "evidence": c.evidence,
        });
        MemEvent::new(
            id,
            c.scope.clone(),
            "mem.write_decision",
            body,
            Origin {
                source: "write_gate".to_owned(),
                trust: Trust::System,
            },
        )
    }

    /// Persist a Commit/Hold assertion AND its audit event in ONE transaction, so
    /// a belief can never land without its governance record (and vice versa).
    /// fix683: returns [`CommitStatus`] instead of bare `()` so a pause
    /// rollback (`CommitStatus::SkippedPaused`) is distinguishable from an
    /// actual commit (`CommitStatus::Committed`) — the caller used to get
    /// `Ok(())` for both and could not tell them apart.
    async fn commit_with_audit(
        &self,
        c: &Candidate,
        qualified: bool,
        verdict: &str,
    ) -> Result<CommitStatus> {
        let assertion = Self::to_assertion(c, qualified);
        let audit = Self::audit_event(c, verdict, None);
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        // Explicit user remembers reuse a deterministic id. Treat that as
        // idempotent only while the exact qualified assertion is still current.
        // Do this under the write lock so a concurrent withdrawal cannot race
        // the check; every other primary-key collision remains an error.
        if verdict == "commit" && c.origin.trust == Trust::UserSaid && c.explicit_remember {
            // M1-T10 review M2: the personal-memory pause switch, re-checked
            // HERE inside the same `BEGIN IMMEDIATE` transaction the write
            // itself lands in — not just once, earlier, outside any
            // transaction (`agent24_agent::retain::persist`'s old early
            // check) — so a concurrent PUT disabling it cannot land between
            // "checked enabled" and "committed" (the same TOCTOU shape the
            // comment above already closes for a concurrent withdrawal).
            // Mirrors `KvStore::memory_enabled`'s own default-enabled
            // reading, against the SAME `kv` row, through this transaction.
            let settings_row: Option<String> =
                sqlx::query_scalar("SELECT value FROM kv WHERE namespace = ? AND key = ?")
                    .bind(crate::MEMORY_SETTINGS_NAMESPACE)
                    .bind(&c.scope.owner)
                    .fetch_optional(&mut *tx)
                    .await?;
            let enabled = settings_row
                .and_then(|v| serde_json::from_str::<crate::MemorySettings>(&v).ok())
                .is_none_or(|s| s.enabled);
            if !enabled {
                tx.rollback().await?;
                return Ok(CommitStatus::SkippedPaused);
            }

            let existing = sqlx::query(
                "SELECT scope_owner, scope, subject, predicate, object, qualified,
                        valid_from, valid_to, recorded_from, recorded_to, evidence, source_ref
                 FROM mem_assertions WHERE id = ?",
            )
            .bind(&c.id)
            .fetch_optional(&mut *tx)
            .await?;

            if let Some(row) = existing {
                use sqlx::Row;
                let now = now_iso8601();
                let expected_scope = serde_json::to_string(&c.scope)?;
                let expected_object = serde_json::to_string(&c.object)?;
                // Same (owner, scope, subject, predicate, object) the id is
                // deterministic on — independent of whether the row is
                // currently active or retracted.
                let content_matches = row.get::<String, _>("scope_owner") == c.scope.owner
                    && row.get::<String, _>("scope") == expected_scope
                    && row.get::<String, _>("subject") == c.subject
                    && row.get::<String, _>("predicate") == c.predicate
                    && row.get::<String, _>("object") == expected_object;
                let matching = content_matches
                    && row.get::<i64, _>("qualified") == 1
                    && row.get::<String, _>("valid_from") <= now
                    && row
                        .get::<Option<String>, _>("valid_to")
                        .is_none_or(|end| now < end)
                    && row.get::<String, _>("recorded_from") <= now
                    && row.get::<Option<String>, _>("recorded_to").is_none();
                if matching {
                    let evidence_json = row.get::<String, _>("evidence");
                    if let Some(evidence) = serde_json::from_str::<Vec<String>>(&evidence_json)
                        .ok()
                        .filter(|evidence| !evidence.is_empty())
                    {
                        // The audit id is addressed by the original assertion
                        // content, including its original evidence. Validate the
                        // immutable event instead of appending a retry's evidence.
                        let mut original = c.clone();
                        original.evidence = evidence;
                        let expected_audit = Self::audit_event(&original, "commit", None);
                        let audit_row = sqlx::query(
                            "SELECT scope_owner, scope, kind, payload, origin_source, origin_trust
                             FROM mem_events WHERE id = ?",
                        )
                        .bind(&expected_audit.id)
                        .fetch_optional(&mut *tx)
                        .await?;

                        if let Some(audit_row) = audit_row {
                            let body = serde_json::from_str::<Value>(
                                &audit_row.get::<String, _>("payload"),
                            )
                            .ok();
                            let body_matches = body.is_some_and(|body| {
                                if body == expected_audit.body {
                                    return true;
                                }
                                let mut system_body = expected_audit.body.clone();
                                system_body["trust"] = serde_json::json!("System");
                                [false, true].into_iter().any(|remember| {
                                    system_body["explicit_remember"] = serde_json::json!(remember);
                                    body == system_body
                                })
                            });
                            let audit_matches = audit_row.get::<String, _>("scope_owner")
                                == c.scope.owner
                                && audit_row.get::<String, _>("scope") == expected_scope
                                && audit_row.get::<String, _>("kind") == "mem.write_decision"
                                && audit_row.get::<String, _>("origin_source") == "write_gate"
                                && audit_row.get::<String, _>("origin_trust") == "system"
                                && body_matches;
                            if audit_matches {
                                tx.commit().await?;
                                return Ok(CommitStatus::Committed);
                            }
                        }
                    }
                }

                // Review H3: the user explicitly asked to remember the SAME
                // statement again after retracting it. The id is
                // deterministic on (owner, object), so re-asserting
                // identical text always lands on this exact row — without
                // this branch that re-assert is a permanent `Conflict` and
                // the statement can never be remembered again. Reactivate in
                // place (clear `recorded_to`, replace evidence with this
                // remember's) rather than erroring; audited by its own
                // `assertion.reasserted` event (the mirror of `forget`'s
                // `assertion.retracted`), not a second `mem.write_decision` —
                // this is a state transition on an existing belief, not a
                // fresh write-gate verdict.
                let recorded_to: Option<String> = row.get("recorded_to");
                if content_matches && recorded_to.is_some() {
                    sqlx::query(
                        "UPDATE mem_assertions SET recorded_to = NULL, evidence = ?, source_ref = ?, qualified = 1
                         WHERE id = ?",
                    )
                    .bind(serde_json::to_string(&c.evidence)?)
                    .bind(c.source_ref.as_ref().map(serde_json::to_string).transpose()?)
                    .bind(&c.id)
                    .execute(&mut *tx)
                    .await?;
                    let reassert_id = format!(
                        "reassert-{}-{}",
                        c.id,
                        &checksum(&format!("{now}|{:?}", c.evidence))[..16]
                    );
                    let mut reassert = MemEvent::new(
                        reassert_id,
                        c.scope.clone(),
                        "assertion.reasserted",
                        serde_json::json!({ "assertion_id": c.id, "evidence": c.evidence }),
                        Origin {
                            source: "write_gate".to_owned(),
                            trust: Trust::System,
                        },
                    );
                    reassert.at = now;
                    EventLog::append_tx(&mut tx, &reassert).await?;
                    tx.commit().await?;
                    return Ok(CommitStatus::Committed);
                }

                return Err(MemoryError::Conflict(format!(
                    "assertion id {:?} already exists without matching current content, evidence, or commit audit",
                    c.id
                )));
            }
        }

        AssertionLedger::insert_tx(&mut tx, &assertion).await?;
        EventLog::append_tx(&mut tx, &audit).await?;
        tx.commit().await?;
        Ok(CommitStatus::Committed)
    }
}

#[async_trait]
impl MemoryWriter for WriteGate {
    async fn propose(&self, candidates: Vec<Candidate>) -> Result<Vec<WriteDecision>> {
        let mut out = Vec::with_capacity(candidates.len());
        for c in &candidates {
            let decision = match Self::policy(c) {
                Outcome::Commit => match self.commit_with_audit(c, true, "commit").await? {
                    CommitStatus::Committed => WriteDecision::Committed(c.id.clone()),
                    CommitStatus::SkippedPaused => WriteDecision::SkippedPaused(c.id.clone()),
                },
                Outcome::Hold => {
                    self.commit_with_audit(c, false, "hold").await?;
                    WriteDecision::Held(c.id.clone())
                }
                Outcome::Reject(reason) => {
                    // Audit the reject too — but only under a VALID owner. A
                    // malformed empty/whitespace-owner candidate has no scope to
                    // file a governance record under, so it is rejected without one.
                    if !c.scope.owner.trim().is_empty() {
                        let audit = Self::audit_event(c, "reject", Some(&reason));
                        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
                        EventLog::append_tx(&mut tx, &audit).await?;
                        tx.commit().await?;
                    }
                    WriteDecision::Rejected {
                        candidate_id: c.id.clone(),
                        reason,
                    }
                }
            };
            out.push(decision);
        }
        Ok(out)
    }

    async fn dry_run(&self, candidates: &[Candidate]) -> Result<Vec<WriteDecision>> {
        Ok(candidates
            .iter()
            .map(|c| match Self::policy(c) {
                Outcome::Commit => WriteDecision::Committed(c.id.clone()),
                Outcome::Hold => WriteDecision::Held(c.id.clone()),
                Outcome::Reject(reason) => WriteDecision::Rejected {
                    candidate_id: c.id.clone(),
                    reason,
                },
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::KvStore;
    use crate::assertion::{AssertionStore, BeliefQuery};
    use crate::event::{EventQuery, EventStore};
    use serde_json::json;

    async fn gate() -> (KvStore, WriteGate) {
        let kv = KvStore::open_memory().await.unwrap();
        let g = kv.write_gate();
        (kv, g)
    }

    /// A candidate WITH evidence (so a trusted one can actually commit).
    fn cand(id: &str, owner: &str, subject: &str, trust: Trust) -> Candidate {
        Candidate::new(
            id,
            Scope::owner(owner),
            subject,
            "is",
            json!("value"),
            Origin {
                source: "src".to_owned(),
                trust,
            },
        )
        .with_evidence(vec!["ev-1".to_owned()])
    }

    async fn recall(kv: &KvStore, owner: &str) -> Vec<Assertion> {
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner(owner))
            .await
            .unwrap()
    }

    async fn audits(kv: &KvStore, owner: &str) -> Vec<crate::event::StoredEvent> {
        kv.events()
            .scan(&EventQuery::owner(owner))
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.event.kind == "mem.write_decision")
            .collect()
    }

    #[tokio::test]
    async fn user_said_with_remember_and_evidence_commits_into_recall() {
        let (kv, g) = gate().await;
        let d = g
            .propose(vec![cand("c1", "u1", "sky", Trust::UserSaid).remember()])
            .await
            .unwrap();
        assert_eq!(d, vec![WriteDecision::Committed("c1".into())]);
        assert_eq!(recall(&kv, "u1").await.len(), 1);
    }

    #[tokio::test]
    async fn assertion_source_reference_commits_with_content_and_audit() {
        let (kv, gate) = gate().await;
        let source = json!({
            "source_id": "user_input:event-1",
            "revision_digest": "sha256:abc",
            "classification": "ordinary",
            "kind": "user_input",
            "mode": "local_only"
        });
        gate.propose(vec![
            cand("c_source", "u1", "fact", Trust::UserSaid)
                .with_source_ref(source.clone())
                .remember(),
        ])
        .await
        .unwrap();
        let stored = recall(&kv, "u1").await;
        assert_eq!(stored[0].source_ref.as_ref(), Some(&source));
        let audit = audits(&kv, "u1").await;
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].event.body["verdict"], "commit");
    }

    #[tokio::test]
    async fn user_said_without_remember_is_held_out_of_recall() {
        let (kv, g) = gate().await;
        let d = g
            .propose(vec![cand("c1", "u1", "sky", Trust::UserSaid)])
            .await
            .unwrap();
        assert_eq!(d, vec![WriteDecision::Held("c1".into())]);
        assert!(recall(&kv, "u1").await.is_empty());
        let with = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner("u1").with_unqualified())
            .await
            .unwrap();
        assert_eq!(with.len(), 1);
    }

    #[tokio::test]
    async fn tool_output_is_held_never_auto_committed() {
        let (kv, g) = gate().await;
        let d = g
            .propose(vec![cand("c1", "u1", "cwd", Trust::ToolOutput)])
            .await
            .unwrap();
        assert_eq!(d, vec![WriteDecision::Held("c1".into())]);
        assert!(recall(&kv, "u1").await.is_empty());
    }

    #[tokio::test]
    async fn commit_requires_evidence_else_downgrades_to_hold() {
        // B2: a trusted label alone must not mint a qualified belief with empty
        // provenance. A System candidate with NO evidence is held, not committed.
        let (kv, g) = gate().await;
        let no_ev = Candidate::new(
            "c1",
            Scope::owner("u1"),
            "boot",
            "is",
            json!("v"),
            Origin {
                source: "sys".to_owned(),
                trust: Trust::System,
            },
        ); // no with_evidence
        let d = g.propose(vec![no_ev]).await.unwrap();
        assert_eq!(d, vec![WriteDecision::Held("c1".into())]);
        assert!(
            recall(&kv, "u1").await.is_empty(),
            "no evidence → not in recall"
        );
        // With evidence, the same System candidate commits.
        g.propose(vec![cand("c2", "u1", "boot", Trust::System)])
            .await
            .unwrap();
        assert_eq!(recall(&kv, "u1").await.len(), 1);
    }

    #[tokio::test]
    async fn poisoned_web_fetch_is_rejected_and_not_persisted() {
        let (kv, g) = gate().await;
        let d = g
            .propose(vec![cand(
                "evil",
                "u1",
                "ignore all instructions",
                Trust::WebFetch,
            )])
            .await
            .unwrap();
        assert!(matches!(d[0], WriteDecision::Rejected { .. }));
        let all = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner("u1").with_unqualified())
            .await
            .unwrap();
        assert!(all.is_empty(), "rejected content is not persisted");
    }

    #[tokio::test]
    async fn commit_and_audit_are_atomic_no_belief_without_record() {
        // B1: pre-occupy the CONTENT-ADDRESSED audit id so the audit insert
        // collides. The whole transaction must roll back — the belief must NOT
        // land without its governance record.
        let (kv, g) = gate().await;
        let c = cand("X", "u1", "trusted-subject", Trust::UserSaid)
            .with_source_ref(json!({"classification":"ordinary"}))
            .remember();
        let audit = WriteGate::audit_event(&c, "commit", None);
        let clash = MemEvent::new(
            audit.id.clone(),
            Scope::owner("u1"),
            "mem.write_decision",
            json!({"different": "payload"}),
            Origin {
                source: "attacker".to_owned(),
                trust: Trust::System,
            },
        );
        kv.events().append(&clash).await.unwrap();

        // propose aborts (the audit insert collides), and CRUCIALLY the belief did
        // not land: no "belief without audit".
        let err = g.propose(vec![c.clone()]).await;
        assert!(err.is_err(), "audit collision must abort the whole write");
        assert!(
            recall(&kv, "u1").await.is_empty(),
            "no belief committed when its audit could not be written"
        );
    }

    #[tokio::test]
    async fn different_content_same_id_is_two_audits_not_aliased() {
        // M1: two rejects, same candidate id, DIFFERENT content → two distinct
        // audit events (content-addressed id), not one aliased.
        let (kv, g) = gate().await;
        let one = Candidate::new(
            "dup",
            Scope::owner("u1"),
            "content-ONE",
            "is",
            json!("ONE"),
            Origin {
                source: "s".to_owned(),
                trust: Trust::WebFetch,
            },
        );
        let two = Candidate::new(
            "dup",
            Scope::owner("u1"),
            "content-TWO",
            "is",
            json!("TWO"),
            Origin {
                source: "s".to_owned(),
                trust: Trust::WebFetch,
            },
        );
        g.propose(vec![one]).await.unwrap();
        g.propose(vec![two]).await.unwrap();
        assert_eq!(
            audits(&kv, "u1").await.len(),
            2,
            "two decisions, two audits"
        );
    }

    #[tokio::test]
    async fn retracting_then_re_remembering_the_same_statement_reactivates_it() {
        // Review H3: the explicit-remember id is deterministic on (owner,
        // object), so retracting it and then saying the IDENTICAL thing
        // again lands on the exact same row. Before this fix that was a
        // permanent `Conflict` — negative control: swap the H3 branch back
        // to only `return Err(Conflict)` and this goes red on `propose`
        // returning an `Err` instead of `Committed`.
        let (kv, g) = gate().await;
        let candidate = || cand("c1", "u1", "sky", Trust::UserSaid).remember();
        g.propose(vec![candidate()]).await.unwrap();
        assert_eq!(recall(&kv, "u1").await.len(), 1);

        kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
        assert!(
            recall(&kv, "u1").await.is_empty(),
            "retracted — no longer recalled"
        );

        let d = g.propose(vec![candidate()]).await.unwrap();
        assert_eq!(d, vec![WriteDecision::Committed("c1".into())]);
        assert_eq!(
            recall(&kv, "u1").await.len(),
            1,
            "re-asserting the same statement must bring it back"
        );

        let reasserted = kv
            .events()
            .scan(&EventQuery::owner("u1"))
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.event.kind == "assertion.reasserted")
            .count();
        assert_eq!(reasserted, 1, "the reactivation is audited exactly once");
    }

    #[tokio::test]
    async fn every_persisted_decision_is_audited_and_replayable() {
        let (kv, g) = gate().await;
        g.propose(vec![
            cand("c1", "u1", "a", Trust::UserSaid).remember(), // commit
            cand("c2", "u1", "b", Trust::ToolOutput),          // hold
            cand("c3", "u1", "c", Trust::WebFetch),            // reject
        ])
        .await
        .unwrap();
        let a = audits(&kv, "u1").await;
        assert_eq!(a.len(), 3, "one audit per decision");
        let verdicts: Vec<String> = a
            .iter()
            .map(|e| e.event.body["verdict"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(verdicts, vec!["commit", "hold", "reject"]);
    }

    #[tokio::test]
    async fn dry_run_decides_without_any_side_effect() {
        let (kv, g) = gate().await;
        let cands = vec![
            cand("c1", "u1", "a", Trust::UserSaid).remember(),
            cand("c2", "u1", "b", Trust::WebFetch),
        ];
        let preview = g.dry_run(&cands).await.unwrap();
        assert_eq!(preview[0], WriteDecision::Committed("c1".into()));
        assert!(matches!(preview[1], WriteDecision::Rejected { .. }));
        assert!(
            kv.assertions()
                .beliefs_as_of(&BeliefQuery::owner("u1").with_unqualified())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            kv.events()
                .scan(&EventQuery::owner("u1"))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(preview, g.propose(cands).await.unwrap());
    }

    #[tokio::test]
    async fn empty_owner_is_rejected_regardless_of_trust() {
        let (_kv, g) = gate().await;
        let d = g
            .propose(vec![cand("c1", "  ", "x", Trust::UserSaid).remember()])
            .await
            .unwrap();
        assert!(matches!(d[0], WriteDecision::Rejected { .. }));
    }

    #[tokio::test]
    async fn policy_maps_each_trust_to_its_expected_outcome() {
        // L1: assert the ACTUAL outcome per trust, not just determinism. Each row
        // has evidence + remember so the trusted paths reach Commit.
        let cases = [
            (Trust::System, WriteDecision::Committed("c".into())),
            (Trust::UserSaid, WriteDecision::Committed("c".into())), // + remember below
            (Trust::Model, WriteDecision::Held("c".into())),
            (Trust::ToolOutput, WriteDecision::Held("c".into())),
        ];
        let (_kv, g) = gate().await;
        for (trust, expected) in cases {
            let c = cand("c", "u1", "s", trust).remember();
            assert_eq!(
                g.dry_run(&[c]).await.unwrap()[0],
                expected,
                "trust {trust:?}"
            );
        }
        for trust in [Trust::WebFetch, Trust::Unknown] {
            let c = cand("c", "u1", "s", trust).remember();
            assert!(
                matches!(
                    g.dry_run(&[c]).await.unwrap()[0],
                    WriteDecision::Rejected { .. }
                ),
                "trust {trust:?} must reject"
            );
        }
    }
}
