//! K1-6a.2 (ADR-K1-03 "K1-6a 实现切片建议" item 2): the host-side
//! **authorization decision service** — "does this module tool call have
//! valid host-recorded consent, and must it still receive per-call approval?"
//!
//! This service is consumed by module-tool authorization and advert checks.
//! It does not mutate 6a.1's storage (`agent24_store::module_consents`) — it only *consults*
//! [`agent24_store::Store::lookup_module_consent`].
//!
//! ## What `Allow` means — and does not mean
//!
//! A valid module consent is necessary to call any module tool, but does not
//! by itself waive per-call approval. `Allow` therefore carries
//! `per_call_approval`: `Read` is false; `WriteLocal` is true except for the
//! narrowly qualified, host-verified reversible-draft case in L-APPR-5;
//! `Exec` is always true, and `External` is also true because module consent
//! is not the required `tool → exact target` standing-grant shape. Only policy
//! or consent failures deny the call here. The module-tool adapter carries this
//! flag into the host's dispatch path, whose existing approval gate runs for
//! every gated host risk. These rules agree with `RiskClass::escape_rank`'s
//! description of which classes a standing grant can pre-answer.
//!
//! This gate also does not bypass policy/organizational/source restrictions —
//! the `org_restricted`/`source_restricted` placeholders on
//! [`ConsentGateRequest`] stand in for the real policy engine ADR §2
//! describes (LocalOnly, org/admin/source limits) that does not exist yet
//! (K1-6b); when either is `true` this gate denies unconditionally, before
//! even looking at the consent store.
//!
//! Both overrides apply even to an otherwise-perfectly-matching `Granted`
//! consent (ADR §2: "即使用户给某项工具常驻许可，每次调用仍受当前风险等级
//! ...组织限制...约束"; 反例 2/8: a valid grant/capability must never cross
//! a stricter org/source policy).
//!
//! ## Fail-closed shape
//!
//! [`ConsentGate::authorize`] produces [`ConsentGateDecision::Allow`] only
//! when the policy placeholders are clear and the store lookup resolves to
//! [`agent24_store::ConsentLookup::Granted`], regardless of risk class.
//! Every other reachable state — no record, a stale/expired/denied record, or
//! the store call itself failing — denies. The match on
//! [`agent24_store::ConsentLookup`] is
//! exhaustive with no wildcard arm, so a future variant added to that enum
//! is a compile error here rather than silently falling through to `Allow`.

use agent24_protocol::RiskClass;
use agent24_store::{ConsentLookup, ConsentSource, ModuleConsentRecord, Store};
use async_trait::async_trait;

/// One call's identity plus the policy placeholders the task spec calls
/// "调用上下文中的来源与组织限制占位" — kept as two separate booleans (rather
/// than one combined flag) so a future audit view can tell which stricter
/// limit fired, even though this slice treats both identically (either one
/// alone denies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentGateRequest {
    pub module: String,
    pub op: String,
    pub module_version: String,
    /// The permission-scope fingerprint for the tool AS IT EXISTS NOW —
    /// callers must re-derive this from the tool's current
    /// `agent24_store::ToolPermissionSummary::scope_fingerprint`, never read
    /// it off a stored row (the same discipline
    /// `Store::lookup_module_consent` already documents; this service does
    /// not re-derive it itself because it has no access to the host's tool
    /// registry).
    pub current_scope_fingerprint: String,
    /// Host-computed side-effect classification, never the module's manifest
    /// declaration. `HostRiskLevel` is the enablement-time rating, a different axis.
    pub risk: RiskClass,
    /// Untrusted manifest declaration, copied from the validated tool entry.
    /// It is only a claim; exemption also requires host verification below.
    pub reversible_draft_declared: bool,
    /// Facts determined by the host from the registered tool and its execution
    /// boundary. Missing evidence is intentionally ineligible.
    pub draft_verification: Option<HostVerifiedDraftSemantics>,
    /// Placeholder for ADR §2's "组织、管理员、来源本身的更严格限制" — the
    /// organizational half. `true` denies regardless of consent state.
    pub org_restricted: bool,
    /// Placeholder for the "来源" (provenance) half of the same ADR clause,
    /// kept separate from `org_restricted` for future audit distinction.
    /// `true` denies regardless of consent state.
    pub source_restricted: bool,
}

/// Host-verified facts required to classify a `WriteLocal` call as a reversible
/// draft. A module's manifest never supplies these facts. The dispatch host
/// must construct this only after verifying the actual storage target and
/// operation semantics; it must use `None` when any fact cannot be verified.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostVerifiedDraftSemantics {
    pub writes_only_to_module_data_dir: bool,
    pub has_revision_history: bool,
    pub can_be_undone: bool,
    pub modifies_authoritative_content: bool,
    pub has_commit_semantics: bool,
    pub has_delete_semantics: bool,
    pub external: bool,
}

impl HostVerifiedDraftSemantics {
    fn qualifies(&self) -> bool {
        self.writes_only_to_module_data_dir
            && self.has_revision_history
            && self.can_be_undone
            && !self.modifies_authoritative_content
            && !self.has_commit_semantics
            && !self.has_delete_semantics
            && !self.external
    }
}

/// Closed set of deny reasons (task spec): `not_granted` / `stale` /
/// `expired` / `denied` / `policy_restricted`. The
/// four store-backed variants carry the record that produced them (when one
/// exists) for a future caller's audit/debugging use, same shape as
/// `agent24_store::ConsentLookup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentDenyReason {
    /// No matching record at all — includes BOTH the task spec's "无记录"
    /// and "查询失败" cases (a store error cannot be told apart from "no
    /// row" by a caller, so both fail closed to the same reason).
    NotGranted,
    Stale(ModuleConsentRecord),
    Expired(ModuleConsentRecord),
    Denied(ModuleConsentRecord),
    /// The grant was revoked (by the user, or by a module lifecycle event —
    /// disable / uninstall / scope-changing upgrade). Kept distinct from
    /// `Denied`: revocation is a later withdrawal of a once-given consent,
    /// not a refusal at enable time (ADR-K1-03 §3). `None` when lifecycle
    /// invalidation left no record to point at.
    Revoked(Option<ModuleConsentRecord>),
    /// `org_restricted` or `source_restricted` was set on the request.
    PolicyRestricted,
}

impl ConsentDenyReason {
    /// Stable slug for logging/audit, matching the task spec's closed set
    /// exactly (mirrors `agent24_policy::guardian::Escalation::reason_code`).
    #[must_use]
    pub fn reason_code(&self) -> &'static str {
        match self {
            ConsentDenyReason::NotGranted => "not_granted",
            ConsentDenyReason::Stale(_) => "stale",
            ConsentDenyReason::Expired(_) => "expired",
            ConsentDenyReason::Denied(_) => "denied",
            ConsentDenyReason::Revoked(_) => "revoked",
            ConsentDenyReason::PolicyRestricted => "policy_restricted",
        }
    }
}

/// Opaque reference to the consent grant that authorized the module call —
/// NOT a capability or bearer token (ADR interface section, item 1: this layer
/// answers module consent only and "不复用...bearer 作为用户同意记录"; item
/// 3: module/third-party code cannot mint or extend one). A future caller
/// may use it for audit logging; it grants nothing by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRef {
    pub module: String,
    pub op: String,
    pub scope_fingerprint: String,
    pub decided_at: String,
    pub expires_at: String,
}

impl From<&ModuleConsentRecord> for GrantRef {
    fn from(record: &ModuleConsentRecord) -> Self {
        Self {
            module: record.module.clone(),
            op: record.op.clone(),
            scope_fingerprint: record.scope_fingerprint.clone(),
            decided_at: record.decided_at.clone(),
            expires_at: record.expires_at.clone(),
        }
    }
}

/// The service's answer for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentGateDecision {
    Allow {
        grant_ref: GrantRef,
        /// Whether the caller must still request approval for this call.
        /// L-APPR-5 requires this for `WriteLocal`, except a host-verified
        /// reversible draft with matching first-party consent, and always for
        /// `Exec`. This gate also requires it for `External` until consent is
        /// scoped as `tool → exact target` (ADR-K1-03). `Read` also skips it.
        per_call_approval: bool,
    },
    Deny {
        reason: ConsentDenyReason,
    },
}

impl ConsentGateDecision {
    /// Whether consent exists. This does not waive per-call approval.
    #[must_use]
    pub fn has_consent(&self) -> bool {
        matches!(self, ConsentGateDecision::Allow { .. })
    }
}

pub struct ModuleConsentAuthorization {
    gate: std::sync::Arc<dyn ConsentGate>,
    request: ConsentGateRequest,
    now: std::sync::Arc<dyn Fn() -> String + Send + Sync>,
}

impl ModuleConsentAuthorization {
    #[must_use]
    pub fn new(
        gate: std::sync::Arc<dyn ConsentGate>,
        request: ConsentGateRequest,
        now: std::sync::Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Self {
        Self { gate, request, now }
    }

    async fn decision(&self, module: &str, operation: &str) -> ConsentGateDecision {
        if self.request.module != module || self.request.op != operation {
            return ConsentGateDecision::Deny {
                reason: ConsentDenyReason::NotGranted,
            };
        }
        self.gate.authorize(&self.request, &(self.now)()).await
    }
}

#[async_trait]
impl agent24_tools::ModuleToolAuthorization for ModuleConsentAuthorization {
    async fn authorize(
        &self,
        module: &str,
        operation: &str,
        _ctx: &agent24_tools::ToolContext,
    ) -> Result<agent24_tools::ModuleToolGrantContext, agent24_domain::tool::ModuleToolCallError>
    {
        use agent24_domain::tool::{ModuleToolCallError, ModuleToolErrorCode};
        match self.decision(module, operation).await {
            ConsentGateDecision::Allow {
                grant_ref,
                per_call_approval,
            } => Ok(agent24_tools::ModuleToolGrantContext {
                authorized_resources: Vec::new(),
                authorization_ref: format!(
                    "{}:{}",
                    grant_ref.scope_fingerprint, grant_ref.decided_at
                ),
                per_call_approval,
            }),
            ConsentGateDecision::Deny { reason } => {
                let details = matches!(reason, ConsentDenyReason::Revoked(_))
                    .then(|| serde_json::json!({"reason": reason.reason_code()}));
                Err(ModuleToolCallError::Module {
                    code: ModuleToolErrorCode::PermissionDenied,
                    retryable: false,
                    details,
                    unknown_code: None,
                })
            }
        }
    }

    async fn has_current_consent(&self, module: &str, operation: &str) -> bool {
        matches!(
            self.decision(module, operation).await,
            ConsentGateDecision::Allow { .. }
        )
    }
}

/// The authorization judgment service itself. A trait (task spec: "提供服务、
/// trait") so K1-5.3's dispatch adapter and live-advert checks depend on this
/// abstraction, not a concrete store-backed type, the same way
/// `agent24_tools::ApprovalGate` decouples dispatch from any one broker.
#[async_trait]
pub trait ConsentGate: Send + Sync {
    /// `now` is the caller-supplied current instant (RFC 3339), threaded
    /// through unchanged to `Store::lookup_module_consent` — this service
    /// does not read a clock itself, for the same testability reason the
    /// store layer takes it as a parameter.
    async fn authorize(&self, request: &ConsentGateRequest, now: &str) -> ConsentGateDecision;
}

/// The only implementation this slice ships: judges against 6a.1's
/// `module_consents` table via [`Store::lookup_module_consent`].
pub struct StoreConsentGate {
    store: Store,
}

impl StoreConsentGate {
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ConsentGate for StoreConsentGate {
    async fn authorize(&self, request: &ConsentGateRequest, now: &str) -> ConsentGateDecision {
        // Rule 2 / ADR §2's "更严组织、来源限制优先": these are policy gates
        // layered ON TOP of module consent, so they are checked — and can
        // deny — before any store lookup even runs. A future caller must
        // never be able to get `Allow` out of a matching consent row alone
        // when either placeholder is set.
        if request.org_restricted || request.source_restricted {
            return ConsentGateDecision::Deny {
                reason: ConsentDenyReason::PolicyRestricted,
            };
        }

        let lookup = match self
            .store
            .lookup_module_consent(
                &request.module,
                &request.op,
                &request.module_version,
                &request.current_scope_fingerprint,
                now,
            )
            .await
        {
            Ok(lookup) => lookup,
            // Task spec rule 4: "查询失败...一律 Deny". There is no distinct
            // reason code for a store error in the task's closed set, and a
            // caller cannot tell "no row" from "could not ask" from the
            // outside — both must behave identically, so both map to
            // `NotGranted`.
            Err(err) => {
                tracing::error!("module consent lookup failed ({err}); denying call");
                return ConsentGateDecision::Deny {
                    reason: ConsentDenyReason::NotGranted,
                };
            }
        };

        // Exhaustive, no wildcard: adding a new `ConsentLookup` variant in
        // `agent24-store` without updating this match is a compile error,
        // not a silent fall-through to `Allow`.
        match lookup {
            ConsentLookup::NotGranted => ConsentGateDecision::Deny {
                reason: ConsentDenyReason::NotGranted,
            },
            ConsentLookup::Denied(record) => ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Denied(record),
            },
            ConsentLookup::Stale(record) => ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Stale(record),
            },
            ConsentLookup::Expired(record) => ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Expired(record),
            },
            ConsentLookup::Revoked(record) => ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Revoked(record),
            },
            ConsentLookup::Granted(record) => {
                // The only WriteLocal exception is a narrow reversible draft:
                // the validated manifest marker is merely a claim, while the
                // host supplies verified operation/path semantics. Source and
                // consent fingerprint come from the matched host consent row.
                // Every uncertain or missing fact falls back to ordinary
                // per-call approval.
                let reversible_draft = request.risk == RiskClass::WriteLocal
                    && request.reversible_draft_declared
                    && record.source == ConsentSource::FirstParty
                    && request
                        .draft_verification
                        .as_ref()
                        .is_some_and(HostVerifiedDraftSemantics::qualifies);
                ConsentGateDecision::Allow {
                    grant_ref: GrantRef::from(&record),
                    per_call_approval: request.risk.requires_approval() && !reversible_draft,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_store::{ConsentSource, HostRiskLevel, ToolPermissionSummary};

    async fn store() -> Store {
        Store::open_memory().await.unwrap()
    }

    fn summary(source: ConsentSource, risk: HostRiskLevel) -> ToolPermissionSummary {
        ToolPermissionSummary::new(
            "documenting",
            "read_doc",
            "1.0.0",
            source,
            Some("doc body".to_owned()),
            None,
            None,
            risk,
        )
    }

    fn request_for(summary: &ToolPermissionSummary, risk: RiskClass) -> ConsentGateRequest {
        ConsentGateRequest {
            module: summary.module.clone(),
            op: summary.op.clone(),
            module_version: summary.module_version.clone(),
            current_scope_fingerprint: summary.scope_fingerprint(),
            risk,
            reversible_draft_declared: false,
            draft_verification: None,
            org_restricted: false,
            source_restricted: false,
        }
    }

    fn eligible_draft_request(summary: &ToolPermissionSummary) -> ConsentGateRequest {
        ConsentGateRequest {
            reversible_draft_declared: true,
            draft_verification: Some(HostVerifiedDraftSemantics {
                writes_only_to_module_data_dir: true,
                has_revision_history: true,
                can_be_undone: true,
                ..HostVerifiedDraftSemantics::default()
            }),
            ..request_for(summary, RiskClass::WriteLocal)
        }
    }

    // ── ADR反例1/4 — 无同意记录时一律拒绝（第一方/手动安装均然） ────────────

    #[tokio::test]
    async fn no_consent_record_denies_with_not_granted() {
        let store = store().await;
        let gate = StoreConsentGate::new(store);
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        let req = request_for(&s, RiskClass::Read);

        let decision = gate.authorize(&req, "2026-10-08T00:00:00Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::NotGranted
            }
        ));
    }

    #[tokio::test]
    async fn manual_install_without_explicit_consent_stays_unauthorized() {
        // ADR 反例4 second half: "第三方/手动安装仍为高风险未授权" — even
        // though `ToolPermissionSummary::new` already floors its risk at
        // `High`, that alone must not authorize anything; only an explicit
        // grant does.
        let store = store().await;
        let gate = StoreConsentGate::new(store);
        let s = summary(ConsentSource::ManualInstall, HostRiskLevel::Low);
        assert_eq!(s.risk, HostRiskLevel::High);
        let req = request_for(&s, RiskClass::Read);

        let decision = gate.authorize(&req, "2026-10-08T00:00:00Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::NotGranted
            }
        ));
    }

    // ── L-APPR-5 matrix: consent is required for every risk class. Read skips
    // per-call approval; WriteLocal can skip only for a qualified reversible
    // draft. Exec always asks; External needs a tool → exact target
    // standing-grant shape.

    #[tokio::test]
    async fn every_risk_class_requires_matching_consent_and_sets_approval_by_law() {
        for risk in [
            RiskClass::Read,
            RiskClass::WriteLocal,
            RiskClass::Exec,
            RiskClass::External,
        ] {
            // No record denies regardless of risk: consent authorizes module
            // access but cannot itself replace per-call approval.
            let missing_store = store().await;
            let missing_gate = StoreConsentGate::new(missing_store);
            let missing_summary = summary(ConsentSource::FirstParty, HostRiskLevel::High);
            let missing = missing_gate
                .authorize(&request_for(&missing_summary, risk), "2026-10-08T00:00:01Z")
                .await;
            assert!(
                matches!(
                    missing,
                    ConsentGateDecision::Deny {
                        reason: ConsentDenyReason::NotGranted
                    }
                ),
                "{risk:?} without consent must deny"
            );

            // A matching consent authorizes the module call. Approval remains
            // per-call for every class except Read (L-APPR-5). External also
            // stays per-call until a tool → exact target grant exists.
            let granted_store = store().await;
            let granted_summary = summary(ConsentSource::FirstParty, HostRiskLevel::High);
            granted_store
                .grant_module_consent(
                    &granted_summary,
                    "2026-10-08T00:00:00Z",
                    "2026-11-08T00:00:00Z",
                )
                .await
                .unwrap();
            let granted_gate = StoreConsentGate::new(granted_store);
            let granted = granted_gate
                .authorize(&request_for(&granted_summary, risk), "2026-10-08T00:00:01Z")
                .await;
            match granted {
                ConsentGateDecision::Allow {
                    grant_ref,
                    per_call_approval,
                } => {
                    assert_eq!(grant_ref.module, "documenting");
                    assert_eq!(grant_ref.op, "read_doc");
                    assert_eq!(
                        grant_ref.scope_fingerprint,
                        granted_summary.scope_fingerprint()
                    );
                    let expected = match risk {
                        RiskClass::Read => false,
                        RiskClass::WriteLocal | RiskClass::Exec | RiskClass::External => true,
                    };
                    assert_eq!(per_call_approval, expected, "{risk:?} L-APPR-5 behavior");
                }
                ConsentGateDecision::Deny { reason } => {
                    panic!("{risk:?} with consent must allow: {reason:?}")
                }
            }
        }
    }

    // K1-6a.5: only the host-verified, first-party, consent-matched,
    // reversible draft subset may skip per-call approval.
    #[tokio::test]
    async fn eligible_reversible_draft_skips_per_call_approval() {
        let store = store().await;
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let decision = StoreConsentGate::new(store)
            .authorize(&eligible_draft_request(&s), "2026-10-08T00:00:01Z")
            .await;
        assert!(matches!(
            decision,
            ConsentGateDecision::Allow {
                per_call_approval: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn eligible_draft_without_enablement_consent_is_denied() {
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        let decision = StoreConsentGate::new(store().await)
            .authorize(&eligible_draft_request(&s), "2026-10-08T00:00:01Z")
            .await;
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::NotGranted
            }
        ));
    }

    #[tokio::test]
    async fn draft_exemption_requires_first_party_source() {
        let store = store().await;
        let s = summary(ConsentSource::ManualInstall, HostRiskLevel::High);
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let decision = StoreConsentGate::new(store)
            .authorize(&eligible_draft_request(&s), "2026-10-08T00:00:01Z")
            .await;
        assert!(matches!(
            decision,
            ConsentGateDecision::Allow {
                per_call_approval: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn draft_exemption_requires_current_matching_consent_fingerprint() {
        let store = store().await;
        let granted = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&granted, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let mut changed = eligible_draft_request(&granted);
        changed.current_scope_fingerprint.push_str("-changed");
        let decision = StoreConsentGate::new(store)
            .authorize(&changed, "2026-10-08T00:00:01Z")
            .await;
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Stale(_)
            }
        ));
    }

    #[tokio::test]
    async fn unverified_or_ineligible_draft_semantics_keep_per_call_approval() {
        type DraftMutation = (&'static str, fn(&mut ConsentGateRequest));
        let cases: [DraftMutation; 8] = [
            ("marker absent", |r: &mut ConsentGateRequest| {
                r.reversible_draft_declared = false;
            }),
            ("verification unavailable", |r: &mut ConsentGateRequest| {
                r.draft_verification = None;
            }),
            ("commit semantics", |r: &mut ConsentGateRequest| {
                r.draft_verification.as_mut().unwrap().has_commit_semantics = true;
            }),
            ("outside private storage", |r: &mut ConsentGateRequest| {
                r.draft_verification
                    .as_mut()
                    .unwrap()
                    .writes_only_to_module_data_dir = false;
            }),
            ("delete semantics", |r: &mut ConsentGateRequest| {
                r.draft_verification.as_mut().unwrap().has_delete_semantics = true;
            }),
            ("authoritative write", |r: &mut ConsentGateRequest| {
                r.draft_verification
                    .as_mut()
                    .unwrap()
                    .modifies_authoritative_content = true;
            }),
            ("not reversible", |r: &mut ConsentGateRequest| {
                r.draft_verification.as_mut().unwrap().can_be_undone = false;
            }),
            ("no revision history", |r: &mut ConsentGateRequest| {
                r.draft_verification.as_mut().unwrap().has_revision_history = false;
            }),
        ];
        for (name, mutate) in cases {
            let store = store().await;
            let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
            store
                .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
                .await
                .unwrap();
            let mut request = eligible_draft_request(&s);
            mutate(&mut request);
            let decision = StoreConsentGate::new(store)
                .authorize(&request, "2026-10-08T00:00:01Z")
                .await;
            assert!(
                matches!(
                    decision,
                    ConsentGateDecision::Allow {
                        per_call_approval: true,
                        ..
                    }
                ),
                "{name} must retain per-call approval"
            );
        }
    }

    #[tokio::test]
    async fn external_risk_never_uses_draft_exemption() {
        let store = store().await;
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let mut request = eligible_draft_request(&s);
        request.risk = RiskClass::External;
        let decision = StoreConsentGate::new(store)
            .authorize(&request, "2026-10-08T00:00:01Z")
            .await;
        assert!(matches!(
            decision,
            ConsentGateDecision::Allow {
                per_call_approval: true,
                ..
            }
        ));
    }

    // ── ADR反例2/8 — 更严组织/来源限制优先，哪怕许可完全匹配 ────────────────

    #[tokio::test]
    async fn org_restriction_denies_even_with_a_matching_granted_consent() {
        let store = store().await;
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let gate = StoreConsentGate::new(store);
        let mut req = request_for(&s, RiskClass::Read);
        req.org_restricted = true;

        let decision = gate.authorize(&req, "2026-10-08T00:00:01Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::PolicyRestricted
            }
        ));
    }

    #[tokio::test]
    async fn source_restriction_denies_even_with_a_matching_granted_consent() {
        let store = store().await;
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let gate = StoreConsentGate::new(store);
        let mut req = request_for(&s, RiskClass::Read);
        req.source_restricted = true;

        let decision = gate.authorize(&req, "2026-10-08T00:00:01Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::PolicyRestricted
            }
        ));
    }

    // ── ADR反例6 — 到期 / 版本或范围变化 ⇒ 拒绝 ─────────────────────────────

    #[tokio::test]
    async fn widened_scope_fingerprint_denies_with_stale() {
        let store = store().await;
        let old = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&old, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let gate = StoreConsentGate::new(store);

        // Same module/op/version, but the host now exposes a wider scope —
        // a different fingerprint than what was granted.
        let widened = ToolPermissionSummary::new(
            &old.module,
            &old.op,
            &old.module_version,
            old.source,
            Some("doc body AND metadata".to_owned()),
            None,
            None,
            old.risk,
        );
        let req = request_for(&widened, RiskClass::Read);

        let decision = gate.authorize(&req, "2026-10-08T00:00:01Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Stale(_)
            }
        ));
    }

    #[tokio::test]
    async fn module_version_bump_denies_with_stale_even_with_identical_scope_text() {
        let store = store().await;
        let old = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&old, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let gate = StoreConsentGate::new(store);

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
        let req = request_for(&upgraded, RiskClass::Read);

        let decision = gate.authorize(&req, "2026-10-08T00:00:01Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Stale(_)
            }
        ));
    }

    #[tokio::test]
    async fn expired_consent_denies_with_expired() {
        let store = store().await;
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-10-08T00:00:00Z")
            .await
            .unwrap();
        let gate = StoreConsentGate::new(store);
        let req = request_for(&s, RiskClass::Read);

        let decision = gate.authorize(&req, "2026-10-08T00:00:01Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Expired(_)
            }
        ));
    }

    #[tokio::test]
    async fn revoked_consent_denies_with_a_distinct_revoked_reason() {
        let store = store().await;
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        store
            .revoke_module_consent(&s.module, None, "2026-10-08T00:00:05Z")
            .await
            .unwrap();
        let gate = StoreConsentGate::new(store);
        let req = request_for(&s, RiskClass::Read);

        let decision = gate.authorize(&req, "2026-10-08T00:00:06Z").await;
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Revoked(_)
            }
        ));
    }

    // ── ADR反例5 (fragment) — 已拒绝/撤销记录下的新调用立即拒绝 ─────────────

    #[tokio::test]
    async fn denied_consent_denies_with_denied() {
        let store = store().await;
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .deny_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();
        let gate = StoreConsentGate::new(store);
        let req = request_for(&s, RiskClass::Read);

        let decision = gate.authorize(&req, "2026-10-08T00:00:01Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::Denied(_)
            }
        ));
    }

    // ── rule 4 — a store-level query failure fails closed, same as "no row" ─

    #[tokio::test]
    async fn a_store_query_failure_denies_with_not_granted() {
        let store = store().await;
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        store
            .grant_module_consent(&s, "2026-10-08T00:00:00Z", "2026-11-08T00:00:00Z")
            .await
            .unwrap();

        // Simulate the lookup query itself failing (not just "no row") by
        // dropping the table out from under it — the documented test-only
        // escape hatch for exactly this kind of tampering test.
        sqlx::query("DROP TABLE module_consents")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();

        let gate = StoreConsentGate::new(store);
        let req = request_for(&s, RiskClass::Read);

        let decision = gate.authorize(&req, "2026-10-08T00:00:01Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::NotGranted
            }
        ));
    }

    // ── policy restriction short-circuits BEFORE even touching the store ───

    #[tokio::test]
    async fn policy_restriction_denies_even_with_no_consent_record_at_all() {
        let store = store().await;
        let gate = StoreConsentGate::new(store);
        let s = summary(ConsentSource::FirstParty, HostRiskLevel::Low);
        let mut req = request_for(&s, RiskClass::Read);
        req.org_restricted = true;

        let decision = gate.authorize(&req, "2026-10-08T00:00:00Z").await;
        assert!(!decision.has_consent());
        assert!(matches!(
            decision,
            ConsentGateDecision::Deny {
                reason: ConsentDenyReason::PolicyRestricted
            }
        ));
    }

    #[test]
    fn reason_codes_match_the_task_specs_closed_set() {
        let dummy = ModuleConsentRecord {
            module: "m".to_owned(),
            op: "op".to_owned(),
            module_version: "1.0.0".to_owned(),
            scope_fingerprint: "sha256:x".to_owned(),
            source: ConsentSource::FirstParty,
            risk: HostRiskLevel::Low,
            readable: None,
            writable: None,
            external: None,
            decision: agent24_store::ConsentDecision::Granted,
            decided_at: "2026-10-08T00:00:00.000Z".to_owned(),
            expires_at: "2026-11-08T00:00:00.000Z".to_owned(),
        };
        assert_eq!(ConsentDenyReason::NotGranted.reason_code(), "not_granted");
        assert_eq!(
            ConsentDenyReason::Stale(dummy.clone()).reason_code(),
            "stale"
        );
        assert_eq!(
            ConsentDenyReason::Expired(dummy.clone()).reason_code(),
            "expired"
        );
        assert_eq!(
            ConsentDenyReason::Denied(dummy.clone()).reason_code(),
            "denied"
        );
        assert_eq!(
            ConsentDenyReason::Revoked(Some(dummy)).reason_code(),
            "revoked"
        );
        assert_eq!(ConsentDenyReason::Revoked(None).reason_code(), "revoked");
        assert_eq!(
            ConsentDenyReason::PolicyRestricted.reason_code(),
            "policy_restricted"
        );
    }
}
