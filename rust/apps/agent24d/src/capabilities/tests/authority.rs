use super::super::*;
use super::{creative, host};
use std::time::Duration;

#[test]
fn host_has_full_authority() {
    let store = CapabilityStore::new("daemon-1");
    let minted = host(&store);
    assert!(
        store
            .validate(
                minted.token(),
                Operation::ApprovalDecision,
                &Resource::global(),
                101
            )
            .is_ok()
    );
}

#[test]
fn authorized_host_snapshot_can_mint_without_bearer_extension() {
    let store = CapabilityStore::new("daemon-1");
    let host = host(&store);
    let authorization = store
        .validate(
            host.token(),
            Operation::CapabilityMint,
            &Resource::global(),
            101,
        )
        .unwrap();
    let creative = store
        .mint_creative_authorized(
            &authorization,
            CreativeMintRequest::new(
                "ws-a",
                "att-a",
                "principal-a",
                "sidecar-1",
                Duration::from_secs(100),
                101,
            ),
        )
        .unwrap();
    assert_eq!(creative.claims().audience, Audience::CreativeRuntime);
}

#[test]
fn every_issuer_obeys_the_creative_ttl_ceiling() {
    let store = CapabilityStore::new("daemon-1");
    let host = host(&store);
    let authorization = store
        .validate(
            host.token(),
            Operation::CapabilityMint,
            &Resource::global(),
            101,
        )
        .unwrap();
    for ttl in [0, MAX_CREATIVE_TTL_SECONDS + 1] {
        assert!(matches!(
            store.mint_creative_authorized(
                &authorization,
                CreativeMintRequest::new(
                    "ws-a",
                    "att-a",
                    "principal-a",
                    "sidecar-1",
                    Duration::from_secs(ttl),
                    101,
                ),
            ),
            Err(CapabilityError::InvalidTtl)
        ));
    }
}

#[test]
fn creative_allowlist_and_default_deny() {
    let store = CapabilityStore::new("daemon-1");
    let host = host(&store);
    let creative = creative(&store, &host);
    let resource = Resource::session("ws-a", "att-a", "principal-a", "sess-a");
    assert!(
        store
            .validate(creative.token(), Operation::RunRead, &resource, 101)
            .is_ok()
    );
    assert_eq!(
        store.validate_action(creative.token(), "approval.decision", &resource, 101),
        Err(CapabilityError::OperationDenied)
    );
}

#[test]
fn models_are_global_but_runs_require_principal_scope() {
    let store = CapabilityStore::new("daemon-1");
    let host = host(&store);
    let creative = creative(&store, &host);
    assert!(
        store
            .validate(
                creative.token(),
                Operation::ModelsRead,
                &Resource::global(),
                101
            )
            .is_ok()
    );
    assert_eq!(
        store.validate(
            creative.token(),
            Operation::RunRead,
            &Resource::global(),
            101
        ),
        Err(CapabilityError::ResourceDenied)
    );
}

#[test]
fn workspace_and_principal_are_exactly_scoped() {
    let store = CapabilityStore::new("daemon-1");
    let host = host(&store);
    let creative = creative(&store, &host);
    assert_eq!(
        store.validate(
            creative.token(),
            Operation::RunRead,
            &Resource::session("ws-b", "att-a", "principal-a", "sess-a"),
            101,
        ),
        Err(CapabilityError::ResourceDenied)
    );
    assert_eq!(
        store.validate(
            creative.token(),
            Operation::RunRead,
            &Resource::session("ws-a", "att-a", "principal-b", "sess-a"),
            101,
        ),
        Err(CapabilityError::ResourceDenied)
    );
}
