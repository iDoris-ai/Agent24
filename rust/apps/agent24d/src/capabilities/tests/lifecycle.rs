use super::super::*;
use super::{creative, host};
use std::time::Duration;

#[test]
fn host_revoke_and_mint_have_a_single_linearization_order() {
    let revoked_first = CapabilityStore::new("daemon-1");
    let revoked_host = host(&revoked_first);
    let authorization = revoked_first
        .validate(
            revoked_host.token(),
            Operation::CapabilityMint,
            &Resource::global(),
            101,
        )
        .unwrap();
    assert!(revoked_first.revoke_by_id(revoked_host.id()));
    assert!(
        revoked_first
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
            .is_err()
    );

    let minted_first = CapabilityStore::new("daemon-1");
    let host = host(&minted_first);
    let authorization = minted_first
        .validate(
            host.token(),
            Operation::CapabilityMint,
            &Resource::global(),
            101,
        )
        .unwrap();
    let child = minted_first
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
    assert_eq!(minted_first.revoke_by_host_generation("host-1"), 2);
    assert_eq!(
        minted_first.validate(
            child.token(),
            Operation::RunRead,
            &Resource::principal("ws-a", "att-a", "principal-a"),
            101,
        ),
        Err(CapabilityError::Revoked)
    );
}

#[test]
fn expiry_is_enforced() {
    let store = CapabilityStore::new("daemon-1");
    let minted = store
        .mint_product_host("host-1", Duration::from_secs(5), 100)
        .unwrap();
    assert!(
        store
            .validate(minted.token(), Operation::RunRead, &Resource::global(), 104)
            .is_ok()
    );
    assert_eq!(
        store.validate(minted.token(), Operation::RunRead, &Resource::global(), 105),
        Err(CapabilityError::Expired)
    );
}

#[test]
fn revoke_changes_epoch_and_stream_revalidation_fails() {
    let store = CapabilityStore::new("daemon-1");
    let minted = host(&store);
    let before = store.epoch();
    assert!(
        store
            .validate(minted.token(), Operation::RunRead, &Resource::global(), 101)
            .is_ok()
    );
    assert!(store.revoke(minted.token()));
    assert!(store.epoch() > before);
    assert_eq!(
        store.revalidate(minted.token(), Operation::RunRead, &Resource::global(), 101),
        Err(CapabilityError::Revoked)
    );
}

#[test]
fn generation_revoke_covers_sidecars() {
    let store = CapabilityStore::new("daemon-1");
    let host = host(&store);
    let creative = creative(&store, &host);
    assert_eq!(store.revoke_by_sidecar_generation("sidecar-1"), 1);
    assert_eq!(
        store.validate(
            creative.token(),
            Operation::RunRead,
            &Resource::principal("ws-a", "att-a", "principal-a"),
            101,
        ),
        Err(CapabilityError::Revoked)
    );
}

#[test]
fn token_rotation_preserves_durable_principal_but_changes_token() {
    let store = CapabilityStore::new("daemon-1");
    let host = host(&store);
    let first = creative(&store, &host);
    let second = creative(&store, &host);
    assert!(first.token() != second.token());
    assert_eq!(first.claims().principal_id, second.claims().principal_id);
    assert!(
        store
            .validate(
                second.token(),
                Operation::SessionRead,
                &Resource::session("ws-a", "att-a", "principal-a", "new-sess"),
                101,
            )
            .is_ok()
    );
}
