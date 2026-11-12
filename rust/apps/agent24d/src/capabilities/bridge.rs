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
    /// The caller was revoked, expired, or rotated before the insert.
    CallerInvalid,
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
    /// `Audience` has no audit arm. The denial writer ignores a
    /// non-component bearer, and a component bearer never takes that arm.
    pub(crate) fn audit_reason(self) -> Option<CapabilityDenyReason> {
        match self {
            Self::Audience => None,
            Self::Component => Some(CapabilityDenyReason::Component),
            Self::Operation => Some(CapabilityDenyReason::Operation),
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
    let db = db.clone();
    write_component_denial_with(caps, auth, reason, move |event| {
        let db = db.clone();
        async move {
            let ts = audit_now()?;
            db.append_capability_token_audit(&ts, &event)
                .await
                .map(|_| ())
                .map_err(|_| ())
        }
    })
    .await;
}

async fn write_component_denial_with<F, Fut>(
    caps: &CapabilityStore,
    auth: &Authorization,
    reason: CapabilityDenyReason,
    write: F,
) where
    F: FnOnce(CapabilityTokenAuditEvent) -> Fut,
    Fut: Future<Output = Result<(), ()>>,
{
    if auth.claims.audience != Audience::ComponentRuntime {
        return;
    }
    let Some(event) = denial_event(auth, reason) else {
        return;
    };
    let Some(claimed_at) = caps.claim_deny_audit(&auth.claims.capability_id, reason) else {
        return;
    };
    if write(event).await.is_err() {
        caps.release_deny_audit(&auth.claims.capability_id, reason, claimed_at);
        tracing::warn!(
            capability_id = %auth.claims.capability_id,
            ?reason,
            "capability denial audit was not written; throttle slot released"
        );
    }
}

/// Audit first. A write failure leaves the store without this token.
/// After that write, the caller is checked again and the new token is
/// inserted under the same store lock. A caller revoked, expired, rotated,
/// or whose shell process has exited during the audit is not issued.
/// That path appends `capability.token.issue_aborted` for the same id. If
/// the compensating row cannot be written the result is [`BridgeIssueError::AuditFailed`].
pub async fn issue_wechat_bridge<F, Fut>(
    store: &CapabilityStore,
    authorization: &Authorization,
    now: UnixSeconds,
    audit: F,
) -> Result<super::MintedCapability, BridgeIssueError>
where
    F: Fn(CapabilityTokenAuditEvent) -> Fut,
    Fut: Future<Output = Result<(), ()>>,
{
    if authorization.claims.audience != Audience::FirstPartyShell
        || authorization.claims.daemon_generation != store.daemon_generation()
    {
        return Err(BridgeIssueError::Rejected);
    }
    let capability_id = random_capability_id();
    let component_id =
        AuditRef::new(DEV_BRIDGE_COMPONENT_ID).map_err(|_| BridgeIssueError::Rejected)?;
    let operation = AuditRef::new(DEV_BRIDGE_OPERATION).map_err(|_| BridgeIssueError::Rejected)?;
    let capability_ref = AuditRef::new(&capability_id).map_err(|_| BridgeIssueError::Rejected)?;
    let issuer_capability_id = AuditRef::new(&authorization.claims.capability_id)
        .map_err(|_| BridgeIssueError::Rejected)?;
    let event = CapabilityTokenAuditEvent::Issued {
        component_id: component_id.clone(),
        capability_id: capability_ref.clone(),
        operation: operation.clone(),
        ttl_seconds: COMPONENT_RUNTIME_TTL_SECONDS,
        issuer_capability_id: issuer_capability_id.clone(),
    };
    if audit(event).await.is_err() {
        return Err(BridgeIssueError::AuditFailed);
    }
    match insert_for_live_caller(
        store,
        authorization,
        now,
        CapabilityClaims {
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
        },
    ) {
        Err(BridgeIssueError::CallerInvalid) => {
            // The issued row names an id that was not inserted. Close it.
            let aborted = CapabilityTokenAuditEvent::IssueAborted {
                component_id,
                capability_id: capability_ref,
                operation,
                issuer_capability_id,
            };
            if audit(aborted).await.is_err() {
                return Err(BridgeIssueError::AuditFailed);
            }
            Err(BridgeIssueError::CallerInvalid)
        }
        other => other,
    }
}

/// Caller liveness and the insert share one lock, so a revoke or a dead
/// shell that lands during the audit cannot slip a token in afterwards.
fn insert_for_live_caller(
    store: &CapabilityStore,
    authorization: &Authorization,
    now: UnixSeconds,
    claims: CapabilityClaims,
) -> Result<super::MintedCapability, BridgeIssueError> {
    let mut state = store
        .shared
        .state
        .lock()
        .map_err(|_| BridgeIssueError::CallerInvalid)?;
    let live = state
        .ids
        .get(&authorization.claims.capability_id)
        .copied()
        .and_then(|digest| state.records.get(&digest))
        .is_some_and(|record| {
            super::validate::process_live(record)
                && !record.revoked
                && record.claims.audience == Audience::FirstPartyShell
                && record.claims.daemon_generation == store.daemon_generation()
                && !record.expired(now)
        });
    if !live {
        return Err(BridgeIssueError::CallerInvalid);
    }
    store
        .insert_locked(&mut state, claims)
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
    // Already revoked, missing, or a poisoned lock: no second Revoked row.
    // A retry after `AuditFailed` therefore does not backfill the row.
    if !store.revoke_by_id(capability_id) {
        return Ok(());
    }
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
        // Still inside `expires_at`. A jump past it is
        // `bridge_token_ttl_ignores_wall_clock_jumps`.
        let inside = claims.expires_at - 1;
        assert!(
            store
                .validate_bearer(
                    &bearer,
                    Operation::InboundIngest,
                    &Resource::global(),
                    inside
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

    /// A wall-clock jump past `expires_at` expires the bridge token. A
    /// backward jump does not: the monotonic deadline still fires on time.
    /// Dropping the `expires_at` arm in `Record::expired` turns the forward
    /// jump red. Shell tokens do not use this arm.
    #[tokio::test(start_paused = true)]
    async fn bridge_token_ttl_ignores_wall_clock_jumps() {
        let (store, auth) = shell_store();
        let minted = issue_wechat_bridge(&store, &auth, unix_now(), |_| async { Ok(()) })
            .await
            .unwrap();
        let (bearer, _, claims) = minted.into_bearer_parts();
        assert!(claims.deadline.is_some());
        let jumped = unix_now() + BRIDGE_TTL_SECONDS * 100;
        assert!(store.require_live_bearer(&bearer, jumped).is_err());
        assert!(
            store
                .validate_bearer(
                    &bearer,
                    Operation::InboundIngest,
                    &Resource::global(),
                    jumped
                )
                .is_err()
        );
        assert!(store.require_live_bearer(&bearer, 0).is_ok());
        tokio::time::advance(Duration::from_secs(BRIDGE_TTL_SECONDS)).await;
        assert!(store.require_live_bearer(&bearer, 0).is_err());
        assert!(
            store
                .validate_bearer(&bearer, Operation::InboundIngest, &Resource::global(), 0)
                .is_err()
        );
    }

    /// Revoking the caller while the audit future is in flight does not
    /// insert a bridge token. Skipping the live check in
    /// `insert_for_live_caller` turns this `CallerInvalid` into `Ok`.
    #[tokio::test]
    async fn revoked_caller_during_audit_is_not_issued() {
        let (store, auth) = shell_store();
        let caller_id = auth.claims.capability_id.clone();
        let during = store.clone();
        let failed = issue_wechat_bridge(&store, &auth, unix_now(), move |_| {
            let during = during.clone();
            let caller_id = caller_id.clone();
            async move {
                during.revoke_by_id(&caller_id);
                Ok(())
            }
        })
        .await;
        assert!(matches!(failed, Err(BridgeIssueError::CallerInvalid)));
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
    }

    /// The issued row is closed with `issue_aborted`. The id is not inserted.
    /// Dropping the `IssueAborted` write turns the action assertion red.
    /// Returning `CallerInvalid` when that write fails turns the second
    /// assertion red.
    #[tokio::test]
    async fn recheck_failure_appends_issue_aborted_and_inserts_nothing() {
        use agent24_store::Store;

        let (store, auth) = shell_store();
        let db = Store::open_memory().await.unwrap();
        let caller_id = auth.claims.capability_id.clone();
        let issuer = caller_id.clone();
        let during = store.clone();
        let db_audit = db.clone();
        let failed = issue_wechat_bridge(&store, &auth, unix_now(), move |event| {
            let during = during.clone();
            let caller_id = caller_id.clone();
            let db_audit = db_audit.clone();
            async move {
                if matches!(event, CapabilityTokenAuditEvent::Issued { .. }) {
                    during.revoke_by_id(&caller_id);
                }
                let ts = audit_now().unwrap();
                db_audit
                    .append_capability_token_audit(&ts, &event)
                    .await
                    .map(|_| ())
                    .map_err(|_| ())
            }
        })
        .await;
        assert!(matches!(failed, Err(BridgeIssueError::CallerInvalid)));
        let entries = db.list_audit().await.unwrap();
        let actions: Vec<_> = entries.iter().map(|entry| entry.action.as_str()).collect();
        assert_eq!(
            actions,
            ["capability.token.issued", "capability.token.issue_aborted"]
        );
        let issued_id = entries[0].detail["issued"]["capability_id"]
            .as_str()
            .unwrap();
        assert_eq!(
            entries[1].detail["issue_aborted"]["capability_id"].as_str(),
            Some(issued_id)
        );
        assert_eq!(
            entries[0].detail["issued"]["issuer_capability_id"].as_str(),
            Some(issuer.as_str())
        );
        assert_eq!(
            entries[1].detail["issue_aborted"]["issuer_capability_id"].as_str(),
            Some(issuer.as_str())
        );
        assert!(store.claims_by_id(issued_id).is_none());

        let (store, auth) = shell_store();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let during = store.clone();
        let caller_id = auth.claims.capability_id.clone();
        let failed = issue_wechat_bridge(&store, &auth, unix_now(), move |event| {
            let during = during.clone();
            let caller_id = caller_id.clone();
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                if matches!(event, CapabilityTokenAuditEvent::Issued { .. }) {
                    during.revoke_by_id(&caller_id);
                }
                if n == 0 { Ok(()) } else { Err(()) }
            }
        })
        .await;
        assert!(matches!(failed, Err(BridgeIssueError::AuditFailed)));
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
    }

    /// A shell whose process is already dead is not issued. Bumping the
    /// recorded start time is the same stand-in `process_live` uses.
    /// Dropping `process_live` in `insert_for_live_caller` turns this
    /// `CallerInvalid` into `Ok`.
    #[tokio::test]
    async fn dead_shell_during_audit_is_not_issued() {
        let (store, auth) = shell_store();
        let caller_id = auth.claims.capability_id.clone();
        let during = store.clone();
        let failed = issue_wechat_bridge(&store, &auth, unix_now(), move |_| {
            let during = during.clone();
            let caller_id = caller_id.clone();
            async move {
                let mut state = during.shared.state.lock().unwrap();
                let digest = *state.ids.get(&caller_id).unwrap();
                state
                    .records
                    .get_mut(&digest)
                    .unwrap()
                    .claims
                    .first_party
                    .as_mut()
                    .unwrap()
                    .peer
                    .start
                    .sec += 1;
                Ok(())
            }
        })
        .await;
        assert!(matches!(failed, Err(BridgeIssueError::CallerInvalid)));
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
    }

    fn component_auth(id: &str) -> Authorization {
        let now = unix_now();
        Authorization {
            claims: CapabilityClaims {
                first_party: None,
                capability_id: id.to_owned(),
                audience: Audience::ComponentRuntime,
                component_id: Some(DEV_BRIDGE_COMPONENT_ID.to_owned()),
                workspace_id: None,
                attachment_id: None,
                principal_id: None,
                daemon_generation: "g".into(),
                host_generation: None,
                sidecar_generation: None,
                created_at: now,
                expires_at: now + BRIDGE_TTL_SECONDS,
                deadline: Some(Instant::now() + Duration::from_secs(BRIDGE_TTL_SECONDS)),
                allowed_operations: BTreeSet::from([Operation::InboundIngest]),
            },
            epoch: 0,
        }
    }

    /// A failed denial write releases its own throttle slot, so the next
    /// refusal of the same id and reason is written. Dropping
    /// `release_deny_audit` leaves the second `list_audit` empty.
    /// Releasing without comparing timestamps clears a newer claim.
    #[tokio::test]
    async fn deny_audit_write_failure_releases_the_slot() {
        use agent24_store::{CapabilityDenyReason, Store};

        let store = CapabilityStore::new("g");
        let db = Store::open_memory().await.unwrap();
        let auth = component_auth("slot-a");
        write_component_denial_with(&store, &auth, CapabilityDenyReason::Revoked, |_| async {
            Err(())
        })
        .await;
        assert!(db.list_audit().await.unwrap().is_empty());

        let claimed = store
            .claim_deny_audit("other-slot", CapabilityDenyReason::Revoked)
            .unwrap();
        {
            let mut slots = store.shared.deny_audit_at.lock().unwrap();
            slots.insert(
                ("other-slot".to_owned(), CapabilityDenyReason::Revoked),
                claimed + Duration::from_millis(5),
            );
        }
        store.release_deny_audit("other-slot", CapabilityDenyReason::Revoked, claimed);
        assert!(
            store
                .claim_deny_audit("other-slot", CapabilityDenyReason::Revoked)
                .is_none()
        );

        write_component_denial(&store, &db, &auth, CapabilityDenyReason::Revoked).await;
        let denied = db
            .list_audit()
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.action == "capability.token.denied")
            .count();
        assert_eq!(denied, 1);
    }

    /// Same id and reason is one slot. A different reason, a different id,
    /// or the interval elapsing opens another. Dropping the retain-and-skip
    /// in `claim_deny_audit` turns the second assertion red.
    #[tokio::test(start_paused = true)]
    async fn deny_audit_for_one_token_and_reason_is_throttled() {
        use agent24_store::CapabilityDenyReason;

        let store = CapabilityStore::new("g");
        assert!(
            store
                .claim_deny_audit("gate-models", CapabilityDenyReason::Route)
                .is_some()
        );
        assert!(
            store
                .claim_deny_audit("gate-models", CapabilityDenyReason::Route)
                .is_none()
        );
        assert!(
            store
                .claim_deny_audit("gate-models", CapabilityDenyReason::Operation)
                .is_some()
        );
        assert!(
            store
                .claim_deny_audit("gate-module", CapabilityDenyReason::Route)
                .is_some()
        );
        tokio::time::advance(DENY_AUDIT_INTERVAL).await;
        assert!(
            store
                .claim_deny_audit("gate-models", CapabilityDenyReason::Route)
                .is_some()
        );
    }

    /// A poisoned deny-audit lock skips the write. Returning `Some` from the
    /// `Err` arm of `claim_deny_audit` turns this red.
    #[test]
    fn poisoned_deny_audit_lock_does_not_claim_a_write() {
        use agent24_store::CapabilityDenyReason;

        let store = CapabilityStore::new("g");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = store.shared.deny_audit_at.lock().unwrap();
            panic!("poison the deny-audit lock");
        }));
        assert!(
            store
                .claim_deny_audit("gate-models", CapabilityDenyReason::Route)
                .is_none()
        );
    }

    /// The second revoke does not append another `Revoked` row. Ignoring
    /// `revoke_by_id`'s bool and always auditing turns the count into 2.
    #[tokio::test]
    async fn revoked_row_is_written_only_when_revoke_by_id_returns_true() {
        use agent24_store::Store;

        let (store, auth) = shell_store();
        let db = Store::open_memory().await.unwrap();
        let record = db.clone();
        let minted = issue_wechat_bridge(&store, &auth, unix_now(), move |event| {
            let record = record.clone();
            async move {
                let ts = audit_now().unwrap();
                record
                    .append_capability_token_audit(&ts, &event)
                    .await
                    .map(|_| ())
                    .map_err(|_| ())
            }
        })
        .await
        .unwrap();
        let (_, id, _) = minted.into_bearer_parts();
        for _ in 0..2 {
            let record = db.clone();
            revoke_wechat_bridge(&store, &id, move |event| {
                let record = record.clone();
                async move {
                    let ts = audit_now().unwrap();
                    record
                        .append_capability_token_audit(&ts, &event)
                        .await
                        .map(|_| ())
                        .map_err(|_| ())
                }
            })
            .await
            .unwrap();
        }
        let revoked = db
            .list_audit()
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.action == "capability.token.revoked")
            .count();
        assert_eq!(revoked, 1);
    }

    /// Revoking the shell record does not revoke the bridge token it issued.
    /// Cascading `revoke` onto component tokens turns the bearer check red.
    #[tokio::test]
    async fn revoking_the_shell_leaves_its_bridge_token_live() {
        let store = CapabilityStore::new("g");
        let authority = FirstPartyAuthority::new(store.clone());
        let peer = test_peer("g");
        let issued = authority.issue(&peer).unwrap();
        let auth = store
            .validate_bearer(
                &issued.secret,
                Operation::CapabilityMint,
                &Resource::global(),
                unix_now(),
            )
            .unwrap();
        let minted = issue_wechat_bridge(&store, &auth, unix_now(), |_| async { Ok(()) })
            .await
            .unwrap();
        let (bearer, _, _) = minted.into_bearer_parts();
        authority.revoke(&peer, &issued.token_id);
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
    }
}
