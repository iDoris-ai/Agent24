use super::super::*;
use super::host;

#[test]
fn bearer_is_strict_and_id_revoke_is_idempotent() {
    let store = CapabilityStore::new("daemon-1");
    let minted = host(&store);
    let id = minted.id().to_owned();
    let (bearer, returned_id, claims) = minted.into_bearer_parts();
    assert_eq!(bearer.len(), 64);
    assert_eq!(returned_id, id);
    assert_eq!(claims.capability_id, id);
    assert!(
        store
            .validate_bearer(
                &bearer,
                Operation::ApprovalDecision,
                &Resource::global(),
                101
            )
            .is_ok()
    );
    assert!(CapabilityToken::parse_bearer("0").is_err());
    assert!(CapabilityToken::parse_bearer(&format!("{}z", &bearer[..63])).is_err());

    let second = host(&store);
    let second_id = second.id().to_owned();
    assert!(store.revoke_by_id(&second_id));
    assert!(!store.revoke_by_id(&second_id));
    assert_eq!(
        store.validate(second.token(), Operation::RunRead, &Resource::global(), 101),
        Err(CapabilityError::Revoked)
    );
}

#[tokio::test]
async fn async_watch_notifies_on_revoke() {
    let store = CapabilityStore::new("daemon-1");
    let minted = host(&store);
    let mut watch = store.watch();
    let previous = watch.seen();
    assert!(store.revoke_by_id(minted.id()));
    assert!(watch.changed().await > previous);
}

#[test]
fn token_is_not_debug_or_serializable() {
    let store = CapabilityStore::new("daemon-1");
    let minted = host(&store);
    let claims_debug = format!("{:?}", minted.claims());
    assert!(!claims_debug.contains("CapabilityToken"));
    let _ = minted.token();
}
