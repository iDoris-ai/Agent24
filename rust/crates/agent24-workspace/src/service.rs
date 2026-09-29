//! Trusted composition of persisted workspace root evidence with pinned filesystem roots.

#![allow(dead_code)] // Dormant until the host admission/lease slice wires the service.

use agent24_store::{Store, WorkspaceInstant, WorkspaceLeaseId, WorkspaceStoreError};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

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
use agent24_store::RunWorkspaceAuthoritySnapshot;
#[cfg(unix)]
use std::fs::File;

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

/// Process-local pinned authority for one live workspace run.
#[doc(hidden)]
pub struct WorkspaceHandle {
    #[cfg(unix)]
    _pinned: PinnedAllocationRoot,
    #[cfg(unix)]
    _evidence: RunWorkspaceAuthoritySnapshot,
    #[cfg(unix)]
    canonical_root: PathBuf,
    #[cfg(not(unix))]
    _private: (),
}

/// Process-local reference to one run's workspace authority binding.
///
/// The reference deliberately stores no path/root material. Callers that need
/// filesystem authority must ask for a fresh handle, which revalidates the
/// persisted run/lease/workspace facts instead of treating an old pin as a
/// permanent capability.
#[doc(hidden)]
pub struct WorkspaceRunAuthority {
    service: Arc<WorkspaceService>,
    run_id: String,
    lease_id: WorkspaceLeaseId,
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
    #[doc(hidden)]
    pub fn compose(store: Store, state_dir: &Path) -> Result<Self> {
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

    /// Mint a process-local handle only after pinning and revalidating persisted authority.
    #[doc(hidden)]
    #[cfg(unix)]
    pub async fn open_run_handle(
        &self,
        run_id: &str,
        lease_id: &WorkspaceLeaseId,
    ) -> Result<WorkspaceHandle> {
        self.open_run_handle_inner(run_id, lease_id, store_now, || async {})
            .await
    }

    /// Bind an immutable run/lease identity only after validating it once.
    #[doc(hidden)]
    #[cfg(unix)]
    pub async fn bind_run_authority(
        self: &Arc<Self>,
        run_id: &str,
        lease_id: &WorkspaceLeaseId,
    ) -> Result<Arc<WorkspaceRunAuthority>> {
        let _ = self.open_run_handle(run_id, lease_id).await?;
        Ok(Arc::new(WorkspaceRunAuthority {
            service: Arc::clone(self),
            run_id: run_id.to_owned(),
            lease_id: lease_id.clone(),
        }))
    }

    #[cfg(unix)]
    async fn open_run_handle_inner<N, F, Fut>(
        &self,
        run_id: &str,
        lease_id: &WorkspaceLeaseId,
        mut now: N,
        between: F,
    ) -> Result<WorkspaceHandle>
    where
        N: FnMut() -> Result<WorkspaceInstant>,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let s1 = self
            .store
            .run_workspace_authority_snapshot(run_id, lease_id, &now()?)
            .await
            .map_err(map_store_error)?;
        let generation = s1.root_generation();
        if generation.contains('/') || generation.contains('\\') {
            return Err(unavailable("root_generation"));
        }
        let locator = format!("{}.{}", s1.workspace_id().as_str(), generation);
        if s1.relative_name() != locator {
            return Err(unavailable("root_locator_mismatch"));
        }
        let candidate = self.roots_path.join(&locator);
        let candidate = candidate
            .to_str()
            .ok_or_else(|| unavailable("canonical_root_mismatch"))?;
        if !s1.canonical_root_matches(candidate) {
            return Err(unavailable("canonical_root_mismatch"));
        }
        let intent = AllocationRootIntent::new(
            locator,
            generation.to_owned(),
            unix_identity(s1.parent_identity())?,
        )?;
        let pinned = PinnedAllocationRoot::reopen_exact(
            &self.roots,
            intent.clone(),
            unix_identity(s1.root_identity())?,
        )?;
        between().await;
        let s2 = self
            .store
            .run_workspace_authority_snapshot(run_id, lease_id, &now()?)
            .await
            .map_err(map_store_error)?;
        if s1 != s2 {
            return Err(unavailable("workspace_authority_changed"));
        }
        pinned.verify_binding(&self.roots, &intent, unix_identity(s2.root_identity())?)?;
        Ok(WorkspaceHandle {
            _pinned: pinned,
            _evidence: s2,
            canonical_root: candidate.into(),
        })
    }

    #[cfg(not(unix))]
    pub(crate) fn compose(_: Store, _: &Path) -> Result<Self> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    #[doc(hidden)]
    #[cfg(not(unix))]
    pub async fn open_run_handle(&self, _: &str, _: &WorkspaceLeaseId) -> Result<WorkspaceHandle> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    #[doc(hidden)]
    #[cfg(not(unix))]
    pub async fn bind_run_authority(
        self: &Arc<Self>,
        _: &str,
        _: &WorkspaceLeaseId,
    ) -> Result<Arc<WorkspaceRunAuthority>> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

impl WorkspaceRunAuthority {
    /// Revalidate the fixed run/lease binding and return a fresh pinned handle.
    #[doc(hidden)]
    pub async fn fresh_handle(&self) -> Result<WorkspaceHandle> {
        self.service
            .open_run_handle(&self.run_id, &self.lease_id)
            .await
    }

    #[doc(hidden)]
    pub async fn read_file(&self, raw: String, max_bytes: usize) -> Result<Vec<u8>> {
        let handle = self.fresh_handle().await?;
        tokio::task::spawn_blocking(move || handle.read_file(&raw, max_bytes))
            .await
            .map_err(|_| unavailable("workspace_read_task"))?
    }

    #[doc(hidden)]
    pub async fn write_file(&self, raw: String, bytes: Vec<u8>) -> Result<usize> {
        let handle = self.fresh_handle().await?;
        tokio::task::spawn_blocking(move || handle.write_file(&raw, &bytes))
            .await
            .map_err(|_| unavailable("workspace_write_task"))?
    }
}

impl WorkspaceHandle {
    #[cfg(unix)]
    fn read_file(&self, raw: &str, max_bytes: usize) -> Result<Vec<u8>> {
        use std::io::Read as _;

        let rel = self.relative_path(raw)?;
        let dir = cap_std::fs::Dir::from_std_file(self._pinned.try_clone_file()?);
        let file = dir.open(&rel).map_err(|_| unavailable("workspace_read"))?;
        let metadata = file
            .metadata()
            .map_err(|_| unavailable("workspace_read_metadata"))?;
        if !metadata.is_file() {
            return Err(WorkspaceError::InvalidSpec { field: "path" });
        }
        let mut bytes = Vec::new();
        file.take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable("workspace_read"))?;
        Ok(bytes)
    }

    #[cfg(unix)]
    fn write_file(&self, raw: &str, bytes: &[u8]) -> Result<usize> {
        use std::io::Write as _;

        let rel = self.relative_path(raw)?;
        let dir = cap_std::fs::Dir::from_std_file(self._pinned.try_clone_file()?);
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        let mut file = dir
            .open_with(&rel, &options)
            .map_err(|_| unavailable("workspace_write"))?;
        file.write_all(bytes)
            .map_err(|_| unavailable("workspace_write"))?;
        Ok(bytes.len())
    }

    #[cfg(unix)]
    fn relative_path(&self, raw: &str) -> Result<PathBuf> {
        let path = Path::new(raw);
        if !path.is_absolute() {
            return Err(WorkspaceError::InvalidSpec { field: "path" });
        }
        let normalized =
            lexical_normalize(path).ok_or(WorkspaceError::InvalidSpec { field: "path" })?;
        let relative = normalized
            .strip_prefix(&self.canonical_root)
            .map_err(|_| WorkspaceError::InvalidSpec { field: "path" })?;
        if relative.as_os_str().is_empty() {
            return Err(WorkspaceError::InvalidSpec { field: "path" });
        }
        Ok(relative.to_path_buf())
    }

    #[cfg(not(unix))]
    fn read_file(&self, _: &str, _: usize) -> Result<Vec<u8>> {
        Err(WorkspaceError::UnsupportedPlatform)
    }

    #[cfg(not(unix))]
    fn write_file(&self, _: &str, _: &[u8]) -> Result<usize> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

fn lexical_normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other),
        }
    }
    Some(out)
}

fn store_now() -> Result<WorkspaceInstant> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| WorkspaceError::RootUnavailable { reason: "clock" })?
        .as_millis();
    let millis =
        u64::try_from(millis).map_err(|_| WorkspaceError::RootUnavailable { reason: "clock" })?;
    store_instant_from_epoch_millis(millis)
}

fn store_instant_from_epoch_millis(millis: u64) -> Result<WorkspaceInstant> {
    let seconds = millis / 1000;
    let fraction = millis % 1000;
    let raw = agent24_core::util::iso8601_from_epoch_secs(seconds);
    let text = raw
        .strip_suffix('Z')
        .map(|prefix| format!("{prefix}.{fraction:03}Z"))
        .ok_or(WorkspaceError::RootUnavailable { reason: "clock" })?;
    WorkspaceInstant::parse(&text).map_err(|_| WorkspaceError::RootUnavailable { reason: "clock" })
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
        const LEASE: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
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

        async fn seed_run_authority(service: &WorkspaceService) -> WorkspaceLeaseId {
            let now = store_now().unwrap();
            let expires = now
                .checked_add_workspace_ttl(WorkspaceTtl::new(86_400_000).unwrap())
                .unwrap();
            query("UPDATE workspaces SET created_at=?, expires_at=?, renewed_at=NULL WHERE id=?")
                .bind(now.as_str())
                .bind(expires.as_str())
                .bind(ID)
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            query("UPDATE workspace_allocations SET created_at=? WHERE workspace_id=?")
                .bind(now.as_str())
                .bind(ID)
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            query("INSERT INTO sessions (id,title,channel,workspace_id,created_at,updated_at) VALUES ('s','s','desktop',?,?,?)")
                .bind(ID)
                .bind(now.as_str())
                .bind(now.as_str())
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at) VALUES ('r','s',?,'running',?,?,?)")
                .bind(ID)
                .bind(format!(r#"{{"prompt":"go","workspace_id":"{ID}","model_override":null,"mode":"normal"}}"#))
                .bind(r#"{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"cost_usd":0.0}"#)
                .bind(now.as_str())
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,?,'r','run',?)")
                .bind(LEASE)
                .bind(ID)
                .bind(GENERATION)
                .bind(now.as_str())
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            WorkspaceLeaseId::parse(LEASE).unwrap()
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

        #[tokio::test]
        async fn run_handle_pins_exact_managed_root() {
            let (_state, service) = fixture(None).await;
            let lease = seed_run_authority(&service).await;
            assert!(service.open_run_handle("r", &lease).await.is_ok());
        }

        #[tokio::test]
        async fn run_authority_revalidates_before_each_fresh_handle() {
            let (_state, service) = fixture(None).await;
            let service = Arc::new(service);
            let lease = seed_run_authority(&service).await;
            let authority = service.bind_run_authority("r", &lease).await.unwrap();
            assert!(authority.fresh_handle().await.is_ok());

            query("UPDATE workspace_leases SET released_at=? WHERE owner_id='r'")
                .bind(store_now().unwrap().as_str())
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            assert!(authority.fresh_handle().await.is_err());
        }

        #[tokio::test]
        async fn run_handle_io_stays_on_pinned_root_after_locator_replacement() {
            let (_state, service) = fixture(None).await;
            let lease = seed_run_authority(&service).await;
            let root = service.roots_path.join(format!("{ID}.{GENERATION}"));
            let file = root.join("note.txt");
            std::fs::write(&file, "old").unwrap();
            let handle = service.open_run_handle("r", &lease).await.unwrap();

            let displaced = service.roots_path.join("displaced-live-root");
            std::fs::rename(&root, &displaced).unwrap();
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(root.join("note.txt"), "replacement").unwrap();

            assert_eq!(
                handle.read_file(file.to_str().unwrap(), 16).unwrap(),
                b"old"
            );
            assert_eq!(
                handle
                    .write_file(file.to_str().unwrap(), b"pinned")
                    .unwrap(),
                6
            );
            assert_eq!(
                std::fs::read(displaced.join("note.txt")).unwrap(),
                b"pinned"
            );
            assert_eq!(
                std::fs::read(root.join("note.txt")).unwrap(),
                b"replacement"
            );
            assert!(handle.read_file("relative.txt", 16).is_err());
            assert!(handle.read_file("/definitely/outside.txt", 16).is_err());
        }

        #[tokio::test]
        async fn run_handle_rejects_wrong_candidate_and_root_identity() {
            let (_state, service) = fixture(Some("/tmp/not-the-managed-root")).await;
            let lease = seed_run_authority(&service).await;
            assert!(service.open_run_handle("r", &lease).await.is_err());

            let (_state, service) = fixture(None).await;
            let lease = seed_run_authority(&service).await;
            query("UPDATE workspaces SET unix_inode=X'0909090909090909' WHERE id=?")
                .bind(ID)
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            query("UPDATE workspace_allocations SET root_unix_inode=X'0909090909090909' WHERE workspace_id=?")
                .bind(ID)
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            assert!(service.open_run_handle("r", &lease).await.is_err());
        }

        #[tokio::test]
        async fn run_handle_revalidates_generation_and_root_after_pin() {
            for generation_change in [true, false] {
                let (_state, service) = fixture(None).await;
                let lease = seed_run_authority(&service).await;
                let store = service.store.clone();
                let result = service
                    .open_run_handle_inner("r", &lease, store_now, || async move {
                        let mut connection = test_hooks::pool(&store).acquire().await.unwrap();
                        if generation_change {
                            sqlx::query("PRAGMA foreign_keys=OFF")
                                .execute(&mut *connection)
                                .await
                                .unwrap();
                            for sql in [
                                "UPDATE workspaces SET root_generation='g2' WHERE id=?",
                                "UPDATE workspace_allocations SET root_generation='g2' WHERE workspace_id=?",
                                "UPDATE workspace_leases SET root_generation='g2' WHERE owner_id='r'",
                            ] {
                                query(sql).bind(ID).execute(&mut *connection).await.unwrap();
                            }
                            sqlx::query("PRAGMA foreign_keys=ON")
                                .execute(&mut *connection)
                                .await
                                .unwrap();
                        } else {
                            query("UPDATE workspaces SET unix_inode=X'0909090909090909' WHERE id=?")
                                .bind(ID)
                                .execute(&mut *connection)
                                .await
                                .unwrap();
                            query("UPDATE workspace_allocations SET root_unix_inode=X'0909090909090909' WHERE workspace_id=?")
                                .bind(ID)
                                .execute(&mut *connection)
                                .await
                                .unwrap();
                        }
                    })
                    .await;
                assert!(matches!(
                    result,
                    Err(WorkspaceError::RootUnavailable {
                        reason: "workspace_authority_changed"
                    })
                ));
            }
        }

        #[tokio::test]
        async fn fresh_s2_rejects_fractional_second_expiry() {
            let (_state, service) = fixture(None).await;
            let lease = seed_run_authority(&service).await;
            let s1 = WorkspaceInstant::parse("2026-09-19T00:00:00.100Z").unwrap();
            let expires = WorkspaceInstant::parse("2026-09-19T00:00:00.500Z").unwrap();
            let s2 = WorkspaceInstant::parse("2026-09-19T00:00:00.900Z").unwrap();

            query("UPDATE workspaces SET created_at=?, expires_at=?, renewed_at=NULL WHERE id=?")
                .bind(s1.as_str())
                .bind(expires.as_str())
                .bind(ID)
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            query("UPDATE workspace_allocations SET created_at=? WHERE workspace_id=?")
                .bind(s1.as_str())
                .bind(ID)
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            query("UPDATE sessions SET created_at=?, updated_at=? WHERE id='s'")
                .bind(s1.as_str())
                .bind(s1.as_str())
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            query("UPDATE runs SET created_at=? WHERE id='r'")
                .bind(s1.as_str())
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();
            query("UPDATE workspace_leases SET acquired_at=? WHERE owner_id='r'")
                .bind(s1.as_str())
                .execute(test_hooks::pool(&service.store))
                .await
                .unwrap();

            let mut times = [s1, s2].into_iter();
            let result = service
                .open_run_handle_inner(
                    "r",
                    &lease,
                    || Ok(times.next().expect("S1 and S2 clock reads")),
                    || async {},
                )
                .await;
            assert!(matches!(
                result,
                Err(WorkspaceError::RootUnavailable {
                    reason: "workspace_registry"
                })
            ));
        }

        #[test]
        fn epoch_millis_clock_keeps_fractional_precision() {
            assert_eq!(
                store_instant_from_epoch_millis(123).unwrap().as_str(),
                "1970-01-01T00:00:00.123Z"
            );
        }
    }
}
