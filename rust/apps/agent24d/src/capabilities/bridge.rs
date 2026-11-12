//! Dev-bridge `ComponentRuntime` tier. One hardcoded profile: `wechat-bridge`
//! may call `inbound.ingest` for eight hours. The daemon does not mint a
//! component token when a component is enabled.
//!
//! Issuance and revocation audit land in the following commit. This commit
//! only admits the token.

use std::collections::BTreeSet;

use super::operations::Operation;
use super::store::CapabilityStore;
use super::token::random_capability_id;
use super::types::{Audience, Authorization, CapabilityClaims, UnixSeconds};

pub const BRIDGE_TTL_SECONDS: u64 = 8 * 60 * 60;
const DEV_BRIDGE_COMPONENT_ID: &str = "wechat-bridge";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeIssueError {
    /// Caller is not a live first-party shell for this daemon.
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeRevokeError {
    /// Not a dev-bridge component token.
    NotComponent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngestDeny {
    Audience,
    Component,
    Operation,
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

/// Mint the only component-runtime tier. Eight hours, this daemon generation,
/// no renewal path.
pub fn issue_wechat_bridge(
    store: &CapabilityStore,
    authorization: &Authorization,
    now: UnixSeconds,
) -> Result<super::MintedCapability, BridgeIssueError> {
    if authorization.claims.audience != Audience::FirstPartyShell
        || authorization.claims.daemon_generation != store.daemon_generation()
    {
        return Err(BridgeIssueError::Rejected);
    }
    store
        .insert(CapabilityClaims {
            first_party: None,
            capability_id: random_capability_id(),
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
            allowed_operations: BTreeSet::from([Operation::InboundIngest]),
        })
        .map_err(|_| BridgeIssueError::Rejected)
}

/// Revoke one dev-bridge token. A component token outside that profile is
/// [`BridgeRevokeError::NotComponent`]; the caller still drops it in memory.
pub fn revoke_wechat_bridge(
    store: &CapabilityStore,
    capability_id: &str,
) -> Result<(), BridgeRevokeError> {
    let Some(claims) = store.claims_by_id(capability_id) else {
        return Err(BridgeRevokeError::NotComponent);
    };
    if claims.audience != Audience::ComponentRuntime
        || claims.component_id.as_deref() != Some(DEV_BRIDGE_COMPONENT_ID)
    {
        return Err(BridgeRevokeError::NotComponent);
    }
    store.revoke_by_id(capability_id);
    Ok(())
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

    #[test]
    fn shell_issues_an_eight_hour_bridge_token_and_a_host_cannot() {
        let (store, auth) = shell_store();
        let mut host_auth = auth.clone();
        host_auth.claims.audience = Audience::ProductHost;
        assert!(matches!(
            issue_wechat_bridge(&store, &host_auth, unix_now()),
            Err(BridgeIssueError::Rejected)
        ));
        let minted = issue_wechat_bridge(&store, &auth, unix_now()).unwrap();
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
        let stale = claims.expires_at;
        assert!(
            store
                .validate_bearer(
                    &bearer,
                    Operation::InboundIngest,
                    &Resource::global(),
                    stale
                )
                .is_err()
        );
        assert!(revoke_wechat_bridge(&store, &id).is_ok());
        assert!(
            store
                .validate_bearer(
                    &bearer,
                    Operation::InboundIngest,
                    &Resource::global(),
                    unix_now()
                )
                .is_err()
        );
    }

    #[test]
    fn each_ingest_check_fails_on_its_own() {
        let (_store, auth) = shell_store();
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
    }
}
