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
    pub(crate) id: WorkspaceId,
    pub(crate) kind: WorkspaceKind,
    pub(crate) state: WorkspaceState,
    pub(crate) authority: WorkspaceAuthority,
    pub(crate) root: TrustedRootRegistration,
    pub(crate) created_at: WorkspaceInstant,
    pub(crate) expires_at: WorkspaceInstant,
    pub(crate) renewed_at: Option<WorkspaceInstant>,
    pub(crate) released_at: Option<WorkspaceInstant>,
    pub(crate) revision: u64,
    pub(crate) cleanup: WorkspaceCleanupRecord,
    pub(crate) ttl: WorkspaceTtl,
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

    #[allow(dead_code)]
    pub(crate) fn project(&self) -> agent24_protocol::Workspace {
        agent24_protocol::Workspace {
            id: self.id.clone(),
            kind: self.kind.as_str().to_owned(),
            state: self.state.as_str().to_owned(),
            provenance: agent24_protocol::WorkspaceProvenance {
                source: self.authority.provenance_source.clone(),
                project_ref: self.authority.provenance_project_ref.clone(),
                base_revision: self.authority.provenance_base_revision.clone(),
            },
            writeback_policy: self.authority.writeback_policy.clone(),
            lifecycle_owner: agent24_protocol::LifecycleOwner {
                kind: self.authority.lifecycle_owner_kind.clone(),
                reference: self.authority.lifecycle_owner_ref.clone(),
            },
            concurrency_policy: self.authority.concurrency_policy.clone(),
            created_at: self.created_at.as_str().to_owned(),
            expires_at: self.expires_at.as_str().to_owned(),
            renewed_at: self.renewed_at.as_ref().map(|v| v.as_str().to_owned()),
            released_at: self.released_at.as_ref().map(|v| v.as_str().to_owned()),
            revision: self.revision,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn projection_redacts_root_generation_identity_and_cleanup() {
        let created_at = WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap();
        let expires_at = WorkspaceInstant::parse("2026-09-19T00:01:00.000Z").unwrap();
        let id = WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap();
        let row = WorkspaceRow {
            id: id.clone(),
            kind: WorkspaceKind::OrchestratorScratch,
            state: WorkspaceState::CleanupFailed,
            authority: WorkspaceAuthority {
                provenance_source: "git".into(),
                provenance_project_ref: Some("project".into()),
                provenance_base_revision: Some("base".into()),
                lifecycle_owner_kind: "orchestrator".into(),
                lifecycle_owner_ref: "owner".into(),
                writeback_policy: "external".into(),
                concurrency_policy: "serial".into(),
            },
            root: TrustedRootRegistration::new(
                "/secret/root".into(),
                "generation-1".into(),
                RootIdentity::unix(&[0; 8], &[1; 8]).unwrap(),
            )
            .unwrap(),
            created_at: created_at.clone(),
            expires_at: expires_at.clone(),
            renewed_at: None,
            released_at: None,
            revision: 1,
            cleanup: WorkspaceCleanupRecord {
                state: WorkspaceState::CleanupFailed,
                quarantined_at: None,
                quarantine_root: None,
                attempts: 1,
                last_attempt_at: Some(created_at.clone()),
                error: Some("secret cleanup".into()),
                retry_at: Some(expires_at.clone()),
            },
            ttl: WorkspaceTtl::new(60_000).unwrap(),
        };
        let public = row.project();
        assert_eq!(public.id, id);
        assert_eq!(public.kind, "orchestrator_scratch");
        assert_eq!(public.state, "cleanup_failed");
        assert_eq!(public.provenance.source, "git");
        assert_eq!(public.lifecycle_owner.reference, "owner");
        assert_eq!(public.created_at, created_at.as_str());
        assert_eq!(public.expires_at, expires_at.as_str());
        assert_eq!(public.revision, 1);
        let wire = serde_json::to_string(&public).unwrap();
        for secret in [
            "/secret/root",
            "generation-1",
            "secret cleanup",
            "canonical_root",
            "root_generation",
            "quarantine_root",
            "cleanup_error",
        ] {
            assert!(!wire.contains(secret), "projection leaked {secret}");
        }
    }
}
