use std::collections::BTreeSet;
use std::fmt;

pub type UnixSeconds = u64;
pub const MAX_CREATIVE_TTL_SECONDS: u64 = 3_600;

pub fn unix_now() -> UnixSeconds {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Audience {
    ProductHost,
    CreativeRuntime,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Resource {
    pub workspace_id: Option<String>,
    pub attachment_id: Option<String>,
    pub principal_id: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
}

impl Resource {
    pub fn global() -> Self {
        Self::default()
    }

    pub fn workspace(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: Some(workspace_id.into()),
            ..Self::default()
        }
    }

    pub fn attachment(workspace_id: impl Into<String>, attachment_id: impl Into<String>) -> Self {
        Self {
            workspace_id: Some(workspace_id.into()),
            attachment_id: Some(attachment_id.into()),
            ..Self::default()
        }
    }

    pub fn principal(
        workspace_id: impl Into<String>,
        attachment_id: impl Into<String>,
        principal_id: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id: Some(workspace_id.into()),
            attachment_id: Some(attachment_id.into()),
            principal_id: Some(principal_id.into()),
            ..Self::default()
        }
    }

    pub fn session(
        workspace_id: impl Into<String>,
        attachment_id: impl Into<String>,
        principal_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id: Some(workspace_id.into()),
            attachment_id: Some(attachment_id.into()),
            principal_id: Some(principal_id.into()),
            session_id: Some(session_id.into()),
            ..Self::default()
        }
    }

    pub fn run(
        workspace_id: impl Into<String>,
        attachment_id: impl Into<String>,
        principal_id: impl Into<String>,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id: Some(workspace_id.into()),
            attachment_id: Some(attachment_id.into()),
            principal_id: Some(principal_id.into()),
            session_id: Some(session_id.into()),
            run_id: Some(run_id.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityClaims {
    pub capability_id: String,
    pub audience: Audience,
    pub workspace_id: Option<String>,
    pub attachment_id: Option<String>,
    pub principal_id: Option<String>,
    pub daemon_generation: String,
    pub host_generation: Option<String>,
    pub sidecar_generation: Option<String>,
    pub created_at: UnixSeconds,
    pub expires_at: UnixSeconds,
    pub allowed_operations: BTreeSet<super::operations::Operation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Authorization {
    pub claims: CapabilityClaims,
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    Unauthorized,
    Expired,
    Revoked,
    StaleGeneration,
    OperationDenied,
    ResourceDenied,
    HostRequired,
    InvalidTtl,
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unauthorized")
    }
}

impl std::error::Error for CapabilityError {}
