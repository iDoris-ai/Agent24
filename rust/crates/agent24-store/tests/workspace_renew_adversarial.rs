#![allow(clippy::expect_used, clippy::unwrap_used)]
use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError, WorkspaceTtl, test_hooks,
};
use serde_json::json;
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

#[derive(Debug, PartialEq, Eq)]
struct LeaseSnapshot {
    lease_id: String,
    workspace_id: String,
    root_generation: String,
    owner_id: String,
    kind: String,
    daemon_generation: Option<String>,
    host_instance_id: Option<String>,
    acquired_at: String,
    expires_at: Option<String>,
    renewed_at: Option<String>,
    released_at: Option<String>,
}

async fn lease_snapshot(store: &Store) -> Vec<LeaseSnapshot> {
    sqlx::query("SELECT * FROM workspace_leases ORDER BY lease_id")
        .fetch_all(test_hooks::pool(store))
        .await
        .unwrap()
        .iter()
        .map(|row| LeaseSnapshot {
            lease_id: row.get("lease_id"),
            workspace_id: row.get("workspace_id"),
            root_generation: row.get("root_generation"),
            owner_id: row.get("owner_id"),
            kind: row.get("kind"),
            daemon_generation: row.get("daemon_generation"),
            host_instance_id: row.get("host_instance_id"),
            acquired_at: row.get("acquired_at"),
            expires_at: row.get("expires_at"),
            renewed_at: row.get("renewed_at"),
            released_at: row.get("released_at"),
        })
        .collect()
}

async fn seed_leases(store: &Store) {
    sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at)
         VALUES (?, ?, 'generation-renew', 'run-owner-opaque', 'run', ?)",
    )
    .bind("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6")
    .bind(ID)
    .bind(CREATED)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind, daemon_generation,
          host_instance_id, acquired_at, expires_at)
         VALUES (?, ?, 'generation-renew', 'host-owner-opaque', 'host',
                 'daemon-generation-opaque', 'host-owner-opaque', ?, ?)",
    )
    .bind("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7")
    .bind(ID)
    .bind(CREATED)
    .bind("2026-09-19T00:00:45.000Z")
    .execute(test_hooks::pool(store))
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

#[tokio::test]
async fn lazy_expiry_commits_before_renewal_rejects_without_repeating_writes() {
    let s = Store::open_memory().await.unwrap();
    create(&s).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
    let now = WorkspaceInstant::parse("2026-09-19T00:02:00.000Z").unwrap();
    let ttl = WorkspaceTtl::new(120_000).unwrap();
    let expected = Err(WorkspaceStoreError::InvalidValue {
        field: "workspace_state",
    });
    assert_eq!(s.renew_workspace(&id, &owner, ttl, &now).await, expected);
    let expired = s.get_workspace(&id).await.unwrap();
    assert_eq!(expired.state, "expired");
    assert_eq!(expired.revision, 2);
    assert_eq!(expired.expires_at, "2026-09-19T00:01:00.000Z");
    let audit = s.list_audit().await.unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "workspace.expired");

    assert_eq!(s.renew_workspace(&id, &owner, ttl, &now).await, expected);
    assert_eq!(s.get_workspace(&id).await.unwrap(), expired);
    assert_eq!(s.list_audit().await.unwrap(), audit);
}

#[tokio::test]
async fn renewal_rejects_times_before_creation_or_prior_renewal() {
    let s = Store::open_memory().await.unwrap();
    create(&s).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
    let ttl = WorkspaceTtl::new(120_000).unwrap();
    let before_create = WorkspaceInstant::parse("2026-09-18T23:59:59.999Z").unwrap();
    assert_eq!(
        s.renew_workspace(&id, &owner, ttl, &before_create).await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "renewed_at"
        })
    );
    let first = WorkspaceInstant::parse("2026-09-19T00:00:30.000Z").unwrap();
    s.renew_workspace(&id, &owner, ttl, &first).await.unwrap();
    let before_renewal = WorkspaceInstant::parse("2026-09-19T00:00:29.999Z").unwrap();
    assert_eq!(
        s.renew_workspace(&id, &owner, ttl, &before_renewal).await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "renewed_at"
        })
    );
    let row = s.get_workspace(&id).await.unwrap();
    assert_eq!(row.renewed_at.as_deref(), Some(first.as_str()));
    assert_eq!(row.revision, 2);
    assert_eq!(s.list_audit().await.unwrap().len(), 1);
}

#[tokio::test]
async fn releasing_cleanup_failed_and_released_workspaces_are_unchanged() {
    for (state, setup) in [
        (
            "releasing",
            "UPDATE workspaces SET state='releasing' WHERE id=?",
        ),
        (
            "cleanup_failed",
            "UPDATE workspaces SET state='cleanup_failed', cleanup_error='failed', cleanup_retry_at=?, cleanup_attempts=1, cleanup_last_attempt_at=? WHERE id=?",
        ),
        (
            "released",
            "UPDATE workspaces SET state='released', released_at=?, quarantine_root='/quarantine', quarantined_at=? WHERE id=?",
        ),
    ] {
        let s = Store::open_memory().await.unwrap();
        create(&s).await;
        let mut query = sqlx::query(setup);
        if state == "cleanup_failed" || state == "released" {
            query = query.bind(CREATED).bind(CREATED).bind(ID);
        } else {
            query = query.bind(ID);
        }
        query.execute(test_hooks::pool(&s)).await.unwrap();
        let id = WorkspaceId::parse(ID).unwrap();
        let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
        let now = WorkspaceInstant::parse("2026-09-19T00:00:30.000Z").unwrap();
        assert_eq!(
            s.renew_workspace(&id, &owner, WorkspaceTtl::new(120_000).unwrap(), &now)
                .await,
            Err(WorkspaceStoreError::InvalidValue {
                field: "workspace_state"
            })
        );
        let row = s.get_workspace(&id).await.unwrap();
        assert_eq!(row.state, state);
        assert_eq!(row.revision, 1);
        assert_eq!(row.renewed_at, None);
        assert!(s.list_audit().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn renewal_success_replay_and_lazy_expiry_preserve_every_lease_field() {
    let s = Store::open_memory().await.unwrap();
    create(&s).await;
    seed_leases(&s).await;
    let before = lease_snapshot(&s).await;
    assert_eq!(before.len(), 2);
    let id = WorkspaceId::parse(ID).unwrap();
    let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
    let ttl = WorkspaceTtl::new(120_000).unwrap();
    let renewed_at = WorkspaceInstant::parse("2026-09-19T00:00:30.000Z").unwrap();

    let renewed = s
        .renew_workspace(&id, &owner, ttl, &renewed_at)
        .await
        .unwrap();
    assert_eq!(lease_snapshot(&s).await, before);
    assert_eq!(
        s.renew_workspace(&id, &owner, ttl, &renewed_at).await,
        Ok(renewed)
    );
    assert_eq!(lease_snapshot(&s).await, before);

    let expiry = WorkspaceInstant::parse("2026-09-19T00:03:00.000Z").unwrap();
    assert_eq!(
        s.renew_workspace(&id, &owner, ttl, &expiry).await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "workspace_state"
        })
    );
    assert_eq!(lease_snapshot(&s).await, before);
}

#[tokio::test]
async fn renewal_audit_payload_is_exact_and_excludes_private_root_material() {
    let s = Store::open_memory().await.unwrap();
    create(&s).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
    let now = WorkspaceInstant::parse("2026-09-19T00:00:30.000Z").unwrap();
    s.renew_workspace(&id, &owner, WorkspaceTtl::new(120_000).unwrap(), &now)
        .await
        .unwrap();

    let audit = s.list_audit().await.unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].ts, now.as_str());
    assert_eq!(audit[0].actor, "workspace_lifecycle");
    assert_eq!(audit[0].action, "workspace.renewed");
    assert_eq!(
        audit[0].detail,
        json!({
            "id": ID,
            "kind": "orchestrator_scratch",
            "result_state": "active",
            "owner_ref": "owner",
            "reason": "owner_requested"
        })
    );
    let payload = audit[0].detail.to_string();
    for private in ["/renew/adversarial", "generation-renew", "token", "secret"] {
        assert!(!payload.contains(private));
    }
}

#[tokio::test]
async fn deferred_renewal_commit_failure_rolls_back_workspace_audit_and_leases() {
    let s = Store::open_memory().await.unwrap();
    create(&s).await;
    seed_leases(&s).await;
    let leases_before = lease_snapshot(&s).await;
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(test_hooks::pool(&s))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER defer_renewal_lease AFTER UPDATE OF renewed_at ON workspaces
         BEGIN
             INSERT INTO workspace_leases
                 (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at)
             VALUES
                 ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X8',
                  'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X9', NEW.root_generation,
                  'orphan-owner', 'run', NEW.created_at);
         END",
    )
    .execute(test_hooks::pool(&s))
    .await
    .unwrap();

    assert_eq!(
        s.renew_workspace(
            &WorkspaceId::parse(ID).unwrap(),
            &LifecycleOwnerRef::parse("owner".into()).unwrap(),
            WorkspaceTtl::new(120_000).unwrap(),
            &WorkspaceInstant::parse("2026-09-19T00:00:30.000Z").unwrap(),
        )
        .await,
        Err(WorkspaceStoreError::Database)
    );
    let row = s
        .get_workspace(&WorkspaceId::parse(ID).unwrap())
        .await
        .unwrap();
    assert_eq!(row.state, "active");
    assert_eq!(row.expires_at, "2026-09-19T00:01:00.000Z");
    assert_eq!(row.renewed_at, None);
    assert_eq!(row.revision, 1);
    assert!(s.list_audit().await.unwrap().is_empty());
    assert_eq!(lease_snapshot(&s).await, leases_before);
}
