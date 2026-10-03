use std::collections::BTreeSet;
use std::time::Duration;

use super::MintedCapability;
use super::operations::Operation;
use super::store::CapabilityStore;
use super::token::random_capability_id;
use super::types::{
    Audience, Authorization, CapabilityClaims, CapabilityError, MAX_CREATIVE_TTL_SECONDS, Resource,
    UnixSeconds,
};

#[derive(Clone, Debug)]
pub struct CreativeMintRequest {
    pub workspace_id: String,
    pub attachment_id: String,
    pub principal_id: String,
    pub sidecar_generation: String,
    pub ttl: Duration,
    pub created_at: UnixSeconds,
    pub allowed_operations: BTreeSet<Operation>,
}

impl CreativeMintRequest {
    pub fn new(
        workspace_id: impl Into<String>,
        attachment_id: impl Into<String>,
        principal_id: impl Into<String>,
        sidecar_generation: impl Into<String>,
        ttl: Duration,
        created_at: UnixSeconds,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            attachment_id: attachment_id.into(),
            principal_id: principal_id.into(),
            sidecar_generation: sidecar_generation.into(),
            ttl,
            created_at,
            allowed_operations: Operation::creative_allowlist(),
        }
    }

    pub fn with_allowed_operations(
        mut self,
        operations: impl IntoIterator<Item = Operation>,
    ) -> Self {
        self.allowed_operations = operations.into_iter().collect();
        self
    }
}

impl CapabilityStore {
    pub fn mint_product_host(
        &self,
        host_generation: impl Into<String>,
        ttl: Duration,
        created_at: UnixSeconds,
    ) -> Result<MintedCapability, CapabilityError> {
        self.insert(CapabilityClaims {
            capability_id: random_capability_id(),
            audience: Audience::ProductHost,
            workspace_id: None,
            attachment_id: None,
            principal_id: None,
            daemon_generation: self.daemon_generation().to_owned(),
            host_generation: Some(host_generation.into()),
            sidecar_generation: None,
            created_at,
            expires_at: created_at.saturating_add(ttl.as_secs()),
            allowed_operations: BTreeSet::new(),
        })
    }

    pub fn mint_creative(
        &self,
        host_token: &super::CapabilityToken,
        request: CreativeMintRequest,
    ) -> Result<MintedCapability, CapabilityError> {
        let host = self.validate(
            host_token,
            Operation::ModelsRead,
            &Resource::global(),
            request.created_at,
        )?;
        self.mint_creative_authorized(&host, request)
    }

    pub fn mint_creative_authorized(
        &self,
        authorization: &Authorization,
        request: CreativeMintRequest,
    ) -> Result<MintedCapability, CapabilityError> {
        if request.ttl.is_zero() || request.ttl > Duration::from_secs(MAX_CREATIVE_TTL_SECONDS) {
            return Err(CapabilityError::InvalidTtl);
        }
        if authorization.claims.audience != Audience::ProductHost
            || authorization.claims.daemon_generation != *self.daemon_generation
            || request.created_at >= authorization.claims.expires_at
        {
            return Err(CapabilityError::HostRequired);
        }
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| CapabilityError::Unauthorized)?;
        let host_digest = state
            .ids
            .get(&authorization.claims.capability_id)
            .copied()
            .ok_or(CapabilityError::HostRequired)?;
        let host_generation = {
            let record = state
                .records
                .get(&host_digest)
                .ok_or(CapabilityError::HostRequired)?;
            if record.revoked || record.claims != authorization.claims {
                return Err(CapabilityError::HostRequired);
            }
            record.claims.host_generation.clone()
        };
        let allowed_operations = request
            .allowed_operations
            .intersection(&Operation::creative_allowlist())
            .copied()
            .collect();
        self.insert_locked(
            &mut state,
            CapabilityClaims {
                capability_id: random_capability_id(),
                audience: Audience::CreativeRuntime,
                workspace_id: Some(request.workspace_id),
                attachment_id: Some(request.attachment_id),
                principal_id: Some(request.principal_id),
                daemon_generation: self.daemon_generation().to_owned(),
                host_generation,
                sidecar_generation: Some(request.sidecar_generation),
                created_at: request.created_at,
                expires_at: request.created_at.saturating_add(request.ttl.as_secs()),
                allowed_operations,
            },
        )
    }
}
