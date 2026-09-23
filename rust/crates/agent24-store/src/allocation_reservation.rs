use serde_json::json;
use sqlx::{Sqlite, Transaction};

use crate::{
    AllocationIntent, AllocationPhase, AllocationRecord, RootIdentity, Store, WorkspaceConflict,
    WorkspaceResult, WorkspaceStoreError,
};

async fn select_reserved(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<AllocationRecord> {
    let row = sqlx::query(
        "SELECT * FROM workspace_allocations
         WHERE allocation_id = ? COLLATE BINARY LIMIT 1",
    )
    .bind(intent.allocation_id().as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    let record = AllocationRecord::decode(&row)?;
    let exact = record.id() == intent.allocation_id()
        && record.workspace_id() == intent.workspace_id()
        && record.root_generation() == intent.root_generation()
        && record.relative_name() == intent.relative_name()
        && record.parent_identity() == intent.parent_identity()
        && record.created_at() == intent.created_at()
        && record.phase() == AllocationPhase::Reserved
        && record.root_identity().is_none()
        && record.failure_reason().is_none();
    exact
        .then_some(record)
        .ok_or(WorkspaceStoreError::CorruptRow {
            table: "workspace_allocations",
            field: "row",
        })
}

impl Store {
    /// Reserve a trusted, structurally validated intent in the allocation journal.
    ///
    /// This internal journal primitive does not authorize its caller, establish
    /// lifecycle ownership, register a workspace, or materialize a filesystem
    /// path. Its IDs and locators must be internally derived, never
    /// request-controlled. Public exposure is deferred to the authorized
    /// `WorkspaceService` layer.
    ///
    /// The reservation row and its `workspace.allocation_reserved` audit entry
    /// are committed atomically. Conflicts are checked in this order:
    /// allocation identifier, allocation workspace, allocation relative name,
    /// then a legacy workspace identifier. Replaying the exact allocation
    /// identifier returns `Conflict(WorkspaceConflict::AllocationIdentifier)`.
    ///
    /// The returned record is strictly reread after the audit write. If the
    /// commit result is uncertain, reconcile with
    /// [`Store::get_workspace_allocation`] using `intent.allocation_id()`.
    /// Errors are typed, static, and redacted: database or trigger details and
    /// submitted identifiers are not exposed.
    pub(crate) async fn reserve_workspace_allocation(
        &self,
        intent: &AllocationIntent,
    ) -> WorkspaceResult<AllocationRecord> {
        let mut tx = self.begin_workspace_immediate().await?;
        let conflict: i64 = sqlx::query_scalar(
            "SELECT CASE
             WHEN EXISTS(SELECT 1 FROM workspace_allocations WHERE allocation_id = ? COLLATE BINARY) THEN 1
             WHEN EXISTS(SELECT 1 FROM workspace_allocations WHERE workspace_id = ? COLLATE BINARY) THEN 2
             WHEN EXISTS(SELECT 1 FROM workspace_allocations WHERE relative_name = ? COLLATE BINARY) THEN 3
             WHEN EXISTS(SELECT 1 FROM workspaces WHERE id = ? COLLATE BINARY) THEN 4 ELSE 0 END",
        )
        .bind(intent.allocation_id().as_str())
        .bind(intent.workspace_id().as_str())
        .bind(intent.relative_name())
        .bind(intent.workspace_id().as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        let conflict = match conflict {
            0 => None,
            1 => Some(WorkspaceConflict::AllocationIdentifier),
            2 => Some(WorkspaceConflict::AllocationWorkspace),
            3 => Some(WorkspaceConflict::AllocationRelativeName),
            4 => Some(WorkspaceConflict::Identifier),
            _ => return Err(WorkspaceStoreError::Database),
        };
        if let Some(conflict) = conflict {
            return Err(WorkspaceStoreError::Conflict(conflict));
        }

        let (kind, unix_device, unix_inode, windows_volume, windows_file_id) =
            match intent.parent_identity() {
                RootIdentity::Unix { device, inode } => (
                    "unix",
                    Some(device.to_vec()),
                    Some(inode.to_vec()),
                    None,
                    None,
                ),
                RootIdentity::Windows {
                    volume_serial,
                    file_id,
                } => (
                    "windows",
                    None,
                    None,
                    Some(volume_serial.to_vec()),
                    Some(file_id.to_vec()),
                ),
            };
        sqlx::query(
            "INSERT INTO workspace_allocations
             (allocation_id, workspace_id, root_generation, relative_name,
              parent_identity_kind, parent_unix_device, parent_unix_inode,
              parent_windows_volume, parent_windows_file_id, root_identity_kind,
              root_unix_device, root_unix_inode, root_windows_volume,
              root_windows_file_id, phase, created_at, failure_reason)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL,
                     'reserved', ?, NULL)",
        )
        .bind(intent.allocation_id().as_str())
        .bind(intent.workspace_id().as_str())
        .bind(intent.root_generation())
        .bind(intent.relative_name())
        .bind(kind)
        .bind(unix_device)
        .bind(unix_inode)
        .bind(windows_volume)
        .bind(windows_file_id)
        .bind(intent.created_at().as_str())
        .execute(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;

        let record = select_reserved(&mut tx, intent).await?;
        let detail = json!({
            "allocation_id": record.id().as_str(),
            "workspace_id": record.workspace_id().as_str(),
            "phase": "reserved",
        });
        Store::append_audit_tx(
            &mut tx,
            record.created_at().as_str(),
            "workspace_allocation",
            "workspace.allocation_reserved",
            &detail,
        )
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;

        // The audit INSERT is an in-transaction mutation point (including
        // through SQLite triggers), so reread the allocation before checking
        // the legacy workspace guard and crossing the commit boundary.
        let record = select_reserved(&mut tx, intent).await?;

        // A legacy workspace can appear after the journal INSERT (for
        // example, through a trigger or a future internal write path).
        // Recheck in this transaction after the audit INSERT so every
        // successful reservation still owns the workspace identifier at the
        // commit boundary.  Keep this conflict static: callers must not see
        // database/trigger details or the submitted identifier.
        let workspace_exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM workspaces WHERE id = ? COLLATE BINARY LIMIT 1")
                .bind(intent.workspace_id().as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
        if workspace_exists.is_some() {
            return Err(WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier));
        }

        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(record)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        LifecycleOwnerRef, NewScratchWorkspace, TrustedRootRegistration, WorkspaceInstant,
        WorkspaceProvenanceInput, WorkspaceTtl,
    };
    use agent24_protocol::WorkspaceId;
    use sqlx::Row;
    use sqlx::sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqliteOperation, SqlitePoolOptions,
    };
    use std::{path::Path, str::FromStr, sync::Arc, sync::mpsc};
    use tokio::sync::Barrier;
    use tokio::time::{Duration, timeout};

    const HOOK_TIMEOUT: Duration = Duration::from_secs(5);
    const ALLOCATION_ID: &str = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";

    async fn hooked_store(path: &Path) -> Store {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(HOOK_TIMEOUT)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        Store { pool }
    }

    fn intent(id: &str, workspace: &str, name: &str) -> AllocationIntent {
        AllocationIntent::new(
            crate::AllocationId::parse(id).unwrap(),
            WorkspaceId::parse(workspace).unwrap(),
            "generation-1".into(),
            name.into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            crate::WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
        )
        .unwrap()
    }

    async fn counts(store: &Store) -> (i64, i64) {
        let row = sqlx::query(
            "SELECT (SELECT COUNT(*) FROM workspace_allocations) AS allocations,
                    (SELECT COUNT(*) FROM audit_log) AS audit",
        )
        .fetch_one(crate::test_hooks::pool(store))
        .await
        .unwrap();
        (row.get("allocations"), row.get("audit"))
    }

    #[tokio::test]
    async fn reservation_is_db_only_and_exact_replay_is_idempotently_rejected() {
        let store = Store::open_memory().await.unwrap();
        let input = intent(ALLOCATION_ID, WORKSPACE_ID, "public-api");

        let record = store.reserve_workspace_allocation(&input).await.unwrap();
        assert_eq!(record.id().as_str(), ALLOCATION_ID);
        assert_eq!(record.workspace_id().as_str(), WORKSPACE_ID);
        assert_eq!(record.phase(), AllocationPhase::Reserved);
        assert!(record.root_identity().is_none());
        assert!(record.failure_reason().is_none());
        assert_eq!(
            store
                .get_workspace(&WorkspaceId::parse(WORKSPACE_ID).unwrap())
                .await
                .err(),
            Some(WorkspaceStoreError::NotFound)
        );

        let audit = store.list_audit().await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "workspace.allocation_reserved");

        assert_eq!(
            store.reserve_workspace_allocation(&input).await.err(),
            Some(WorkspaceStoreError::Conflict(
                WorkspaceConflict::AllocationIdentifier
            ))
        );
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
    }

    fn legacy_workspace(id: &str, root: &str, identity: RootIdentity) -> NewScratchWorkspace {
        NewScratchWorkspace::new(
            WorkspaceId::parse(id).unwrap(),
            TrustedRootRegistration::new(root.into(), "generation-1".into(), identity).unwrap(),
            WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
            LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap(),
            WorkspaceTtl::new(60_000).unwrap(),
        )
    }

    async fn ordering_counts(store: &Store) -> (i64, i64, i64) {
        let row = sqlx::query(
            "SELECT (SELECT COUNT(*) FROM workspaces) AS workspaces,
                    (SELECT COUNT(*) FROM workspace_allocations) AS allocations,
                    (SELECT COUNT(*) FROM audit_log) AS audit",
        )
        .fetch_one(crate::test_hooks::pool(store))
        .await
        .unwrap();
        (
            row.get("workspaces"),
            row.get("allocations"),
            row.get("audit"),
        )
    }

    async fn wal_race(
        path: &Path,
        left: AllocationIntent,
        right: AllocationIntent,
    ) -> (
        WorkspaceResult<AllocationRecord>,
        WorkspaceResult<AllocationRecord>,
        Store,
    ) {
        let first = Store::open(path).await.unwrap();
        let second = Store::open(path).await.unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let first_gate = Arc::clone(&barrier);
        let second_gate = Arc::clone(&barrier);
        let (left, right) = tokio::join!(
            async move {
                first_gate.wait().await;
                first.reserve_workspace_allocation(&left).await
            },
            async move {
                second_gate.wait().await;
                second.reserve_workspace_allocation(&right).await
            },
        );
        (left, right, Store::open(path).await.unwrap())
    }

    async fn assert_wal_conflict(
        path: &Path,
        left: AllocationIntent,
        right: AllocationIntent,
        conflict: WorkspaceConflict,
    ) {
        let (left_result, right_result, observer) = wal_race(path, left, right).await;
        assert!(left_result.is_ok() ^ right_result.is_ok());
        let winner = left_result
            .as_ref()
            .ok()
            .or_else(|| right_result.as_ref().ok())
            .unwrap();
        let loser = if left_result.is_ok() {
            right_result.as_ref().err().unwrap()
        } else {
            left_result.as_ref().err().unwrap()
        };
        assert_eq!(loser, &WorkspaceStoreError::Conflict(conflict));
        let visible = observer
            .get_workspace_allocation(winner.id())
            .await
            .unwrap();
        assert_eq!(visible.id().as_str(), winner.id().as_str());
        assert_eq!(counts(&observer).await, (1, 1));
        observer.verify_audit_chain().await.unwrap();

        let replay = AllocationIntent::new(
            winner.id().clone(),
            winner.workspace_id().clone(),
            winner.root_generation().to_owned(),
            winner.relative_name().to_owned(),
            winner.parent_identity(),
            winner.created_at().clone(),
        )
        .unwrap();
        assert!(matches!(
            observer.reserve_workspace_allocation(&replay).await,
            Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::AllocationIdentifier
            ))
        ));
        assert_eq!(counts(&observer).await, (1, 1));
        observer.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_caller_after_commit_hook_entry_keeps_durable_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commit-uncertain.sqlite");
        let intent = intent(
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "commit-uncertain",
        );
        let store = hooked_store(&path).await;
        let observer = Store::open(&path).await.unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut connection = store.pool.acquire().await.unwrap();
        connection
            .lock_handle()
            .await
            .unwrap()
            .set_commit_hook(move || {
                let _ = entered_tx.send(());
                release_rx.recv_timeout(HOOK_TIMEOUT).is_ok()
            });
        drop(connection);
        let task_store = store.clone();
        let task_intent = Arc::new(intent);
        let task_intent_for_task = Arc::clone(&task_intent);
        let task = tokio::spawn(async move {
            task_store
                .reserve_workspace_allocation(&task_intent_for_task)
                .await
        });
        timeout(
            HOOK_TIMEOUT,
            tokio::task::spawn_blocking(move || entered_rx.recv_timeout(HOOK_TIMEOUT)),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(counts(&observer).await, (0, 0));
        task.abort();
        assert!(matches!(
            timeout(HOOK_TIMEOUT, task).await,
            Ok(Err(error)) if error.is_cancelled()
        ));
        release_tx.send(()).unwrap();
        timeout(HOOK_TIMEOUT, store.pool.close()).await.unwrap();

        let reopened = Store::open(&path).await.unwrap();
        assert_eq!(counts(&reopened).await, (1, 1));
        let record = reopened
            .get_workspace_allocation(task_intent.allocation_id())
            .await
            .unwrap();
        assert!(record.id() == task_intent.allocation_id());
        assert!(record.workspace_id() == task_intent.workspace_id());
        assert_eq!(record.root_generation(), task_intent.root_generation());
        assert_eq!(record.relative_name(), task_intent.relative_name());
        assert!(record.parent_identity() == task_intent.parent_identity());
        assert!(record.created_at() == task_intent.created_at());
        assert_eq!(record.phase(), AllocationPhase::Reserved);
        assert!(record.root_identity().is_none() && record.failure_reason().is_none());
        assert!(matches!(
            reopened.reserve_workspace_allocation(&task_intent).await,
            Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::AllocationIdentifier
            ))
        ));
        assert_eq!(counts(&reopened).await, (1, 1));
    }

    #[tokio::test]
    async fn cancelled_caller_before_commit_rolls_back_after_connection_barrier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pre-commit-cancel.sqlite");
        let task_intent = intent(
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "pre-commit-cancel",
        );
        let store = timeout(HOOK_TIMEOUT, hooked_store(&path)).await.unwrap();
        let observer = timeout(HOOK_TIMEOUT, Store::open(&path))
            .await
            .unwrap()
            .unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);

        // `hooked_store` has exactly one WAL connection. Installing the hook on
        // that connection makes the callback boundary deterministic: the INSERT
        // is visible only inside the uncommitted transaction, before the audit
        // INSERT and COMMIT can run.
        let mut connection = timeout(HOOK_TIMEOUT, store.pool.acquire())
            .await
            .unwrap()
            .unwrap();
        let mut hook_handle = timeout(HOOK_TIMEOUT, connection.lock_handle())
            .await
            .unwrap()
            .unwrap();
        hook_handle.set_update_hook(move |event| {
            if event.operation == SqliteOperation::Insert
                && event.database == "main"
                && event.table == "workspace_allocations"
            {
                let _ = entered_tx.send(());
                let _ = release_rx.recv_timeout(HOOK_TIMEOUT);
            }
        });
        drop(hook_handle);
        drop(connection);

        let task_store = store.clone();
        let task_intent = Arc::new(task_intent);
        let task_intent_for_task = Arc::clone(&task_intent);
        let task = tokio::spawn(async move {
            task_store
                .reserve_workspace_allocation(&task_intent_for_task)
                .await
        });
        timeout(
            HOOK_TIMEOUT,
            tokio::task::spawn_blocking(move || entered_rx.recv_timeout(HOOK_TIMEOUT)),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();

        // The worker is inside the SQLite update hook, so the transaction has
        // not reached COMMIT. Aborting drops the SQLx transaction and queues its
        // rollback; the callback is released only after cancellation is known.
        assert_eq!(
            timeout(HOOK_TIMEOUT, counts(&observer)).await.unwrap(),
            (0, 0)
        );
        task.abort();
        assert!(matches!(
            timeout(HOOK_TIMEOUT, task).await,
            Ok(Err(error)) if error.is_cancelled()
        ));
        release_tx.send(()).unwrap();

        // A query on the same physical connection is a FIFO barrier behind the
        // rollback queued by Transaction::drop. Do not inspect the database
        // until this barrier completes.
        let mut barrier = timeout(HOOK_TIMEOUT, store.pool.acquire())
            .await
            .unwrap()
            .unwrap();
        timeout(HOOK_TIMEOUT, sqlx::query("SELECT 1").execute(&mut *barrier))
            .await
            .unwrap()
            .unwrap();
        timeout(HOOK_TIMEOUT, barrier.lock_handle())
            .await
            .unwrap()
            .unwrap()
            .remove_update_hook();
        drop(barrier);
        timeout(HOOK_TIMEOUT, store.pool.close()).await.unwrap();

        let reopened = timeout(HOOK_TIMEOUT, Store::open(&path))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            timeout(HOOK_TIMEOUT, counts(&reopened)).await.unwrap(),
            (0, 0)
        );
        let record = timeout(
            HOOK_TIMEOUT,
            reopened.reserve_workspace_allocation(&task_intent),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(record.id().as_str(), task_intent.allocation_id().as_str());
        assert_eq!(record.phase(), AllocationPhase::Reserved);
        assert_eq!(
            timeout(HOOK_TIMEOUT, counts(&reopened)).await.unwrap(),
            (1, 1)
        );
        timeout(HOOK_TIMEOUT, reopened.verify_audit_chain())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn rejected_commit_rolls_back_and_retry_on_new_connection_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commit-rejected.sqlite");
        let intent = intent(
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "commit-rejected",
        );
        let store = hooked_store(&path).await;
        let observer = Store::open(&path).await.unwrap();
        let (hooked_tx, hooked_rx) = mpsc::sync_channel(1);
        let mut connection = store.pool.acquire().await.unwrap();
        connection
            .lock_handle()
            .await
            .unwrap()
            .set_commit_hook(move || {
                let _ = hooked_tx.try_send(());
                false
            });
        drop(connection);

        let error = store
            .reserve_workspace_allocation(&intent)
            .await
            .err()
            .unwrap();
        assert_eq!(error, WorkspaceStoreError::Database);
        assert_eq!(error.to_string(), "workspace database error");
        assert!(!error.to_string().contains("commit"));
        timeout(
            HOOK_TIMEOUT,
            tokio::task::spawn_blocking(move || hooked_rx.recv_timeout(HOOK_TIMEOUT)),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(counts(&observer).await, (0, 0));

        let mut connection = store.pool.acquire().await.unwrap();
        connection.lock_handle().await.unwrap().remove_commit_hook();
        drop(connection);
        timeout(HOOK_TIMEOUT, store.pool.close()).await.unwrap();

        let reopened = Store::open(&path).await.unwrap();
        assert_eq!(counts(&reopened).await, (0, 0));
        let record = reopened
            .reserve_workspace_allocation(&intent)
            .await
            .unwrap();
        assert_eq!(record.id().as_str(), intent.allocation_id().as_str());
        assert_eq!(record.phase(), AllocationPhase::Reserved);
        assert_eq!(counts(&reopened).await, (1, 1));
        reopened.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn wal_reservation_conflicts_are_serialized_and_replay_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        assert_wal_conflict(
            &dir.path().join("same-id.sqlite"),
            intent(
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "same-id-left",
            ),
            intent(
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
                "same-id-right",
            ),
            WorkspaceConflict::AllocationIdentifier,
        )
        .await;
        assert_wal_conflict(
            &dir.path().join("same-workspace.sqlite"),
            intent(
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X8",
                "same-workspace-name",
            ),
            intent(
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X9",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X8",
                "same-workspace-name",
            ),
            WorkspaceConflict::AllocationWorkspace,
        )
        .await;
        assert_wal_conflict(
            &dir.path().join("same-relative-name.sqlite"),
            intent(
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XA",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XB",
                "same-relative-name",
            ),
            intent(
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XC",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XD",
                "same-relative-name",
            ),
            WorkspaceConflict::AllocationRelativeName,
        )
        .await;
    }

    #[tokio::test]
    async fn file_wal_legacy_create_and_reservation_race_has_only_valid_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-reservation-race.sqlite");
        let now = WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap();
        let owner = LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap();
        let workspace_id = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        let allocation_id = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        let intent = intent(allocation_id, workspace_id, "wal-race");
        let legacy = legacy_workspace(workspace_id, "/legacy/wal-race", intent.parent_identity());
        let legacy_store = hooked_store(&path).await;
        let reservation_store = hooked_store(&path).await;
        let legacy_task_store = legacy_store.clone();
        let reservation_task_store = reservation_store.clone();
        let barrier = Arc::new(Barrier::new(2));
        let legacy_gate = Arc::clone(&barrier);
        let reservation_gate = Arc::clone(&barrier);
        let legacy_owner = owner.clone();
        let legacy_now = now.clone();
        let (legacy_result, reservation_result) = tokio::join!(
            async move {
                legacy_gate.wait().await;
                legacy_task_store
                    .create_workspace(&legacy, &legacy_owner, &legacy_now)
                    .await
            },
            async move {
                reservation_gate.wait().await;
                reservation_task_store
                    .reserve_workspace_allocation(&intent)
                    .await
            },
        );
        let reservation_won = match (&legacy_result, &reservation_result) {
            (Ok(workspace), Err(WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier))) => {
                assert_eq!(workspace.id.as_str(), workspace_id);
                false
            }
            (Ok(workspace), Ok(record)) => {
                assert_eq!(workspace.id.as_str(), workspace_id);
                assert_eq!(record.id().as_str(), allocation_id);
                true
            }
            _ => panic!("unexpected legacy/reservation WAL result"),
        };
        legacy_store.pool.close().await;
        reservation_store.pool.close().await;

        let reopened = Store::open(&path).await.unwrap();
        let workspace_key = WorkspaceId::parse(workspace_id).unwrap();
        let allocation_key = crate::AllocationId::parse(allocation_id).unwrap();
        assert_eq!(
            reopened.get_workspace(&workspace_key).await.unwrap().id,
            workspace_key
        );
        if reservation_won {
            assert!(
                reopened
                    .get_workspace_allocation(&allocation_key)
                    .await
                    .is_ok()
            );
            assert_eq!(ordering_counts(&reopened).await, (1, 1, 1));
        } else {
            assert!(matches!(
                reopened.get_workspace_allocation(&allocation_key).await,
                Err(WorkspaceStoreError::NotFound)
            ));
            assert_eq!(ordering_counts(&reopened).await, (1, 0, 0));
        }
        reopened.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn allocation_conflict_priority_is_adjacent_and_legacy_is_below_journal_name() {
        let store = Store::open_memory().await.unwrap();
        store
            .reserve_workspace_allocation(&intent(
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "identifier-priority",
            ))
            .await
            .unwrap();
        assert!(matches!(
            store
                .reserve_workspace_allocation(&intent(
                    "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                    "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                    "identifier-vs-workspace",
                ))
                .await,
            Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::AllocationIdentifier
            ))
        ));

        store
            .reserve_workspace_allocation(&intent(
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
                "workspace-priority",
            ))
            .await
            .unwrap();
        assert!(matches!(
            store
                .reserve_workspace_allocation(&intent(
                    "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
                    "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
                    "workspace-priority",
                ))
                .await,
            Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::AllocationWorkspace
            ))
        ));

        let legacy_id = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X8";
        store
            .reserve_workspace_allocation(&intent(
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X9",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X9",
                "journal-and-legacy",
            ))
            .await
            .unwrap();
        let now = WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap();
        let owner = LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap();
        store
            .create_workspace(
                &legacy_workspace(
                    legacy_id,
                    "/legacy/priority",
                    RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
                ),
                &owner,
                &now,
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .reserve_workspace_allocation(&intent(
                    "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XA",
                    legacy_id,
                    "journal-and-legacy",
                ))
                .await,
            Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::AllocationRelativeName
            ))
        ));
        assert_eq!(ordering_counts(&store).await, (1, 3, 3));
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn reserves_both_parent_identities_with_audit() {
        let store = Store::open_memory().await.unwrap();
        for (aid, wid, name, identity) in [
            (
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "unix",
                RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            ),
            (
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
                "windows",
                RootIdentity::windows(&[3; 8], &[4; 16]).unwrap(),
            ),
        ] {
            let intent = AllocationIntent::new(
                crate::AllocationId::parse(aid).unwrap(),
                WorkspaceId::parse(wid).unwrap(),
                "generation-1".into(),
                name.into(),
                identity,
                crate::WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
            )
            .unwrap();
            let record = store.reserve_workspace_allocation(&intent).await.unwrap();
            assert_eq!(record.parent_identity(), identity);
            assert_eq!(record.phase(), AllocationPhase::Reserved);
            assert!(record.root_identity().is_none() && record.failure_reason().is_none());
        }
        let audit = store.list_audit().await.unwrap();
        assert_eq!(audit.len(), 2);
        assert!(
            audit
                .iter()
                .all(|entry| entry.action == "workspace.allocation_reserved")
        );
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn audit_failure_rolls_back_and_connection_recovers() {
        let store = Store::open_memory().await.unwrap();
        let i = intent(
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "audit-failure",
        );
        sqlx::query(
            "CREATE TRIGGER reject_reservation_audit BEFORE INSERT ON audit_log
             BEGIN SELECT RAISE(ABORT, 'secret reservation audit'); END",
        )
        .execute(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        let error = store.reserve_workspace_allocation(&i).await.err().unwrap();
        assert_eq!(error, WorkspaceStoreError::Database);
        assert_eq!(error.to_string(), "workspace database error");
        assert!(!error.to_string().contains("secret reservation audit"));
        assert_eq!(counts(&store).await, (0, 0));
        sqlx::query("DROP TRIGGER reject_reservation_audit")
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(store.reserve_workspace_allocation(&i).await.is_ok());
    }

    #[tokio::test]
    async fn strict_reread_failures_roll_back_and_recover() {
        for (name, trigger_name, update, field) in [
            (
                "mismatch",
                "mismatch",
                "UPDATE workspace_allocations SET root_generation = 'other-generation' WHERE allocation_id = NEW.allocation_id",
                "row",
            ),
            (
                "invalid-field",
                "invalid_field",
                "UPDATE workspace_allocations SET root_generation = CAST(42 AS BLOB) WHERE allocation_id = NEW.allocation_id",
                "root_generation",
            ),
        ] {
            let store = Store::open_memory().await.unwrap();
            let i = intent(
                if name == "mismatch" {
                    "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X8"
                } else {
                    "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X9"
                },
                if name == "mismatch" {
                    "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X8"
                } else {
                    "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X9"
                },
                name,
            );
            let trigger = format!(
                "CREATE TRIGGER tamper_{trigger_name} AFTER INSERT ON workspace_allocations BEGIN {update}; END"
            );
            sqlx::query(&trigger)
                .execute(crate::test_hooks::pool(&store))
                .await
                .unwrap();
            assert_eq!(
                store.reserve_workspace_allocation(&i).await.err(),
                Some(WorkspaceStoreError::CorruptRow {
                    table: "workspace_allocations",
                    field,
                })
            );
            assert_eq!(counts(&store).await, (0, 0));
            sqlx::query(&format!("DROP TRIGGER tamper_{trigger_name}"))
                .execute(crate::test_hooks::pool(&store))
                .await
                .unwrap();
            assert!(store.reserve_workspace_allocation(&i).await.is_ok());
        }
    }

    #[tokio::test]
    async fn audit_insert_mutations_are_caught_by_final_strict_reread() {
        for (name, update, field) in [
            (
                "semantic",
                "UPDATE workspace_allocations SET root_generation = 'other-generation' WHERE allocation_id = (SELECT json_extract(NEW.detail, '$.allocation_id'))",
                "row",
            ),
            (
                "storage",
                "UPDATE workspace_allocations SET root_generation = CAST('generation-1' AS BLOB) WHERE allocation_id = (SELECT json_extract(NEW.detail, '$.allocation_id'))",
                "root_generation",
            ),
        ] {
            let store = Store::open_memory().await.unwrap();
            let id = if name == "semantic" {
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XB"
            } else {
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XC"
            };
            let workspace = if name == "semantic" {
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XB"
            } else {
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XC"
            };
            let i = intent(id, workspace, name);
            let trigger_name = format!("tamper_audit_{name}");
            let trigger = format!(
                "CREATE TRIGGER {trigger_name} AFTER INSERT ON audit_log
                 WHEN NEW.action = 'workspace.allocation_reserved'
                 BEGIN {update}; END"
            );
            sqlx::query(&trigger)
                .execute(crate::test_hooks::pool(&store))
                .await
                .unwrap();
            assert_eq!(
                store.reserve_workspace_allocation(&i).await.err(),
                Some(WorkspaceStoreError::CorruptRow {
                    table: "workspace_allocations",
                    field,
                })
            );
            assert_eq!(counts(&store).await, (0, 0));
            sqlx::query(&format!("DROP TRIGGER {trigger_name}"))
                .execute(crate::test_hooks::pool(&store))
                .await
                .unwrap();
            assert!(store.reserve_workspace_allocation(&i).await.is_ok());
            assert_eq!(counts(&store).await, (1, 1));
            store.verify_audit_chain().await.unwrap();
        }
    }

    #[tokio::test]
    async fn workspace_id_guard_rolls_back_when_allocation_or_audit_insert_moves_sentinel() {
        for (trigger_name, table) in [
            ("allocation_workspace_id_guard", "workspace_allocations"),
            ("audit_workspace_id_guard", "audit_log"),
        ] {
            let store = Store::open_memory().await.unwrap();
            let target = intent(
                if table == "workspace_allocations" {
                    "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XB"
                } else {
                    "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XC"
                },
                if table == "workspace_allocations" {
                    "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XB"
                } else {
                    "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XC"
                },
                trigger_name,
            );
            let sentinel_id = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XA";
            let sentinel = legacy_workspace(
                sentinel_id,
                if table == "workspace_allocations" {
                    "/legacy/guard-allocation"
                } else {
                    "/legacy/guard-audit"
                },
                RootIdentity::unix(
                    if table == "workspace_allocations" {
                        &[5; 8]
                    } else {
                        &[6; 8]
                    },
                    if table == "workspace_allocations" {
                        &[7; 8]
                    } else {
                        &[8; 8]
                    },
                )
                .unwrap(),
            );
            let owner = LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap();
            let now = WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap();
            store
                .create_workspace(&sentinel, &owner, &now)
                .await
                .unwrap();

            let trigger = format!(
                "CREATE TRIGGER {trigger_name} AFTER INSERT ON {table}
                 BEGIN UPDATE workspaces SET id = '{}' WHERE id = '{}'; END",
                target.workspace_id().as_str(),
                sentinel_id,
            );
            sqlx::query(&trigger)
                .execute(crate::test_hooks::pool(&store))
                .await
                .unwrap();

            let error = match store.reserve_workspace_allocation(&target).await {
                Ok(_) => panic!("workspace ID guard must reject the reservation"),
                Err(error) => error,
            };
            assert_eq!(
                error,
                WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier)
            );
            assert!(!error.to_string().contains(target.workspace_id().as_str()));
            assert_eq!(ordering_counts(&store).await, (1, 0, 0));
            assert_eq!(
                store
                    .get_workspace(&WorkspaceId::parse(sentinel_id).unwrap())
                    .await
                    .unwrap()
                    .id
                    .as_str(),
                sentinel_id
            );
            assert!(matches!(
                store.get_workspace(target.workspace_id()).await,
                Err(WorkspaceStoreError::NotFound)
            ));

            sqlx::query(&format!("DROP TRIGGER {trigger_name}"))
                .execute(crate::test_hooks::pool(&store))
                .await
                .unwrap();
            let record = store.reserve_workspace_allocation(&target).await.unwrap();
            assert_eq!(
                record.workspace_id().as_str(),
                target.workspace_id().as_str()
            );
            assert_eq!(ordering_counts(&store).await, (1, 1, 1));
            store.verify_audit_chain().await.unwrap();
        }
    }

    #[tokio::test]
    async fn allocation_insert_failure_is_redacted_and_recovers() {
        let store = Store::open_memory().await.unwrap();
        let i = intent(
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XA",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XA",
            "insert-failure",
        );
        sqlx::query(
            "CREATE TRIGGER reject_reservation_insert BEFORE INSERT ON workspace_allocations
             BEGIN SELECT RAISE(ABORT, 'secret reservation insert'); END",
        )
        .execute(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        let error = store.reserve_workspace_allocation(&i).await.err().unwrap();
        assert_eq!(error, WorkspaceStoreError::Database);
        assert_eq!(error.to_string(), "workspace database error");
        assert!(!error.to_string().contains("secret reservation insert"));
        assert_eq!(counts(&store).await, (0, 0));
        sqlx::query("DROP TRIGGER reject_reservation_insert")
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(store.reserve_workspace_allocation(&i).await.is_ok());
    }

    #[tokio::test]
    async fn legacy_create_and_reservation_ordering_preserves_each_contract() {
        let now = WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap();
        let owner = LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap();

        // Legacy create wins: reservation sees the real public workspace row.
        let store = Store::open_memory().await.unwrap();
        let first = intent(
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "legacy-first",
        );
        store
            .create_workspace(
                &legacy_workspace(
                    first.workspace_id().as_str(),
                    "/legacy/first",
                    first.parent_identity(),
                ),
                &owner,
                &now,
            )
            .await
            .unwrap();
        assert!(matches!(
            store.reserve_workspace_allocation(&first).await,
            Err(WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier))
        ));
        assert_eq!(ordering_counts(&store).await, (1, 0, 0));
        store.verify_audit_chain().await.unwrap();

        // Reservation wins: legacy create may still materialize the same ID;
        // replaying the intent is an allocation-ID conflict with no new audit.
        let store = Store::open_memory().await.unwrap();
        let second = intent(
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "legacy-after-reserve",
        );
        store.reserve_workspace_allocation(&second).await.unwrap();
        assert_eq!(ordering_counts(&store).await, (0, 1, 1));
        store
            .create_workspace(
                &legacy_workspace(
                    second.workspace_id().as_str(),
                    "/legacy/after-reserve",
                    second.parent_identity(),
                ),
                &owner,
                &now,
            )
            .await
            .unwrap();
        assert_eq!(ordering_counts(&store).await, (1, 1, 1));
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
        store.verify_audit_chain().await.unwrap();
        assert!(matches!(
            store.reserve_workspace_allocation(&second).await,
            Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::AllocationIdentifier
            ))
        ));
        assert_eq!(ordering_counts(&store).await, (1, 1, 1));
        store.verify_audit_chain().await.unwrap();

        // Different IDs remain a positive control: a reservation does not
        // block a legacy workspace with an independent identity and ID.
        let store = Store::open_memory().await.unwrap();
        let reserved = intent(
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "different-reserved",
        );
        store.reserve_workspace_allocation(&reserved).await.unwrap();
        store
            .create_workspace(
                &legacy_workspace(
                    "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X8",
                    "/legacy/different",
                    RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
                ),
                &owner,
                &now,
            )
            .await
            .unwrap();
        assert_eq!(ordering_counts(&store).await, (1, 1, 1));
        store.verify_audit_chain().await.unwrap();
    }
}
