#![allow(clippy::unwrap_used)]

mod common;

use agent24_protocol::WorkspaceId;
use agent24_store::{
    AllocationFailureReason, AllocationId, AllocationIntent, AllocationPhase, RootIdentity, Store,
    WorkspaceConflict, WorkspaceInstant, WorkspaceStoreError, test_hooks,
};
use sqlx::{Row, Sqlite};
use std::{sync::mpsc, time::Duration};

const NOW: &str = "2026-09-19T00:00:00.000Z";
const WAIT: Duration = Duration::from_secs(5);

fn identity(n: u8) -> RootIdentity {
    RootIdentity::unix(&[n; 8], &[n.wrapping_add(1); 8]).unwrap()
}

fn intent(n: u8) -> AllocationIntent {
    let suffix = char::from(b'0' + n);
    AllocationIntent::new(
        AllocationId::parse(&format!("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X{suffix}")).unwrap(),
        WorkspaceId::parse(format!("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X{suffix}")).unwrap(),
        format!("generation-{n}"),
        format!("scratch-{n}"),
        identity(n),
        WorkspaceInstant::parse(NOW).unwrap(),
    )
    .unwrap()
}

fn reason(value: &str) -> AllocationFailureReason {
    AllocationFailureReason::parse(value).unwrap()
}

async fn materialized(store: &Store, input: &AllocationIntent, root: RootIdentity) {
    common::insert_reserved_allocation(store, input).await;
    store
        .materialize_workspace_allocation(input, root)
        .await
        .unwrap();
}

async fn retention_audits(store: &Store) -> usize {
    store
        .list_audit()
        .await
        .unwrap()
        .iter()
        .filter(|entry| entry.action == "workspace.allocation_retained")
        .count()
}

async fn raw_table(store: &Store, table: &str) -> Vec<Vec<String>> {
    let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(test_hooks::pool(store))
        .await
        .unwrap()
        .into_iter()
        .map(|row| sqlx::Row::try_get::<String, _>(&row, "name").unwrap())
        .collect::<Vec<_>>();
    let fields = columns
        .iter()
        .map(|name| format!("quote(\"{name}\"), typeof(\"{name}\")"))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query(&format!("SELECT {fields} FROM \"{table}\" ORDER BY rowid"))
        .fetch_all(test_hooks::pool(store))
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (0..row.len())
                .map(|index| sqlx::Row::try_get::<String, _>(&row, index).unwrap())
                .collect()
        })
        .collect()
}

async fn snapshot(store: &Store) -> Vec<Vec<Vec<String>>> {
    vec![
        raw_table(store, "workspace_allocations").await,
        raw_table(store, "workspaces").await,
        raw_table(store, "workspace_leases").await,
        raw_table(store, "audit_log").await,
        raw_table(store, "sqlite_sequence").await,
    ]
}

#[tokio::test]
async fn retained_reserved_and_materialized_rows_survive_reopen_and_exact_replay() {
    for (n, is_materialized) in [(1, false), (2, true)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("retained-{n}.sqlite"));
        let store = Store::open(&path).await.unwrap();
        let input = intent(n);
        if is_materialized {
            materialized(&store, &input, identity(20 + n)).await;
        } else {
            common::insert_reserved_allocation(&store, &input).await;
        }

        store
            .retain_workspace_allocation(&input, reason("io_error"))
            .await
            .unwrap();
        drop(store);

        let reopened = Store::open(&path).await.unwrap();
        let record = reopened
            .get_workspace_allocation(input.allocation_id())
            .await
            .unwrap();
        assert_eq!(record.phase(), AllocationPhase::Retained);
        assert!(record.failure_reason() == Some(&reason("io_error")));
        assert_eq!(retention_audits(&reopened).await, 1);

        reopened
            .retain_workspace_allocation(&input, reason("io_error"))
            .await
            .unwrap();
        assert_eq!(retention_audits(&reopened).await, 1);
    }
}

#[tokio::test]
async fn divergent_replays_and_unrelated_audit_leave_retained_bytes_unchanged() {
    let store = Store::open_memory().await.unwrap();
    let input = intent(3);
    materialized(&store, &input, identity(30)).await;
    store
        .retain_workspace_allocation(&input, reason("io_error"))
        .await
        .unwrap();
    let retained = snapshot(&store).await;

    assert_eq!(
        store
            .retain_workspace_allocation(&input, reason("other_reason"))
            .await,
        Err(WorkspaceStoreError::Conflict(
            WorkspaceConflict::AllocationIdentifier
        ))
    );
    assert_eq!(snapshot(&store).await, retained);

    let changed = AllocationIntent::new(
        input.allocation_id().clone(),
        input.workspace_id().clone(),
        input.root_generation().to_owned(),
        "changed".to_owned(),
        input.parent_identity(),
        input.created_at().clone(),
    )
    .unwrap();
    assert_eq!(
        store
            .retain_workspace_allocation(&changed, reason("io_error"))
            .await,
        Err(WorkspaceStoreError::Conflict(
            WorkspaceConflict::AllocationIdentifier
        ))
    );
    assert_eq!(snapshot(&store).await, retained);

    store
        .append_audit(
            NOW,
            "unrelated",
            "unrelated.audit",
            &serde_json::json!({"n": 3}),
        )
        .await
        .unwrap();
    let after_unrelated = snapshot(&store).await;
    store
        .retain_workspace_allocation(&input, reason("io_error"))
        .await
        .unwrap();
    assert_eq!(snapshot(&store).await, after_unrelated);
}

async fn pin_four(store: &Store) -> Vec<sqlx::pool::PoolConnection<Sqlite>> {
    let mut pinned = Vec::new();
    for _ in 0..4 {
        pinned.push(test_hooks::pool(store).acquire().await.unwrap());
    }
    pinned
}

#[tokio::test]
async fn rejected_outer_commit_reopens_original_state_then_allows_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retention-commit-reject.sqlite");
    let store = Store::open(&path).await.unwrap();
    let input = intent(4);
    common::insert_reserved_allocation(&store, &input).await;

    let pinned = pin_four(&store).await;
    let (seen_tx, seen_rx) = mpsc::sync_channel(1);
    let mut exact_connection = test_hooks::pool(&store).acquire().await.unwrap();
    exact_connection
        .lock_handle()
        .await
        .unwrap()
        .set_commit_hook(move || {
            let _ = seen_tx.send(());
            false
        });
    drop(exact_connection);

    assert_eq!(
        store
            .retain_workspace_allocation(&input, reason("io_error"))
            .await,
        Err(WorkspaceStoreError::Database)
    );
    tokio::task::spawn_blocking(move || seen_rx.recv_timeout(WAIT))
        .await
        .unwrap()
        .unwrap();
    drop(pinned);
    test_hooks::pool(&store).close().await;
    drop(store);

    let reopened = Store::open(&path).await.unwrap();
    let original = reopened
        .get_workspace_allocation(input.allocation_id())
        .await
        .unwrap();
    assert_eq!(original.phase(), AllocationPhase::Reserved);
    assert!(original.failure_reason().is_none());
    assert_eq!(retention_audits(&reopened).await, 0);

    reopened
        .retain_workspace_allocation(&input, reason("io_error"))
        .await
        .unwrap();
    assert_eq!(
        reopened
            .get_workspace_allocation(input.allocation_id())
            .await
            .unwrap()
            .phase(),
        AllocationPhase::Retained
    );
    assert_eq!(retention_audits(&reopened).await, 1);
}
