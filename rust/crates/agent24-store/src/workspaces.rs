use agent24_protocol::WorkspaceId;
use chrono::{DateTime, SecondsFormat, Utc};
use thiserror::Error;

const MAX_WORKSPACE_MS: i64 = 7 * 86_400_000;
const MAX_HOST_MS: i64 = 90_000;

pub type WorkspaceResult<T> = Result<T, WorkspaceStoreError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkspaceStoreError {
    #[error("corrupt {table} row: invalid {field}")]
    CorruptRow {
        table: &'static str,
        field: &'static str,
    },
    #[error("invalid workspace value: {field}")]
    InvalidValue { field: &'static str },
    #[error("workspace database error")]
    Database,
    #[error("workspace not found")]
    NotFound,
    #[error("workspace conflict: {0}")]
    Conflict(WorkspaceConflict),
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceConflict {
    #[error("identifier")]
    Identifier,
    #[error("allocation identifier")]
    AllocationIdentifier,
    #[error("allocation workspace")]
    AllocationWorkspace,
    #[error("allocation relative name")]
    AllocationRelativeName,
    #[error("canonical root")]
    CanonicalRoot,
    #[error("root identity")]
    RootIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkspaceInstant {
    text: String,
    epoch_millis: i64,
}

impl WorkspaceInstant {
    pub fn parse(text: &str) -> WorkspaceResult<Self> {
        if text.len() != 24 || !text.ends_with('Z') || text.get(17..19) == Some("60") {
            return Err(WorkspaceStoreError::InvalidValue { field: "timestamp" });
        }
        let parsed = DateTime::parse_from_rfc3339(text)
            .map_err(|_| WorkspaceStoreError::InvalidValue { field: "timestamp" })?;
        if parsed.to_rfc3339_opts(SecondsFormat::Millis, true) != text {
            return Err(WorkspaceStoreError::InvalidValue { field: "timestamp" });
        }
        Ok(Self {
            text: text.to_owned(),
            epoch_millis: parsed.timestamp_millis(),
        })
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    pub fn epoch_millis(&self) -> i64 {
        self.epoch_millis
    }

    pub fn checked_add_workspace_ttl(&self, ttl: WorkspaceTtl) -> WorkspaceResult<Self> {
        let millis = self.epoch_millis.checked_add(ttl.millis()).ok_or(
            WorkspaceStoreError::InvalidValue {
                field: "expires_at",
            },
        )?;
        let text = DateTime::<Utc>::from_timestamp_millis(millis)
            .ok_or(WorkspaceStoreError::InvalidValue {
                field: "expires_at",
            })?
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        Self::parse(&text).map_err(|_| WorkspaceStoreError::InvalidValue {
            field: "expires_at",
        })
    }
}

impl std::fmt::Display for WorkspaceInstant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceTtl(i64);
impl WorkspaceTtl {
    pub fn new(milliseconds: i64) -> WorkspaceResult<Self> {
        (1..=MAX_WORKSPACE_MS)
            .contains(&milliseconds)
            .then_some(Self(milliseconds))
            .ok_or(WorkspaceStoreError::InvalidValue {
                field: "workspace_ttl",
            })
    }
    pub fn millis(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostLeaseTtl(i64);
impl HostLeaseTtl {
    pub fn new(milliseconds: i64) -> WorkspaceResult<Self> {
        (1..=MAX_HOST_MS)
            .contains(&milliseconds)
            .then_some(Self(milliseconds))
            .ok_or(WorkspaceStoreError::InvalidValue {
                field: "host_lease_ttl",
            })
    }
    pub fn millis(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLeaseId(String);
impl WorkspaceLeaseId {
    pub fn parse(value: &str) -> WorkspaceResult<Self> {
        const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let b = value.as_bytes();
        if b.len() != 29
            || &b[..3] != b"wl_"
            || !matches!(b[3], b'0'..=b'7')
            || !b[4..].iter().all(|c| ALPHABET.contains(c))
        {
            return Err(WorkspaceStoreError::InvalidValue { field: "lease_id" });
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootIdentity {
    Unix {
        device: [u8; 8],
        inode: [u8; 8],
    },
    Windows {
        volume_serial: [u8; 8],
        file_id: [u8; 16],
    },
}

impl RootIdentity {
    pub fn unix(device: &[u8], inode: &[u8]) -> WorkspaceResult<Self> {
        Ok(Self::Unix {
            device: device
                .try_into()
                .map_err(|_| WorkspaceStoreError::InvalidValue {
                    field: "unix_device",
                })?,
            inode: inode
                .try_into()
                .map_err(|_| WorkspaceStoreError::InvalidValue {
                    field: "unix_inode",
                })?,
        })
    }
    pub fn windows(volume_serial: &[u8], file_id: &[u8]) -> WorkspaceResult<Self> {
        Ok(Self::Windows {
            volume_serial: volume_serial.try_into().map_err(|_| {
                WorkspaceStoreError::InvalidValue {
                    field: "windows_volume_serial",
                }
            })?,
            file_id: file_id
                .try_into()
                .map_err(|_| WorkspaceStoreError::InvalidValue {
                    field: "windows_file_id",
                })?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceKind {
    OrchestratorScratch,
    LegacyCompat,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceState {
    Active,
    Expired,
    Releasing,
    Released,
    CleanupFailed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseKind {
    Run,
    Host,
}

macro_rules! closed_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            pub fn parse(value: &str) -> WorkspaceResult<Self> {
                match value { $( $text => Ok(Self::$variant), )+ _ => Err(WorkspaceStoreError::InvalidValue { field: stringify!($name) }) }
            }
            pub fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $text, )+ }
            }
        }
    };
}
closed_enum!(WorkspaceKind { OrchestratorScratch => "orchestrator_scratch", LegacyCompat => "legacy_compat" });
closed_enum!(WorkspaceState { Active => "active", Expired => "expired", Releasing => "releasing", Released => "released", CleanupFailed => "cleanup_failed" });
closed_enum!(LeaseKind { Run => "run", Host => "host" });

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedRootRegistration {
    canonical_root: String,
    root_generation: String,
    identity: RootIdentity,
}
impl TrustedRootRegistration {
    pub fn new(
        canonical_root: String,
        root_generation: String,
        identity: RootIdentity,
    ) -> WorkspaceResult<Self> {
        if canonical_root.trim().is_empty() || canonical_root.contains('\0') {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "canonical_root",
            });
        }
        if root_generation.trim().is_empty() || root_generation.contains('\0') {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "root_generation",
            });
        }
        Ok(Self {
            canonical_root,
            root_generation,
            identity,
        })
    }
    pub fn canonical_root(&self) -> &str {
        &self.canonical_root
    }
    pub fn root_generation(&self) -> &str {
        &self.root_generation
    }
    pub fn identity(&self) -> RootIdentity {
        self.identity
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProvenanceInput {
    source: String,
    project_ref: Option<String>,
    base_revision: Option<String>,
}
impl WorkspaceProvenanceInput {
    pub fn new(
        source: String,
        project_ref: Option<String>,
        base_revision: Option<String>,
    ) -> WorkspaceResult<Self> {
        if source.trim().is_empty()
            || source.contains('\0')
            || project_ref.as_deref().is_some_and(|v| v.contains('\0'))
            || base_revision.as_deref().is_some_and(|v| v.contains('\0'))
        {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "provenance",
            });
        }
        Ok(Self {
            source,
            project_ref,
            base_revision,
        })
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn project_ref(&self) -> Option<&str> {
        self.project_ref.as_deref()
    }
    pub fn base_revision(&self) -> Option<&str> {
        self.base_revision.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleOwnerRef(String);
impl LifecycleOwnerRef {
    pub fn parse(value: String) -> WorkspaceResult<Self> {
        if value.trim().is_empty() || value.contains('\0') {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "lifecycle_owner_ref",
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewScratchWorkspace {
    id: WorkspaceId,
    root: TrustedRootRegistration,
    provenance: WorkspaceProvenanceInput,
    lifecycle_owner_ref: LifecycleOwnerRef,
    ttl: WorkspaceTtl,
}
impl NewScratchWorkspace {
    pub fn new(
        id: WorkspaceId,
        root: TrustedRootRegistration,
        provenance: WorkspaceProvenanceInput,
        lifecycle_owner_ref: LifecycleOwnerRef,
        ttl: WorkspaceTtl,
    ) -> Self {
        Self {
            id,
            root,
            provenance,
            lifecycle_owner_ref,
            ttl,
        }
    }
    pub fn id(&self) -> &WorkspaceId {
        &self.id
    }
    pub fn root(&self) -> &TrustedRootRegistration {
        &self.root
    }
    pub fn provenance(&self) -> &WorkspaceProvenanceInput {
        &self.provenance
    }
    pub fn lifecycle_owner_ref(&self) -> &LifecycleOwnerRef {
        &self.lifecycle_owner_ref
    }
    pub fn ttl(&self) -> WorkspaceTtl {
        self.ttl
    }
    pub fn kind(&self) -> WorkspaceKind {
        WorkspaceKind::OrchestratorScratch
    }
    pub fn state(&self) -> WorkspaceState {
        WorkspaceState::Active
    }
    pub fn revision(&self) -> u64 {
        1
    }
    pub fn writeback_policy(&self) -> &'static str {
        "external"
    }
    pub fn lifecycle_owner_kind(&self) -> &'static str {
        "orchestrator"
    }
    pub fn concurrency_policy(&self) -> &'static str {
        "serial"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceAuthority {
    pub(crate) provenance_source: String,
    pub(crate) provenance_project_ref: Option<String>,
    pub(crate) provenance_base_revision: Option<String>,
    pub(crate) lifecycle_owner_kind: String,
    pub(crate) lifecycle_owner_ref: String,
    pub(crate) writeback_policy: String,
    pub(crate) concurrency_policy: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCleanupRecord {
    pub(crate) state: WorkspaceState,
    pub(crate) quarantine_root: Option<String>,
    pub(crate) quarantined_at: Option<WorkspaceInstant>,
    pub(crate) attempts: u64,
    pub(crate) last_attempt_at: Option<WorkspaceInstant>,
    pub(crate) error: Option<String>,
    pub(crate) retry_at: Option<WorkspaceInstant>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLeaseRecord {
    pub(crate) id: WorkspaceLeaseId,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) root_generation: String,
    pub(crate) owner_id: String,
    pub(crate) kind: LeaseKind,
    pub(crate) daemon_generation: Option<String>,
    pub(crate) host_instance_id: Option<String>,
    pub(crate) acquired_at: WorkspaceInstant,
    pub(crate) expires_at: Option<WorkspaceInstant>,
    pub(crate) renewed_at: Option<WorkspaceInstant>,
    pub(crate) released_at: Option<WorkspaceInstant>,
}
