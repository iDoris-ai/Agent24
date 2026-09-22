#![allow(clippy::expect_used, clippy::unwrap_used)]
use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError, WorkspaceTtl, test_hooks,
};
use sqlx::Row;

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        TrustedRootRegistration::new(
            "/renew/adversarial".into(),
            "generation-renew".into(),
            RootIdentity::unix(&[31; 8], &[32; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
        LifecycleOwnerRef::parse("owner".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}
async fn create(store: &Store) {
    store
        .create_workspace(
            &input(),
            &LifecycleOwnerRef::parse("owner".into()).unwrap(),
            &WorkspaceInstant::parse(CREATED).unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn renewal_owner_precedes_expiry_and_overflow_and_legacy_is_rejected() {
    let s = Store::open_memory().await.unwrap();
    create(&s).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let ttl = WorkspaceTtl::new(60_000).unwrap();
    let wrong = LifecycleOwnerRef::parse("wrong".into()).unwrap();
    let expired_now = WorkspaceInstant::parse("2026-09-19T00:02:00.000Z").unwrap();
    assert_eq!(
        s.renew_workspace(&id, &wrong, ttl, &expired_now).await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "lifecycle_owner_ref"
        })
    );
    let overflow = WorkspaceInstant::parse("9999-12-31T23:59:59.999Z").unwrap();
    assert_eq!(
        s.renew_workspace(&id, &wrong, ttl, &overflow).await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "lifecycle_owner_ref"
        })
    );
    sqlx::query("UPDATE workspaces SET kind='legacy_compat' WHERE id=?")
        .bind(ID)
        .execute(test_hooks::pool(&s))
        .await
        .unwrap();
    assert_eq!(
        s.renew_workspace(
            &id,
            &LifecycleOwnerRef::parse("owner".into()).unwrap(),
            ttl,
            &expired_now
        )
        .await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "workspace_kind"
        })
    );
    assert!(s.list_audit().await.unwrap().is_empty());
}

#[tokio::test]
async fn audit_or_valid_reselect_tamper_rolls_back_and_leases_remain_untouched() {
    let s = Store::open_memory().await.unwrap();
    create(&s).await;
    sqlx::query("INSERT INTO workspace_leases(lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6',?,'generation-renew','run-owner','run',?)").bind(ID).bind(CREATED).execute(test_hooks::pool(&s)).await.unwrap();
    sqlx::query("CREATE TRIGGER tamper_renew AFTER UPDATE OF renewed_at ON workspaces BEGIN UPDATE workspaces SET provenance_source='other' WHERE id=NEW.id; END").execute(test_hooks::pool(&s)).await.unwrap();
    let now = WorkspaceInstant::parse("2026-09-19T00:00:30.000Z").unwrap();
    let id = WorkspaceId::parse(ID).unwrap();
    let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
    let ttl = WorkspaceTtl::new(120_000).unwrap();
    assert_eq!(
        s.renew_workspace(&id, &owner, ttl, &now).await,
        Err(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "row"
        })
    );
    sqlx::query("DROP TRIGGER tamper_renew")
        .execute(test_hooks::pool(&s))
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER deny_renew_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT,'secret trigger'); END").execute(test_hooks::pool(&s)).await.unwrap();
    assert_eq!(
        s.renew_workspace(&id, &owner, ttl, &now).await,
        Err(WorkspaceStoreError::Database)
    );
    let row = sqlx::query(
        "SELECT state,expires_at,renewed_at,revision,provenance_source FROM workspaces WHERE id=?",
    )
    .bind(ID)
    .fetch_one(test_hooks::pool(&s))
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("state"), "active");
    assert_eq!(
        row.get::<String, _>("expires_at"),
        "2026-09-19T00:01:00.000Z"
    );
    assert_eq!(row.get::<Option<String>, _>("renewed_at"), None);
    assert_eq!(row.get::<i64, _>("revision"), 1);
    assert_eq!(row.get::<String, _>("provenance_source"), "git");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workspace_leases")
            .fetch_one(test_hooks::pool(&s))
            .await
            .unwrap(),
        1
    );
    assert!(s.list_audit().await.unwrap().is_empty());
}

#[tokio::test]
async fn wal_expiry_race_never_resurrects_or_loses_audit() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("renew-race.sqlite"))
        .await
        .unwrap();
    create(&s).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
    let ttl = WorkspaceTtl::new(120_000).unwrap();
    let renew_now = WorkspaceInstant::parse("2026-09-19T00:00:30.000Z").unwrap();
    let expire_now = WorkspaceInstant::parse("2026-09-19T00:01:00.000Z").unwrap();
    let (renew, expire) = tokio::join!(
        s.renew_workspace(&id, &owner, ttl, &renew_now),
        s.expire_workspace(&id, &expire_now)
    );
    assert!(renew.is_ok() || expire.is_ok());
    let final_row = s.get_workspace(&id).await.unwrap();
    assert!(matches!(final_row.state.as_str(), "active" | "expired"));
    if final_row.state == "active" {
        assert_eq!(final_row.revision, 2);
        assert_eq!(final_row.expires_at, "2026-09-19T00:02:30.000Z");
    } else {
        assert_eq!(final_row.revision, 2);
        assert_eq!(final_row.expires_at, "2026-09-19T00:01:00.000Z");
    }
    let audit = s.list_audit().await.unwrap();
    assert_eq!(audit.len(), 1);
    assert!(matches!(
        audit[0].action.as_str(),
        "workspace.renewed" | "workspace.expired"
    ));
    s.verify_audit_chain().await.unwrap();
}
