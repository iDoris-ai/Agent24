//! Host-owned outbound decision contract (ADR-K1-02 §2.4).
use std::sync::Arc;

use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressPurpose {
    ModelInference,
    ModelDiscovery,
    HttpFetch,
    McpTool,
    ModuleTool,
    ProcessExecution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressDestination(Option<String>);

impl EgressDestination {
    #[must_use]
    pub fn exact(id: impl Into<String>) -> Self {
        Self(Some(id.into()))
    }
    #[must_use]
    pub fn unknown() -> Self {
        Self(None)
    }
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressResource {
    pub resource_id: String,
    pub revision: Option<String>,
    pub local_only: bool,
    pub authorization_ref: Option<String>,
    pub policy_version: u64,
}

impl EgressResource {
    #[must_use]
    pub fn local_only(id: impl Into<String>, revision: impl Into<String>) -> Self {
        Self {
            resource_id: id.into(),
            revision: Some(revision.into()),
            local_only: true,
            authorization_ref: None,
            policy_version: 0,
        }
    }
    #[must_use]
    pub fn cloud_authorized(
        id: impl Into<String>,
        revision: impl Into<String>,
        policy_version: u64,
    ) -> Self {
        Self {
            resource_id: id.into(),
            revision: Some(revision.into()),
            local_only: false,
            authorization_ref: None,
            policy_version,
        }
    }

    #[must_use]
    pub fn with_authorization_ref(mut self, reference: impl Into<String>) -> Self {
        self.authorization_ref = Some(reference.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRequest {
    pub resources: Vec<EgressResource>,
    pub purpose: EgressPurpose,
    pub destination: EgressDestination,
    pub authorization_generation: u64,
    pub remote: bool,
}

impl EgressRequest {
    #[must_use]
    pub fn remote(
        resources: Vec<EgressResource>,
        purpose: EgressPurpose,
        destination: EgressDestination,
        authorization_generation: u64,
    ) -> Self {
        Self {
            resources,
            purpose,
            destination,
            authorization_generation,
            remote: true,
        }
    }

    /// Enforce invariant checks before consulting the host's live grant service.
    /// In particular, LocalOnly, missing provenance, and unknown destinations
    /// cannot be overridden by a permissive host implementation.
    pub async fn authorize(&self, gate: &dyn EgressGate) -> Result<(), EgressDecision> {
        if !self.remote
            || self.destination.id().is_none_or(str::is_empty)
            || self.resources.is_empty()
            || self.resources.iter().any(|resource| {
                resource.local_only
                    || resource.revision.as_deref().is_none_or(str::is_empty)
                    || resource
                        .authorization_ref
                        .as_deref()
                        .is_none_or(str::is_empty)
                    || resource.policy_version == 0
            })
        {
            return Err(EgressDecision);
        }
        gate.check(self).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("outbound request denied by host policy")]
pub struct EgressDecision;

#[async_trait]
pub trait EgressGate: Send + Sync {
    /// Re-evaluated at the actual outbound boundary; implementations must not cache grants.
    async fn check(&self, request: &EgressRequest) -> Result<(), EgressDecision>;
}

#[derive(Debug, Default)]
pub struct DenyAllEgress;

#[async_trait]
impl EgressGate for DenyAllEgress {
    async fn check(&self, _request: &EgressRequest) -> Result<(), EgressDecision> {
        Err(EgressDecision)
    }
}

impl dyn EgressGate {
    #[must_use]
    pub fn deny_all() -> Arc<dyn EgressGate> {
        Arc::new(DenyAllEgress)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct ExactGrant {
        resource: &'static str,
        revision: &'static str,
        authorization_ref: &'static str,
        policy_version: u64,
        purpose: EgressPurpose,
        destination: &'static str,
        generation: u64,
    }

    #[async_trait]
    impl EgressGate for ExactGrant {
        async fn check(&self, request: &EgressRequest) -> Result<(), EgressDecision> {
            if request.resources.len() == 1
                && request.resources[0].resource_id == self.resource
                && request.resources[0].revision.as_deref() == Some(self.revision)
                && request.resources[0].authorization_ref.as_deref() == Some(self.authorization_ref)
                && request.resources[0].policy_version == self.policy_version
                && request.purpose == self.purpose
                && request.destination.id() == Some(self.destination)
                && request.authorization_generation == self.generation
            {
                Ok(())
            } else {
                Err(EgressDecision)
            }
        }
    }

    #[tokio::test]
    async fn default_policy_rejects_remote_and_unknown_destinations() {
        let request = EgressRequest::remote(
            vec![EgressResource::local_only(
                "selected_material:payroll",
                "rev-1",
            )],
            EgressPurpose::ModelInference,
            EgressDestination::exact("provider:unknown"),
            7,
        );
        assert!(request.authorize(&DenyAllEgress).await.is_err());
    }

    #[tokio::test]
    async fn cloud_grant_must_match_resource_purpose_destination_and_generation() {
        let request = EgressRequest::remote(
            vec![
                EgressResource::cloud_authorized("selected_material:report", "sha256:abc", 3)
                    .with_authorization_ref("grant-1"),
            ],
            EgressPurpose::ModelInference,
            EgressDestination::exact("provider:acme"),
            3,
        );
        let grant = ExactGrant {
            resource: "selected_material:report",
            revision: "sha256:abc",
            authorization_ref: "grant-1",
            policy_version: 3,
            purpose: EgressPurpose::ModelInference,
            destination: "provider:acme",
            generation: 3,
        };
        assert!(request.authorize(&grant).await.is_ok());

        let mut changed = request.clone();
        changed.destination = EgressDestination::exact("provider:other");
        assert!(changed.authorize(&grant).await.is_err());
        let mut changed = request.clone();
        changed.resources[0].revision = Some("sha256:other".into());
        assert!(changed.authorize(&grant).await.is_err());
        let mut changed = request.clone();
        changed.authorization_generation = 4;
        assert!(changed.authorize(&grant).await.is_err());
        let mut changed = request.clone();
        changed.purpose = EgressPurpose::HttpFetch;
        assert!(changed.authorize(&grant).await.is_err());
    }

    #[tokio::test]
    async fn a_host_cannot_override_local_only_or_unknown_destination() {
        let grant = ExactGrant {
            resource: "selected_material:payroll",
            revision: "rev-1",
            authorization_ref: "grant",
            policy_version: 2,
            purpose: EgressPurpose::ModelInference,
            destination: "provider:acme",
            generation: 7,
        };
        let local = EgressRequest::remote(
            vec![EgressResource::local_only(
                "selected_material:payroll",
                "rev-1",
            )],
            EgressPurpose::ModelInference,
            EgressDestination::exact("provider:acme"),
            7,
        );
        assert!(local.authorize(&grant).await.is_err());
        let unknown = EgressRequest::remote(
            vec![
                EgressResource::cloud_authorized("selected_material:payroll", "rev-1", 2)
                    .with_authorization_ref("grant"),
            ],
            EgressPurpose::ModelInference,
            EgressDestination::unknown(),
            7,
        );
        assert!(unknown.authorize(&grant).await.is_err());
    }

    struct RevocableGrant(AtomicBool);

    #[async_trait]
    impl EgressGate for RevocableGrant {
        async fn check(&self, _request: &EgressRequest) -> Result<(), EgressDecision> {
            self.0
                .load(Ordering::SeqCst)
                .then_some(())
                .ok_or(EgressDecision)
        }
    }

    #[tokio::test]
    async fn a_queued_authorization_is_rechecked_after_revocation_or_expiry() {
        let grant = RevocableGrant(AtomicBool::new(true));
        let request = EgressRequest::remote(
            vec![
                EgressResource::cloud_authorized("doc:a", "rev-1", 2)
                    .with_authorization_ref("grant-a"),
            ],
            EgressPurpose::ModelInference,
            EgressDestination::exact("provider:a"),
            9,
        );
        assert!(request.authorize(&grant).await.is_ok());
        grant.0.store(false, Ordering::SeqCst);
        assert!(request.authorize(&grant).await.is_err());
    }
}
