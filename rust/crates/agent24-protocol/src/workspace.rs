use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
pub const MAX_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
pub const DEFAULT_TTL_SECONDS: u64 = 24 * 60 * 60;
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceValidationError {
    #[error("workspace id must be ws_<26 uppercase Crockford base32 characters>")]
    InvalidId,
    #[error("workspace provenance source must not be empty")]
    EmptySource,
    #[error("workspace lifecycle owner reference must not be empty")]
    EmptyOwnerReference,
    #[error("workspace timestamps must be RFC3339")]
    InvalidTimestamp,
    #[error("workspace TTL must be positive and no greater than seven days")]
    InvalidTtl,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    pub fn parse(value: impl Into<String>) -> Result<Self, WorkspaceValidationError> {
        let value = value.into();
        let suffix = value
            .strip_prefix("ws_")
            .ok_or(WorkspaceValidationError::InvalidId)?;
        if suffix.len() != 26
            || !suffix
                .bytes()
                .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte))
        {
            return Err(WorkspaceValidationError::InvalidId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl Serialize for WorkspaceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for WorkspaceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    OrchestratorScratch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    Active,
    Expired,
    Releasing,
    Released,
    CleanupFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProvenance {
    pub source: String,
    pub project_ref: Option<String>,
    pub base_revision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleOwnerKind {
    Orchestrator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleOwner {
    pub kind: LifecycleOwnerKind,
    pub reference: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritebackPolicy {
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    Serial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Workspace {
    pub id: WorkspaceId,
    pub kind: WorkspaceKind,
    pub state: WorkspaceState,
    pub provenance: WorkspaceProvenance,
    pub writeback_policy: WritebackPolicy,
    pub lifecycle_owner: LifecycleOwner,
    pub concurrency_policy: ConcurrencyPolicy,
    pub created_at: String,
    pub expires_at: String,
    pub renewed_at: Option<String>,
    pub released_at: Option<String>,
    pub revision: u64,
}

impl Workspace {
    pub fn validate(&self) -> Result<(), WorkspaceValidationError> {
        if self.provenance.source.trim().is_empty() {
            return Err(WorkspaceValidationError::EmptySource);
        }
        if self.lifecycle_owner.reference.trim().is_empty() {
            return Err(WorkspaceValidationError::EmptyOwnerReference);
        }
        let created = parse_timestamp(&self.created_at)?;
        let expires = parse_timestamp(&self.expires_at)?;
        let ttl = (expires - created).num_seconds();
        if ttl <= 0 || ttl as u64 > MAX_TTL_SECONDS {
            return Err(WorkspaceValidationError::InvalidTtl);
        }
        for timestamp in [&self.renewed_at, &self.released_at].into_iter().flatten() {
            parse_timestamp(timestamp)?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceWire {
    id: WorkspaceId,
    kind: WorkspaceKind,
    state: WorkspaceState,
    provenance: WorkspaceProvenance,
    writeback_policy: WritebackPolicy,
    lifecycle_owner: LifecycleOwner,
    concurrency_policy: ConcurrencyPolicy,
    created_at: String,
    expires_at: String,
    renewed_at: Option<String>,
    released_at: Option<String>,
    revision: u64,
}

impl<'de> Deserialize<'de> for Workspace {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WorkspaceWire::deserialize(deserializer)?;
        let workspace = Self {
            id: wire.id,
            kind: wire.kind,
            state: wire.state,
            provenance: wire.provenance,
            writeback_policy: wire.writeback_policy,
            lifecycle_owner: wire.lifecycle_owner,
            concurrency_policy: wire.concurrency_policy,
            created_at: wire.created_at,
            expires_at: wire.expires_at,
            renewed_at: wire.renewed_at,
            released_at: wire.released_at,
            revision: wire.revision,
        };
        workspace.validate().map_err(serde::de::Error::custom)?;
        Ok(workspace)
    }
}

fn parse_timestamp(value: &str) -> Result<DateTime<FixedOffset>, WorkspaceValidationError> {
    DateTime::parse_from_rfc3339(value).map_err(|_| WorkspaceValidationError::InvalidTimestamp)
}
