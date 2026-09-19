use agent24_protocol::WorkspaceId;
use sqlx::{Row, sqlite::SqliteRow};

use crate::{
    HostLeaseTtl, LeaseKind, WorkspaceInstant, WorkspaceLeaseId, WorkspaceLeaseRecord,
    WorkspaceResult, WorkspaceStoreError,
};

fn bad(field: &'static str) -> WorkspaceStoreError {
    WorkspaceStoreError::CorruptRow {
        table: "workspace_leases",
        field,
    }
}
fn text(row: &SqliteRow, field: &'static str) -> WorkspaceResult<String> {
    let value = row.try_get::<String, _>(field).map_err(|_| bad(field))?;
    if value.is_empty() || value.contains('\0') {
        return Err(bad(field));
    }
    Ok(value)
}
fn nonblank(row: &SqliteRow, field: &'static str) -> WorkspaceResult<String> {
    let value = text(row, field)?;
    if value.trim().is_empty() {
        return Err(bad(field));
    }
    Ok(value)
}
fn opt_text(row: &SqliteRow, field: &'static str) -> WorkspaceResult<Option<String>> {
    let value = row
        .try_get::<Option<String>, _>(field)
        .map_err(|_| bad(field))?;
    if value.as_deref().is_some_and(|v| v.contains('\0')) {
        return Err(bad(field));
    }
    Ok(value)
}
fn instant(row: &SqliteRow, field: &'static str) -> WorkspaceResult<WorkspaceInstant> {
    WorkspaceInstant::parse(&text(row, field)?).map_err(|_| bad(field))
}
fn opt_instant(
    row: &SqliteRow,
    field: &'static str,
) -> WorkspaceResult<Option<WorkspaceInstant>> {
    opt_text(row, field)?
        .map(|v| WorkspaceInstant::parse(&v).map_err(|_| bad(field)))
        .transpose()
}
fn at_least(
    a: &WorkspaceInstant,
    b: &WorkspaceInstant,
    field: &'static str,
) -> WorkspaceResult<()> {
    (a.epoch_millis() >= b.epoch_millis())
        .then_some(())
        .ok_or(bad(field))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLeaseRow {
    pub record: WorkspaceLeaseRecord,
}

impl WorkspaceLeaseRow {
    pub fn decode(row: &SqliteRow) -> WorkspaceResult<Self> {
        let id = WorkspaceLeaseId::parse(&text(row, "lease_id")?)
            .map_err(|_| bad("lease_id"))?;
        let workspace_id = WorkspaceId::parse(text(row, "workspace_id")?)
            .map_err(|_| bad("workspace_id"))?;
        let root_generation = nonblank(row, "root_generation")?;
        let owner_id = nonblank(row, "owner_id")?;
        let kind = LeaseKind::parse(&text(row, "kind")?).map_err(|_| bad("kind"))?;
        let daemon_generation = opt_text(row, "daemon_generation")?;
        let host_instance_id = opt_text(row, "host_instance_id")?;
        let acquired_at = instant(row, "acquired_at")?;
        let expires_at = opt_instant(row, "expires_at")?;
        let renewed_at = opt_instant(row, "renewed_at")?;
        let released_at = opt_instant(row, "released_at")?;

        match kind {
            LeaseKind::Run if daemon_generation.is_some()
                || host_instance_id.is_some()
                || expires_at.is_some()
                || renewed_at.is_some() =>
            {
                return Err(bad("kind"));
            }
            LeaseKind::Host => {
                let daemon = daemon_generation.as_deref().ok_or(bad("daemon_generation"))?;
                if daemon.trim().is_empty() {
                    return Err(bad("daemon_generation"));
                }
                let host = host_instance_id.as_deref().ok_or(bad("host_instance_id"))?;
                if host.trim().is_empty() || host != owner_id {
                    return Err(bad("host_instance_id"));
                }
                let expires = expires_at.as_ref().ok_or(bad("expires_at"))?;
                if expires <= &acquired_at {
                    return Err(bad("expires_at"));
                }
                if let Some(renewed) = &renewed_at {
                    at_least(renewed, &acquired_at, "renewed_at")?;
                    if renewed >= expires {
                        return Err(bad("renewed_at"));
                    }
                }
                let base = renewed_at.as_ref().unwrap_or(&acquired_at);
                HostLeaseTtl::new(
                    expires
                        .epoch_millis()
                        .checked_sub(base.epoch_millis())
                        .ok_or(bad("expires_at"))?,
                )
                .map_err(|_| bad("expires_at"))?;
            }
        }
        if let Some(released) = &released_at {
            at_least(released, &acquired_at, "released_at")?;
            if let Some(renewed) = &renewed_at {
                at_least(released, renewed, "released_at")?;
            }
        }
        Ok(Self {
            record: WorkspaceLeaseRecord {
                id,
                workspace_id,
                root_generation,
                owner_id,
                kind,
                daemon_generation,
                host_instance_id,
                acquired_at,
                expires_at,
                renewed_at,
                released_at,
            },
        })
    }
}
