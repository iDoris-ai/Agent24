use agent24_protocol::WorkspaceId;
use sqlx::sqlite::SqliteRow;

use super::workspace_decode_support::{
    at_least, bad, blob, count, instant, nonblank, opt_instant, opt_nonblank, opt_text, text,
};

use crate::{
    RootIdentity, TrustedRootRegistration, WorkspaceAuthority, WorkspaceCleanupRecord,
    WorkspaceInstant, WorkspaceKind, WorkspaceResult, WorkspaceState, WorkspaceTtl,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub id: WorkspaceId,
    pub kind: WorkspaceKind,
    pub state: WorkspaceState,
    pub authority: WorkspaceAuthority,
    pub root: TrustedRootRegistration,
    pub created_at: WorkspaceInstant,
    pub expires_at: WorkspaceInstant,
    pub renewed_at: Option<WorkspaceInstant>,
    pub released_at: Option<WorkspaceInstant>,
    pub revision: u64,
    pub cleanup: WorkspaceCleanupRecord,
    pub ttl: WorkspaceTtl,
}

impl WorkspaceRow {
    pub fn decode(row: &SqliteRow) -> WorkspaceResult<Self> {
        let id = WorkspaceId::parse(text(row, "id")?).map_err(|_| bad("id"))?;
        let kind = WorkspaceKind::parse(&text(row, "kind")?).map_err(|_| bad("kind"))?;
        let state = WorkspaceState::parse(&text(row, "state")?).map_err(|_| bad("state"))?;
        let created_at = instant(row, "created_at")?;
        let expires_at = instant(row, "expires_at")?;
        let renewed_at = opt_instant(row, "renewed_at")?;
        let released_at = opt_instant(row, "released_at")?;
        let base = renewed_at.as_ref().unwrap_or(&created_at);
        let ttl = WorkspaceTtl::new(
            expires_at
                .epoch_millis()
                .checked_sub(base.epoch_millis())
                .ok_or(bad("expires_at"))?,
        )
        .map_err(|_| bad("expires_at"))?;
        if let Some(renewed) = &renewed_at {
            at_least(renewed, &created_at, "renewed_at")?;
            if renewed >= &expires_at {
                return Err(bad("renewed_at"));
            }
        }
        if let Some(released) = &released_at {
            at_least(released, &created_at, "released_at")?;
            if let Some(renewed) = &renewed_at {
                at_least(released, renewed, "released_at")?;
            }
        }
        let quarantined_at = opt_instant(row, "quarantined_at")?;
        if let Some(quarantined) = &quarantined_at {
            at_least(quarantined, &created_at, "quarantined_at")?;
            if let Some(renewed) = &renewed_at {
                at_least(quarantined, renewed, "quarantined_at")?;
            }
            if let Some(released) = &released_at {
                at_least(released, quarantined, "released_at")?;
            }
        }
        let identity_kind = text(row, "root_identity_kind")?;
        let (unix_device, unix_inode, windows_volume, windows_file) = (
            blob(row, "unix_device")?,
            blob(row, "unix_inode")?,
            blob(row, "windows_volume_serial")?,
            blob(row, "windows_file_id")?,
        );
        let identity = match identity_kind.as_str() {
            "unix" if windows_volume.is_none() && windows_file.is_none() => RootIdentity::unix(
                unix_device.as_deref().ok_or(bad("unix_device"))?,
                unix_inode.as_deref().ok_or(bad("unix_inode"))?,
            )
            .map_err(|_| bad("root_identity"))?,
            "windows" if unix_device.is_none() && unix_inode.is_none() => RootIdentity::windows(
                windows_volume
                    .as_deref()
                    .ok_or(bad("windows_volume_serial"))?,
                windows_file.as_deref().ok_or(bad("windows_file_id"))?,
            )
            .map_err(|_| bad("root_identity"))?,
            _ => return Err(bad("root_identity_kind")),
        };
        let root = TrustedRootRegistration::new(
            text(row, "canonical_root")?,
            text(row, "root_generation")?,
            identity,
        )
        .map_err(|_| bad("root"))?;
        let cleanup_attempts = count(row, "cleanup_attempts")?;
        let revision = count(row, "revision")?;
        if revision == 0 {
            return Err(bad("revision"));
        }
        let last_attempt_at = opt_instant(row, "cleanup_last_attempt_at")?;
        if (cleanup_attempts == 0) != last_attempt_at.is_none() {
            return Err(bad("cleanup_attempts"));
        }
        let cleanup = WorkspaceCleanupRecord {
            state,
            quarantine_root: opt_nonblank(row, "quarantine_root")?,
            quarantined_at,
            attempts: cleanup_attempts,
            last_attempt_at,
            error: opt_text(row, "cleanup_error")?,
            retry_at: opt_instant(row, "cleanup_retry_at")?,
        };
        if cleanup.quarantined_at.is_some() && cleanup.quarantine_root.is_none() {
            return Err(bad("quarantined_at"));
        }
        if cleanup.quarantine_root.is_some()
            && cleanup.quarantined_at.is_none()
            && state != WorkspaceState::Releasing
        {
            return Err(bad("quarantine_root"));
        }
        match state {
            WorkspaceState::Active | WorkspaceState::Expired
                if cleanup.quarantine_root.is_some()
                    || cleanup.quarantined_at.is_some()
                    || cleanup.attempts != 0
                    || released_at.is_some()
                    || cleanup.error.is_some()
                    || cleanup.retry_at.is_some() =>
            {
                return Err(bad("state"));
            }
            WorkspaceState::Released
                if released_at.is_none()
                    || cleanup.quarantine_root.is_none()
                    || cleanup.quarantined_at.is_none()
                    || cleanup.error.is_some()
                    || cleanup.retry_at.is_some() =>
            {
                return Err(bad("state"));
            }
            WorkspaceState::CleanupFailed
                if cleanup.error.is_none()
                    || cleanup.retry_at.is_none()
                    || cleanup.attempts == 0
                    || cleanup.last_attempt_at.is_none() =>
            {
                return Err(bad("state"));
            }
            _ => {}
        }
        if state != WorkspaceState::Released && released_at.is_some() {
            return Err(bad("released_at"));
        }
        if state != WorkspaceState::CleanupFailed
            && (cleanup.error.is_some() || cleanup.retry_at.is_some())
        {
            return Err(bad("state"));
        }
        let writeback_policy = text(row, "writeback_policy")?;
        let lifecycle_owner_kind = text(row, "lifecycle_owner_kind")?;
        let concurrency_policy = text(row, "concurrency_policy")?;
        if writeback_policy != "external" {
            return Err(bad("writeback_policy"));
        }
        if lifecycle_owner_kind != "orchestrator" {
            return Err(bad("lifecycle_owner_kind"));
        }
        if concurrency_policy != "serial" {
            return Err(bad("concurrency_policy"));
        }
        Ok(Self {
            id,
            kind,
            state,
            authority: WorkspaceAuthority {
                provenance_source: nonblank(row, "provenance_source")?,
                provenance_project_ref: opt_text(row, "provenance_project_ref")?,
                provenance_base_revision: opt_text(row, "provenance_base_revision")?,
                lifecycle_owner_kind,
                lifecycle_owner_ref: nonblank(row, "lifecycle_owner_ref")?,
                writeback_policy,
                concurrency_policy,
            },
            root,
            created_at,
            expires_at,
            renewed_at,
            released_at,
            revision,
            cleanup,
            ttl,
        })
    }
}
