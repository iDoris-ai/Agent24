#![allow(clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    AllocationId, AllocationIntent, AllocationPhase, AllocationRecord, LifecycleOwnerRef,
    NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration, WorkspaceConflict,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceResult, WorkspaceStoreError, WorkspaceTtl,
    test_hooks,
};
use sqlx::{Row, sqlite::SqliteOperation};
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::{sync::Barrier, time::timeout};

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
        format!("root-{n}"),
        identity(n),
        WorkspaceInstant::parse(NOW).unwrap(),
    )
    .unwrap()
}

async fn count(store: &Store, table: &str, action: Option<&str>) -> i64 {
    let pool = test_hooks::pool(store);
    let sql = format!(
        "SELECT count(*) FROM {table}{}",
        if action.is_some() {
            " WHERE action = ?"
        } else {
            ""
        }
    );
    let query = sqlx::query_scalar::<_, i64>(&sql);
    match action {
        Some(value) => query.bind(value).fetch_one(pool).await.unwrap(),
        None => query.fetch_one(pool).await.unwrap(),
    }
}

async fn materialized_count(store: &Store) -> i64 {
    count(
        store,
        "audit_log",
        Some("workspace.allocation_materialized"),
    )
    .await
}

async fn reserved(store: &Store, input: &AllocationIntent) {
    store.reserve_workspace_allocation(input).await.unwrap();
}

async fn exec(store: &Store, statement: &str) {
    sqlx::query(statement)
        .execute(test_hooks::pool(store))
        .await
        .unwrap();
}

async fn materialize_error(
    store: &Store,
    input: &AllocationIntent,
    root: RootIdentity,
) -> WorkspaceStoreError {
    store
        .materialize_workspace_allocation(input, root)
        .await
        .err()
        .unwrap()
}

async fn assert_materialize_error(
    store: &Store,
    input: &AllocationIntent,
    root: RootIdentity,
    expected: WorkspaceStoreError,
) {
    assert_eq!(materialize_error(store, input, root).await, expected);
}

async fn materialize(
    store: &Store,
    input: &AllocationIntent,
    root: RootIdentity,
) -> AllocationRecord {
    store
        .materialize_workspace_allocation(input, root)
        .await
        .unwrap()
}

async fn allocation(store: &Store, input: &AllocationIntent) -> AllocationRecord {
    store
        .get_workspace_allocation(input.allocation_id())
        .await
        .unwrap()
}

async fn assert_reserved_only(store: &Store, input: &AllocationIntent) {
    let row = allocation(store, input).await;
    assert_eq!(row.phase(), AllocationPhase::Reserved);
    assert!(row.root_identity().is_none());
    assert_eq!(count(store, "audit_log", None).await, 1);
    assert_eq!(materialized_count(store).await, 0);
}

async fn signal(rx: mpsc::Receiver<()>) {
    let result = timeout(
        WAIT,
        tokio::task::spawn_blocking(move || rx.recv_timeout(WAIT)),
    )
    .await;
    result.unwrap().unwrap().unwrap();
}

async fn was_cancelled<T>(task: tokio::task::JoinHandle<T>) {
    assert!(matches!(timeout(WAIT, task).await, Ok(Err(error)) if error.is_cancelled()));
}

#[rustfmt::skip]
async fn sentinel(store: &Store, id: &str, root: RootIdentity, suffix: &str) {
    let owner = LifecycleOwnerRef::parse("adversarial-test".to_owned()).unwrap();
    let input = NewScratchWorkspace::new(WorkspaceId::parse(id).unwrap(), TrustedRootRegistration::new(format!("/sentinel/{suffix}"), format!("sentinel-{suffix}"), root).unwrap(), WorkspaceProvenanceInput::new("test".to_owned(), None, None).unwrap(), owner.clone(), WorkspaceTtl::new(60_000).unwrap());
    store.create_workspace(&input, &owner, &WorkspaceInstant::parse(NOW).unwrap()).await.unwrap();
}

fn competitor_update(id: &str, workspace_id: &str, root: RootIdentity, mode: &str) -> String {
    if mode == "identifier" {
        format!("UPDATE workspaces SET id = '{workspace_id}' WHERE id = '{id}'")
    } else {
        let RootIdentity::Unix { device, inode } = root else {
            unreachable!()
        };
        format!(
            "UPDATE workspaces SET unix_device = X'{}', unix_inode = X'{}' WHERE id = '{id}'",
            hex(&device),
            hex(&inode)
        )
    }
}

#[rustfmt::skip]
async fn assert_workspace_root(store: &Store, id: &str, expected: RootIdentity) {
    let row = sqlx::query("SELECT unix_device, unix_inode FROM workspaces WHERE id = ?").bind(id).fetch_one(test_hooks::pool(store)).await.unwrap();
    let (device, inode): (Vec<u8>, Vec<u8>) = (row.get("unix_device"), row.get("unix_inode"));
    let RootIdentity::Unix { device: expected_device, inode: expected_inode } = expected else { unreachable!() };
    assert_eq!((device, inode), (expected_device.to_vec(), expected_inode.to_vec()));
}

async fn pin_four(store: &Store) -> Vec<sqlx::pool::PoolConnection<sqlx::Sqlite>> {
    let mut pinned = Vec::new();
    for _ in 0..4 {
        pinned.push(test_hooks::pool(store).acquire().await.unwrap());
    }
    pinned
}

async fn commit_hook<F: FnMut() -> bool + Send + 'static>(store: &Store, hook: F) {
    let mut c = test_hooks::pool(store).acquire().await.unwrap();
    c.lock_handle().await.unwrap().set_commit_hook(hook);
}

async fn flush(store: &Store, remove_update_hook: bool) {
    let mut c = test_hooks::pool(store).acquire().await.unwrap();
    timeout(WAIT, sqlx::query("SELECT 1").execute(&mut *c))
        .await
        .unwrap()
        .unwrap();
    if remove_update_hook {
        c.lock_handle().await.unwrap().remove_update_hook();
    }
}

fn materialization_task(
    store: Store,
    input: AllocationIntent,
    root: RootIdentity,
) -> tokio::task::JoinHandle<WorkspaceResult<AllocationRecord>> {
    tokio::spawn(async move { store.materialize_workspace_allocation(&input, root).await })
}

#[tokio::test]
async fn independent_wal_replay_and_divergent_root_races_are_serialized() {
    for (same_root, n) in [(true, 1), (false, 2)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.sqlite");
        let seed = Store::open(&path).await.unwrap();
        let input = intent(n);
        reserved(&seed, &input).await;
        drop(seed);
        let left = Store::open(&path).await.unwrap();
        let right = Store::open(&path).await.unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let a = Arc::clone(&barrier);
        let b = Arc::clone(&barrier);
        let root_a = identity(20);
        let root_b = if same_root { root_a } else { identity(22) };
        let (first, second) = tokio::join!(
            async {
                a.wait().await;
                left.materialize_workspace_allocation(&input, root_a).await
            },
            async {
                b.wait().await;
                right.materialize_workspace_allocation(&input, root_b).await
            },
        );
        let winner_root = first
            .as_ref()
            .ok()
            .or_else(|| second.as_ref().ok())
            .unwrap()
            .root_identity()
            .unwrap();
        let observer = Store::open(&path).await.unwrap();
        assert_eq!(count(&observer, "workspace_allocations", None).await, 1);
        assert_eq!(materialized_count(&observer).await, 1);
        let record = allocation(&observer, &input).await;
        assert_eq!(record.phase(), AllocationPhase::Materialized);
        assert_eq!(record.root_identity(), Some(winner_root));
        if same_root {
            assert!(first.is_ok() && second.is_ok());
        } else {
            assert!(first.is_ok() ^ second.is_ok());
            let loser = if first.is_err() {
                first.err().unwrap()
            } else {
                second.err().unwrap()
            };
            assert_eq!(
                loser,
                WorkspaceStoreError::Conflict(WorkspaceConflict::RootIdentity)
            );
            let message = loser.to_string();
            assert_eq!(message, "workspace conflict: root identity");
            assert!(!message.contains(input.allocation_id().as_str()));
            assert!(!message.contains("20"));
        }
    }
}

#[tokio::test]
async fn ignored_cas_update_and_first_reread_tampering_roll_back() {
    for (n, trigger) in [
        (
            3,
            "CREATE TRIGGER ignore_cas BEFORE UPDATE ON workspace_allocations
             BEGIN SELECT RAISE(IGNORE); END",
        ),
        (
            4,
            "CREATE TRIGGER tamper_first AFTER UPDATE ON workspace_allocations
             BEGIN UPDATE workspace_allocations SET root_unix_device = X'0909090909090909'
             WHERE allocation_id = NEW.allocation_id; END",
        ),
    ] {
        let store = Store::open_memory().await.unwrap();
        let input = intent(n);
        reserved(&store, &input).await;
        exec(&store, trigger).await;
        if n == 4 {
            exec(
                &store,
                "CREATE TRIGGER heal_after_audit AFTER INSERT ON audit_log
                WHEN NEW.action = 'workspace.allocation_materialized'
                BEGIN UPDATE workspace_allocations SET root_unix_device = X'1e1e1e1e1e1e1e1e',
                root_unix_inode = X'1f1f1f1f1f1f1f1f'
                WHERE allocation_id = 'wa_01J5M4Q2Y7N8P9R0S1T2V3W4X4'; END",
            )
            .await;
        }
        assert_materialize_error(&store, &input, identity(30), WorkspaceStoreError::Database).await;
        assert_reserved_only(&store, &input).await;
    }
}

#[tokio::test]
async fn audit_abort_and_final_allocation_reread_tamper_roll_back() {
    for (n, trigger) in [
        (
            5,
            "CREATE TRIGGER abort_materialized_audit BEFORE INSERT ON audit_log
             WHEN NEW.action = 'workspace.allocation_materialized'
             BEGIN SELECT RAISE(ABORT, 'private trigger detail'); END",
        ),
        (
            6,
            "CREATE TRIGGER tamper_after_audit AFTER INSERT ON audit_log
             WHEN NEW.action = 'workspace.allocation_materialized'
             BEGIN UPDATE workspace_allocations SET root_unix_device = X'0909090909090909'
             WHERE allocation_id = 'wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6'; END",
        ),
    ] {
        let store = Store::open_memory().await.unwrap();
        let input = intent(n);
        reserved(&store, &input).await;
        let ddl = trigger.replace(
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            input.allocation_id().as_str(),
        );
        exec(&store, &ddl).await;
        let error = store
            .materialize_workspace_allocation(&input, identity(31))
            .await
            .err()
            .unwrap();
        assert_eq!(error, WorkspaceStoreError::Database);
        assert_eq!(error.to_string(), "workspace database error");
        assert!(!error.to_string().contains("private trigger detail"));
        assert_reserved_only(&store, &input).await;
    }
}

#[tokio::test]
#[rustfmt::skip]
async fn post_audit_identifier_and_root_competitors_roll_back_both_changes() {
    for (n, kind) in [(7, "identifier"), (8, "root")] {
        let store = Store::open_memory().await.unwrap();
        let input = intent(n);
        let sentinel_id = format!("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X{}", char::from(b'0' + n + 1));
        let (sentinel_root, claimant_root) = (identity(40 + n), identity(50 + n));
        reserved(&store, &input).await; sentinel(&store, &sentinel_id, sentinel_root, kind).await;
        let ddl = format!("CREATE TRIGGER competitor_{kind} AFTER INSERT ON audit_log WHEN NEW.action = 'workspace.allocation_materialized' BEGIN {}; END", competitor_update(&sentinel_id, input.workspace_id().as_str(), claimant_root, kind));
        exec(&store, &ddl).await;
        assert_materialize_error(&store, &input, claimant_root, WorkspaceStoreError::Database).await;
        assert_reserved_only(&store, &input).await;
        assert_workspace_root(&store, &sentinel_id, sentinel_root).await;
        assert_eq!(count(&store, "workspaces", None).await, 1);
    }
}

#[tokio::test]
#[rustfmt::skip]
async fn existing_and_replay_workspace_competitors_have_typed_conflicts() {
    for (n, mode, conflict) in [
        (1, "identifier", WorkspaceConflict::Identifier),
        (2, "root", WorkspaceConflict::RootIdentity),
    ] {
        competitor_case(n, mode, conflict, false).await;
        competitor_case(n, mode, conflict, true).await;
    }
}

#[rustfmt::skip]
async fn competitor_case(n: u8, mode: &str, conflict: WorkspaceConflict, replay: bool) {
    let store = Store::open_memory().await.unwrap();
    let input = intent(n);
    let sid = format!("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X{}", char::from(b'0' + n + 3));
    let (root, old_root) = (identity(80 + n), identity(70 + n));
    if !replay { sentinel(&store, &sid, old_root, mode).await; }
    reserved(&store, &input).await;
    if replay {
        materialize(&store, &input, root).await;
        sentinel(&store, &sid, old_root, mode).await;
    }
    exec(&store, &competitor_update(&sid, input.workspace_id().as_str(), root, mode)).await;
    assert_materialize_error(&store, &input, root, WorkspaceStoreError::Conflict(conflict)).await;
    assert_eq!(allocation(&store, &input).await.phase(), if replay { AllocationPhase::Materialized } else { AllocationPhase::Reserved });
    assert_eq!(count(&store, "audit_log", Some("workspace.allocation_reserved")).await, 1);
    assert_eq!(materialized_count(&store).await, if replay { 1 } else { 0 });
    let (sid, root) = if mode == "identifier" { (input.workspace_id().as_str(), old_root) } else { (sid.as_str(), root) };
    assert_workspace_root(&store, sid, root).await;
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test]
async fn rejected_commit_rolls_back_materialization_and_exact_replay_has_no_second_audit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("commit-reject.sqlite");
    let store = Store::open(&path).await.unwrap();
    let input = intent(9);
    reserved(&store, &input).await;
    let _pinned = pin_four(&store).await;
    let (seen_tx, seen_rx) = mpsc::sync_channel(1);
    commit_hook(&store, move || {
        let _ = seen_tx.send(());
        false
    })
    .await;
    let error = store
        .materialize_workspace_allocation(&input, identity(60))
        .await
        .err()
        .unwrap();
    assert_eq!(error, WorkspaceStoreError::Database);
    signal(seen_rx).await;
    drop(_pinned);
    assert_reserved_only(&store, &input).await;
    test_hooks::pool(&store).close().await;
    let reopened = Store::open(&path).await.unwrap();
    assert_reserved_only(&reopened, &input).await;
    let root = identity(61);
    materialize(&reopened, &input, root).await;
    materialize(&reopened, &input, root).await;
    assert_eq!(materialized_count(&reopened).await, 1);
}

#[tokio::test]
async fn precommit_cancellation_rolls_back_after_update_hook_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel-before-commit.sqlite");
    let store = Store::open(&path).await.unwrap();
    let input = intent(0);
    reserved(&store, &input).await;
    let mut pinned = pin_four(&store).await;
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let mut connection = test_hooks::pool(&store).acquire().await.unwrap();
    connection
        .lock_handle()
        .await
        .unwrap()
        .set_update_hook(move |event| {
            if event.operation == SqliteOperation::Update && event.table == "workspace_allocations"
            {
                let _ = entered_tx.send(());
                let _ = release_rx.recv_timeout(WAIT);
            }
        });
    drop(connection);
    let task_input = intent(0);
    let task = materialization_task(store.clone(), task_input, identity(62));
    signal(entered_rx).await;
    task.abort();
    was_cancelled(task).await;
    release_tx.send(()).unwrap();
    flush(&store, true).await;
    drop(pinned.drain(..));
    assert_reserved_only(&store, &input).await;
}

#[tokio::test]
async fn blocked_commit_hook_cancellation_reopens_and_reconciles_durable_result() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uncertain-commit.sqlite");
    let store = Store::open(&path).await.unwrap();
    let input = intent(3);
    reserved(&store, &input).await;
    let observer = Store::open(&path).await.unwrap();
    let _pinned = pin_four(&store).await;
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    commit_hook(&store, move || {
        let _ = entered_tx.send(());
        release_rx.recv_timeout(WAIT).is_ok()
    })
    .await;
    let task = materialization_task(store.clone(), intent(3), identity(63));
    signal(entered_rx).await;
    assert_eq!(
        allocation(&observer, &intent(3)).await.phase(),
        AllocationPhase::Reserved
    );
    task.abort();
    was_cancelled(task).await;
    release_tx.send(()).unwrap();
    flush(&store, false).await;
    drop(_pinned);
    test_hooks::pool(&store).close().await;
    test_hooks::pool(&observer).close().await;
    let reopened = Store::open(&path).await.unwrap();
    let record = allocation(&reopened, &intent(3)).await;
    assert_eq!(record.phase(), AllocationPhase::Materialized);
    assert_eq!(record.root_identity(), Some(identity(63)));
    assert_eq!(materialized_count(&reopened).await, 1);
    let exact = intent(3);
    materialize(&reopened, &exact, identity(63)).await;
    assert_eq!(materialized_count(&reopened).await, 1);
    assert!(matches!(
        reopened
            .materialize_workspace_allocation(&exact, identity(64))
            .await,
        Err(WorkspaceStoreError::Conflict(
            WorkspaceConflict::RootIdentity
        ))
    ));
}
