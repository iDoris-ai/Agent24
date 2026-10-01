use agent24_store::{AllocationIntent, RootIdentity, Store, test_hooks};

/// Seed the internal journal state required by public boundary tests.
///
/// Reservation remains crate-private: tests use the existing raw-SQL hook for
/// the one journal row, then the public audit API for the matching audit tail.
pub async fn insert_reserved_allocation(store: &Store, intent: &AllocationIntent) {
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
    .execute(test_hooks::pool(store))
    .await
    .unwrap();

    store
        .append_audit(
            intent.created_at().as_str(),
            "workspace_allocation",
            "workspace.allocation_reserved",
            &serde_json::json!({
                "allocation_id": intent.allocation_id().as_str(),
                "workspace_id": intent.workspace_id().as_str(),
                "phase": "reserved",
            }),
        )
        .await
        .unwrap();
}
