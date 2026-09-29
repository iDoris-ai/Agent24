//! Trusted composition of persisted workspace root evidence with pinned filesystem roots.

#![allow(dead_code)] // Dormant until the host admission/lease slice wires the service.

use agent24_store::{Store, WorkspaceStoreError};
use std::path::Path;

use crate::WorkspaceError;

#[cfg(unix)]
use crate::allocation_root::{AllocationRootIntent, PinnedAllocationRoot};
#[cfg(unix)]
use crate::root::{ManagedParent, RootIdentity, WORKSPACE_ROOTS};
#[cfg(unix)]
use agent24_protocol::WorkspaceId;
#[cfg(unix)]
use agent24_store::RootIdentity as StoreRootIdentity;
#[cfg(unix)]
use std::{fs::File, path::PathBuf};

type Result<T> = std::result::Result<T, WorkspaceError>;

/// Workspace service boundary. Product-facing construction stays closed until
/// the later host admission slice establishes caller authority and leases.
pub struct WorkspaceService {
    #[cfg(unix)]
    store: Store,
    #[cfg(unix)]
    roots: ManagedParent,
    #[cfg(unix)]
    roots_path: PathBuf,
    #[cfg(not(unix))]
    _private: (),
}

/// Filesystem proof for one registered workspace root.
///
/// This value is crate-private and deliberately is not an admission token or a
/// Run `WorkspaceHandle`. Keeping the descriptor alive pins the object identity;
/// the path is retained only for the future trusted host handoff.
#[cfg(unix)]
pub(crate) struct ResolvedHostWorkspaceRoot {
    workspace_id: WorkspaceId,
    root_generation: String,
    canonical_root: PathBuf,
    pinned: PinnedAllocationRoot,
}

impl WorkspaceService {
    /// Compose the dormant service from a trusted, absolute Agent24 state dir.
    #[cfg(unix)]
    pub(crate) fn compose(store: Store, state_dir: &Path) -> Result<Self> {
        if !state_dir.is_absolute() {
            return Err(WorkspaceError::InvalidSpec { field: "state_dir" });
        }
        let state = File::open(state_dir).map_err(|_| WorkspaceError::RootUnavailable {
            reason: "state_dir_open",
        })?;
        let roots = ManagedParent::open_or_create(&state)?;
        Ok(Self {
            store,
            roots,
            roots_path: state_dir.join(WORKSPACE_ROOTS),
        })
    }

    /// Resolve persisted root evidence and reopen the exact managed directory.
    /// Lifecycle, TTL, capability, and lease admission intentionally remain out
    /// of this slice.
    #[cfg(unix)]
    pub(crate) async fn resolve_host_root(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Result<ResolvedHostWorkspaceRoot> {
        let snapshot = self
            .store
            .get_workspace_root_snapshot(workspace_id)
            .await
            .map_err(map_store_error)?;
        if snapshot.workspace_id() != workspace_id {
            return Err(unavailable("workspace_identity_mismatch"));
        }

        let generation = snapshot.root().root_generation();
        if generation.contains('/') || generation.contains('\\') {
            return Err(unavailable("root_generation"));
        }
        let locator = format!("{}.{}", workspace_id.as_str(), generation);
        if snapshot.relative_name() != locator {
            return Err(unavailable("root_locator_mismatch"));
        }

        let canonical_root = self.roots_path.join(&locator);
        if canonical_root.to_str() != Some(snapshot.root().canonical_root()) {
            return Err(unavailable("canonical_root_mismatch"));
        }

        let intent = AllocationRootIntent::new(
            locator,
            generation.to_owned(),
            unix_identity(snapshot.parent_identity())?,
        )?;
        let expected_root = unix_identity(snapshot.root().identity())?;
        let pinned = PinnedAllocationRoot::reopen_exact(&self.roots, intent, expected_root)?;

        Ok(ResolvedHostWorkspaceRoot {
            workspace_id: workspace_id.clone(),
            root_generation: generation.to_owned(),
            canonical_root,
            pinned,
        })
    }

    #[cfg(not(unix))]
    pub(crate) fn compose(_: Store, _: &Path) -> Result<Self> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

#[cfg(unix)]
impl ResolvedHostWorkspaceRoot {
    pub(crate) fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }

    pub(crate) fn root_generation(&self) -> &str {
        &self.root_generation
    }

    pub(crate) fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    pub(crate) fn pinned_identity(&self) -> RootIdentity {
        self.pinned.identity()
    }
}

fn map_store_error(error: WorkspaceStoreError) -> WorkspaceError {
    match error {
        WorkspaceStoreError::NotFound => unavailable("workspace_not_found"),
        _ => unavailable("workspace_registry"),
    }
}

fn unavailable(reason: &'static str) -> WorkspaceError {
    WorkspaceError::RootUnavailable { reason }
}

#[cfg(unix)]
fn unix_identity(identity: StoreRootIdentity) -> Result<RootIdentity> {
    match identity {
        StoreRootIdentity::Unix { device, inode } => Ok(RootIdentity {
            dev_le: device,
            ino_le: inode,
        }),
        StoreRootIdentity::Windows { .. } => Err(WorkspaceError::UnsupportedPlatform),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    #[cfg(unix)]
    mod unix {
        use super::super::*;
        use agent24_store::{
            AllocationId, LifecycleOwnerRef, NewScratchWorkspace,
            RootIdentity as StoreRootIdentity, TrustedRootRegistration, WorkspaceInstant,
            WorkspaceProvenanceInput, WorkspaceTtl, test_hooks,
        };
        use sqlx::query;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        const ALLOCATION: &str = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        const GENERATION: &str = "g1";
        const CREATED: &str = "2026-09-19T00:00:00.000Z";

        fn store_identity(identity: RootIdentity) -> StoreRootIdentity {
            StoreRootIdentity::unix(&identity.dev_le, &identity.ino_le).unwrap()
        }

        async fn fixture(
            canonical_override: Option<&str>,
        ) -> (tempfile::TempDir, WorkspaceService) {
            let state = tempfile::tempdir().unwrap();
            let store = Store::open_memory().await.unwrap();
            let service = WorkspaceService::compose(store.clone(), state.path()).unwrap();
            let workspace_id = WorkspaceId::parse(ID).unwrap();
            let locator = format!("{ID}.{GENERATION}");
            let pinned = service.roots.create_root(&locator).unwrap();
            let root_identity = store_identity(pinned.identity());
            let parent_identity = store_identity(service.roots.identity());
            let canonical = canonical_override.map_or_else(
                || {
                    service
                        .roots_path
                        .join(&locator)
                        .to_str()
                        .unwrap()
                        .to_owned()
                },
                str::to_owned,
            );
            let owner = LifecycleOwnerRef::parse("orchestrator-test".into()).unwrap();
            let input = NewScratchWorkspace::new(
                workspace_id.clone(),
                TrustedRootRegistration::new(canonical, GENERATION.into(), root_identity).unwrap(),
                WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
                owner.clone(),
                WorkspaceTtl::new(60_000).unwrap(),
            );
            store
                .create_workspace(&input, &owner, &WorkspaceInstant::parse(CREATED).unwrap())
                .await
                .unwrap();
            let (parent_kind, parent_device, parent_inode) = match parent_identity {
                StoreRootIdentity::Unix { device, inode } => ("unix", device, inode),
                StoreRootIdentity::Windows { .. } => unreachable!(),
            };
            let (root_kind, root_device, root_inode) = match root_identity {
                StoreRootIdentity::Unix { device, inode } => ("unix", device, inode),
                StoreRootIdentity::Windows { .. } => unreachable!(),
            };
            query(
                "INSERT INTO workspace_allocations
                 (allocation_id, workspace_id, root_generation, relative_name,
                  parent_identity_kind, parent_unix_device, parent_unix_inode,
                  root_identity_kind, root_unix_device, root_unix_inode,
                  phase, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'committed', ?)",
            )
            .bind(AllocationId::parse(ALLOCATION).unwrap().as_str())
            .bind(workspace_id.as_str())
            .bind(GENERATION)
            .bind(&locator)
            .bind(parent_kind)
            .bind(parent_device.to_vec())
            .bind(parent_inode.to_vec())
            .bind(root_kind)
            .bind(root_device.to_vec())
            .bind(root_inode.to_vec())
            .bind(CREATED)
            .execute(test_hooks::pool(&store))
            .await
            .unwrap();
            drop(pinned);
            (state, service)
        }

        #[tokio::test]
        async fn resolves_committed_workspace_to_exact_pinned_root() {
            let (_state, service) = fixture(None).await;
            let id = WorkspaceId::parse(ID).unwrap();
            let resolved = service.resolve_host_root(&id).await.unwrap();
            assert_eq!(resolved.workspace_id(), &id);
            assert_eq!(resolved.root_generation(), GENERATION);
            assert_eq!(
                resolved.canonical_root(),
                service.roots_path.join(format!("{ID}.{GENERATION}"))
            );
            let metadata = std::fs::metadata(resolved.canonical_root()).unwrap();
            assert_eq!(
                resolved.pinned_identity().dev_le,
                metadata.dev().to_le_bytes()
            );
            assert_eq!(
                resolved.pinned_identity().ino_le,
                metadata.ino().to_le_bytes()
            );
        }

        #[tokio::test]
        async fn rejects_missing_or_redirected_registry_root() {
            let state = tempfile::tempdir().unwrap();
            let service =
                WorkspaceService::compose(Store::open_memory().await.unwrap(), state.path())
                    .unwrap();
            let id = WorkspaceId::parse(ID).unwrap();
            assert!(matches!(
                service.resolve_host_root(&id).await,
                Err(WorkspaceError::RootUnavailable {
                    reason: "workspace_not_found"
                })
            ));

            let (_state, service) = fixture(Some("/tmp/not-the-managed-root")).await;
            assert!(matches!(
                service.resolve_host_root(&id).await,
                Err(WorkspaceError::RootUnavailable {
                    reason: "canonical_root_mismatch"
                })
            ));
        }

        #[tokio::test]
        async fn locator_replacement_cannot_reuse_persisted_root_identity() {
            let (_state, service) = fixture(None).await;
            let id = WorkspaceId::parse(ID).unwrap();
            let root = service.roots_path.join(format!("{ID}.{GENERATION}"));
            let displaced = service.roots_path.join("displaced-old-root");
            std::fs::rename(&root, &displaced).unwrap();
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(matches!(
                service.resolve_host_root(&id).await,
                Err(WorkspaceError::RootUnavailable {
                    reason: "root_identity_mismatch"
                })
            ));
        }
    }
}
