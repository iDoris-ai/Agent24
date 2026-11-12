//! Dev-bridge `ComponentRuntime` tier. One hardcoded profile: `wechat-bridge`
//! may call `inbound.ingest` for eight hours. The daemon does not mint a
//! component token when a component is enabled.

use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

use agent24_store::{
    AuditRef, AuditTimestamp, COMPONENT_RUNTIME_TTL_SECONDS, CapabilityDenyReason,
    CapabilityRevokeReason, CapabilityTokenAuditEvent, DEV_BRIDGE_COMPONENT_ID,
    DEV_BRIDGE_OPERATION, Store,
};
use tokio::time::Instant;

use super::operations::Operation;
use super::store::CapabilityStore;
use super::token::random_capability_id;
use super::types::{Audience, Authorization, CapabilityClaims, CapabilityError, UnixSeconds};

pub(crate) use super::store::DENY_AUDIT_INTERVAL;
pub use agent24_store::COMPONENT_RUNTIME_TTL_SECONDS as BRIDGE_TTL_SECONDS;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeIssueError {
    /// Caller is not a live first-party shell for this daemon.
    Rejected,
    /// The decision row was not written, so no token was inserted.
    AuditFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeRevokeError {
    /// Not a dev-bridge component token.
    NotComponent,
    /// The row was not written. The token is already revoked.
    AuditFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngestDeny {
    Audience,
    Component,
    Operation,
}

impl IngestDeny {
    pub(crate) fn reason(self) -> CapabilityDenyReason {
        match self {
            Self::Audience => CapabilityDenyReason::Audience,
            Self::Component => CapabilityDenyReason::Component,
            Self::Operation => CapabilityDenyReason::Operation,
        }
    }
}

pub(crate) fn liveness_deny(err: super::types::CapabilityError) -> Option<CapabilityDenyReason> {
    match err {
        super::types::CapabilityError::Revoked => Some(CapabilityDenyReason::Revoked),
        super::types::CapabilityError::Expired => Some(CapabilityDenyReason::Expired),
        super::types::CapabilityError::StaleGeneration => {
            Some(CapabilityDenyReason::StaleGeneration)
        }
        _ => None,
    }
}

/// Handler gate for `POST /api/v1/inbound/events`. Each arm is independent:
/// a token that fails only one of them must still be refused.
pub fn component_may_ingest(auth: &Authorization) -> Result<(), IngestDeny> {
    if auth.claims.audience != Audience::ComponentRuntime {
        return Err(IngestDeny::Audience);
    }
    if auth.claims.component_id.as_deref() != Some(DEV_BRIDGE_COMPONENT_ID) {
        return Err(IngestDeny::Component);
    }
    if !auth
        .claims
        .allowed_operations
        .contains(&Operation::InboundIngest)
    {
        return Err(IngestDeny::Operation);
    }
    Ok(())
}

fn denial_event(
    auth: &Authorization,
    reason: CapabilityDenyReason,
) -> Option<CapabilityTokenAuditEvent> {
    let operation = AuditRef::new(DEV_BRIDGE_OPERATION).ok()?;
    let component_id = auth
        .claims
        .component_id
        .as_deref()
        .and_then(|id| AuditRef::new(id).ok());
    let capability_id = AuditRef::new(&auth.claims.capability_id).ok();
    Some(CapabilityTokenAuditEvent::Denied {
        reason,
        component_id,
        capability_id,
        operation,
    })
}

/// Append `capability.token.denied` for a component token. The operation
/// field stays `inbound.ingest` (the only profile `validate` accepts). The
/// reason distinguishes route, operation, and liveness. The bearer is not
/// stored. The same id and reason inside [`DENY_AUDIT_INTERVAL`] is skipped.
pub async fn write_component_denial(
    caps: &CapabilityStore,
    db: &Store,
    auth: &Authorization,
    reason: CapabilityDenyReason,
) {
    if auth.claims.audience != Audience::ComponentRuntime {
        return;
    }
    let Some(event) = denial_event(auth, reason) else {
        return;
    };
    if !caps.claim_deny_audit(&auth.claims.capability_id, reason) {
        return;
    }
    if let Ok(ts) = audit_now() {
        let _ = db.append_capability_token_audit(&ts, &event).await;
    }
}

/// Audit first. A write failure leaves the store without this token.
pub async fn issue_wechat_bridge<F, Fut>(
    store: &CapabilityStore,
    authorization: &Authorization,
    now: UnixSeconds,
    audit: F,
) -> Result<super::MintedCapability, BridgeIssueError>
where
    F: FnOnce(CapabilityTokenAuditEvent) -> Fut,
    Fut: Future<Output = Result<(), ()>>,
{
    if authorization.claims.audience != Audience::FirstPartyShell
        || authorization.claims.daemon_generation != store.daemon_generation()
    {
        return Err(BridgeIssueError::Rejected);
    }
    let capability_id = random_capability_id();
    let event = CapabilityTokenAuditEvent::Issued {
        component_id: AuditRef::new(DEV_BRIDGE_COMPONENT_ID)
            .map_err(|_| BridgeIssueError::Rejected)?,
        capability_id: AuditRef::new(&capability_id).map_err(|_| BridgeIssueError::Rejected)?,
        operation: AuditRef::new(DEV_BRIDGE_OPERATION).map_err(|_| BridgeIssueError::Rejected)?,
        ttl_seconds: COMPONENT_RUNTIME_TTL_SECONDS,
    };
    if audit(event).await.is_err() {
        return Err(BridgeIssueError::AuditFailed);
    }
    store
        .insert(CapabilityClaims {
            first_party: None,
            capability_id,
            audience: Audience::ComponentRuntime,
            component_id: Some(DEV_BRIDGE_COMPONENT_ID.to_owned()),
            workspace_id: None,
            attachment_id: None,
            principal_id: None,
            daemon_generation: store.daemon_generation().to_owned(),
            host_generation: None,
            sidecar_generation: None,
            created_at: now,
            expires_at: now.saturating_add(BRIDGE_TTL_SECONDS),
            deadline: Some(Instant::now() + Duration::from_secs(BRIDGE_TTL_SECONDS)),
            allowed_operations: BTreeSet::from([Operation::InboundIngest]),
        })
        .map_err(|_| BridgeIssueError::Rejected)
}

/// Mark the token revoked before the audit write. A write failure must not
/// put the old bearer back into service; the caller retries the row.
pub async fn revoke_wechat_bridge<F, Fut>(
    store: &CapabilityStore,
    capability_id: &str,
    audit: F,
) -> Result<(), BridgeRevokeError>
where
    F: FnOnce(CapabilityTokenAuditEvent) -> Fut,
    Fut: Future<Output = Result<(), ()>>,
{
    let Some(claims) = store.claims_by_id(capability_id) else {
        return Err(BridgeRevokeError::NotComponent);
    };
    if claims.audience != Audience::ComponentRuntime
        || claims.component_id.as_deref() != Some(DEV_BRIDGE_COMPONENT_ID)
    {
        return Err(BridgeRevokeError::NotComponent);
    }
    store.revoke_by_id(capability_id);
    let event = CapabilityTokenAuditEvent::Revoked {
        component_id: AuditRef::new(DEV_BRIDGE_COMPONENT_ID)
            .map_err(|_| BridgeRevokeError::AuditFailed)?,
        capability_id: AuditRef::new(capability_id).map_err(|_| BridgeRevokeError::AuditFailed)?,
        reason: CapabilityRevokeReason::Explicit,
    };
    if audit(event).await.is_err() {
        return Err(BridgeRevokeError::AuditFailed);
    }
    Ok(())
}

pub fn audit_now() -> Result<AuditTimestamp, ()> {
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    AuditTimestamp::new(stamp).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::capabilities::first_party::{FirstPartyAuthority, test_peer};
    use crate::capabilities::{Operation, Resource, unix_now};
    use crate::enroll_api::EnrollAuthority;

    fn shell_store() -> (CapabilityStore, Authorization) {
        let store = CapabilityStore::new("g");
        let issued = FirstPartyAuthority::new(store.clone())
            .issue(&test_peer("g"))
            .unwrap();
        let auth = store
            .validate_bearer(
                &issued.secret,
                Operation::CapabilityMint,
                &Resource::global(),
                unix_now(),
            )
            .unwrap();
        (store, auth)
    }

    #[tokio::test]
    async fn audit_failure_does_not_issue_and_revoke_failure_kills_the_token() {
        let (store, auth) = shell_store();
        let failed = issue_wechat_bridge(&store, &auth, unix_now(), |_| async { Err(()) }).await;
        assert!(matches!(failed, Err(BridgeIssueError::AuditFailed)));
        assert!(store.claims_by_id("unused").is_none());
        assert!(
            store
                .shared
                .state
                .lock()
                .unwrap()
                .records
                .values()
                .all(|record| record.claims.audience != Audience::ComponentRuntime)
        );
        let mut host_auth = auth.clone();
        host_auth.claims.audience = Audience::ProductHost;
        assert!(matches!(
            issue_wechat_bridge(&store, &host_auth, unix_now(), |_| async { Ok(()) }).await,
            Err(BridgeIssueError::Rejected)
        ));

        let minted = issue_wechat_bridge(&store, &auth, unix_now(), |_| async { Ok(()) })
            .await
            .unwrap();
        let (bearer, id, claims) = minted.into_bearer_parts();
        assert_eq!(claims.expires_at - claims.created_at, BRIDGE_TTL_SECONDS);
        assert!(
            store
                .validate_bearer(
                    &bearer,
                    Operation::InboundIngest,
                    &Resource::global(),
                    unix_now()
                )
                .is_ok()
        );
        // Wall-clock `expires_at` is not the gate. `bridge_token_ttl_ignores_wall_clock_jumps`
        // advances the monotonic deadline.
        let stale = claims.expires_at;
        assert!(
            store
                .validate_bearer(
                    &bearer,
                    Operation::InboundIngest,
                    &Resource::global(),
                    stale
                )
                .is_ok()
        );

        let revoke_failed = revoke_wechat_bridge(&store, &id, |_| async { Err(()) }).await;
        assert_eq!(revoke_failed, Err(BridgeRevokeError::AuditFailed));
        assert!(matches!(
            store.validate_bearer(
                &bearer,
                Operation::InboundIngest,
                &Resource::global(),
                unix_now()
            ),
            Err(CapabilityError::Revoked)
        ));
    }

    #[test]
    fn each_ingest_check_fails_on_its_own() {
        let (store, auth) = shell_store();
        let now = unix_now();
        let mut claims = auth.claims.clone();
        claims.audience = Audience::ComponentRuntime;
        claims.component_id = Some("other-bridge".into());
        claims.allowed_operations = BTreeSet::from([Operation::InboundIngest]);
        claims.capability_id = "other".into();
        let other = Authorization {
            claims: claims.clone(),
            epoch: 0,
        };
        assert_eq!(component_may_ingest(&other), Err(IngestDeny::Component));
        claims.component_id = Some(DEV_BRIDGE_COMPONENT_ID.into());
        claims.audience = Audience::FirstPartyShell;
        assert_eq!(
            component_may_ingest(&Authorization {
                claims: claims.clone(),
                epoch: 0
            }),
            Err(IngestDeny::Audience)
        );
        claims.audience = Audience::ComponentRuntime;
        claims.allowed_operations.clear();
        assert_eq!(
            component_may_ingest(&Authorization { claims, epoch: 0 }),
            Err(IngestDeny::Operation)
        );
        let _ = (store, now);
    }

    /// A jump of the wall clock past `expires_at` leaves the bridge token
    /// live. Advancing the runtime clock by the TTL expires it.
    /// Clearing `deadline` in `issue_wechat_bridge` turns the first `is_ok` red.
    #[tokio::test(start_paused = true)]
    async fn bridge_token_ttl_ignores_wall_clock_jumps() {
        let (store, auth) = shell_store();
        let minted = issue_wechat_bridge(&store, &auth, unix_now(), |_| async { Ok(()) })
            .await
            .unwrap();
        let (bearer, _, claims) = minted.into_bearer_parts();
        assert!(claims.deadline.is_some());
        let jumped = unix_now() + BRIDGE_TTL_SECONDS * 100;
        assert!(store.require_live_bearer(&bearer, jumped).is_ok());
        assert!(
            store
                .validate_bearer(
                    &bearer,
                    Operation::InboundIngest,
                    &Resource::global(),
                    jumped
                )
                .is_ok()
        );
        tokio::time::advance(Duration::from_secs(BRIDGE_TTL_SECONDS)).await;
        assert!(store.require_live_bearer(&bearer, 0).is_err());
        assert!(
            store
                .validate_bearer(&bearer, Operation::InboundIngest, &Resource::global(), 0)
                .is_err()
        );
    }

    /// Same id and reason is one slot. A different reason, a different id,
    /// or the interval elapsing opens another. Dropping the retain-and-skip
    /// in `claim_deny_audit` turns the second assertion red.
    #[tokio::test(start_paused = true)]
    async fn deny_audit_for_one_token_and_reason_is_throttled() {
        use agent24_store::CapabilityDenyReason;

        let store = CapabilityStore::new("g");
        assert!(store.claim_deny_audit("gate-models", CapabilityDenyReason::Route));
        assert!(!store.claim_deny_audit("gate-models", CapabilityDenyReason::Route));
        assert!(store.claim_deny_audit("gate-models", CapabilityDenyReason::Operation));
        assert!(store.claim_deny_audit("gate-module", CapabilityDenyReason::Route));
        tokio::time::advance(DENY_AUDIT_INTERVAL).await;
        assert!(store.claim_deny_audit("gate-models", CapabilityDenyReason::Route));
    }
}
