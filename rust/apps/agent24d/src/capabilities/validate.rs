use super::operations::{Operation, ResourceScope};
use super::store::{CapabilityStore, Record};
use super::token::{CapabilityToken, digest};
use super::types::{Authorization, CapabilityError, Resource, UnixSeconds};

impl CapabilityStore {
    pub fn validate(
        &self,
        token: &CapabilityToken,
        operation: Operation,
        resource: &Resource,
        now: UnixSeconds,
    ) -> Result<Authorization, CapabilityError> {
        let digest = digest(token);
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| CapabilityError::Unauthorized)?;
        let record = state
            .records
            .get(&digest)
            .ok_or(CapabilityError::Unauthorized)?;
        self.validate_record(record, operation, resource, now, state.epoch)?;
        Ok(Authorization {
            claims: record.claims.clone(),
            epoch: state.epoch,
        })
    }

    pub fn validate_bearer(
        &self,
        bearer: &str,
        operation: Operation,
        resource: &Resource,
        now: UnixSeconds,
    ) -> Result<Authorization, CapabilityError> {
        let token = CapabilityToken::parse_bearer(bearer)?;
        self.validate(&token, operation, resource, now)
    }

    pub fn validate_action(
        &self,
        token: &CapabilityToken,
        action: &str,
        resource: &Resource,
        now: UnixSeconds,
    ) -> Result<Authorization, CapabilityError> {
        let operation = Operation::from_action(action).ok_or(CapabilityError::OperationDenied)?;
        self.validate(token, operation, resource, now)
    }

    pub fn revalidate(
        &self,
        token: &CapabilityToken,
        operation: Operation,
        resource: &Resource,
        now: UnixSeconds,
    ) -> Result<Authorization, CapabilityError> {
        self.validate(token, operation, resource, now)
    }

    pub fn revalidate_bearer(
        &self,
        bearer: &str,
        operation: Operation,
        resource: &Resource,
        now: UnixSeconds,
    ) -> Result<Authorization, CapabilityError> {
        self.validate_bearer(bearer, operation, resource, now)
    }

    pub(crate) fn validate_record(
        &self,
        record: &Record,
        operation: Operation,
        resource: &Resource,
        now: UnixSeconds,
        _epoch: u64,
    ) -> Result<(), CapabilityError> {
        if record.revoked {
            return Err(CapabilityError::Revoked);
        }
        if record.claims.daemon_generation != *self.daemon_generation {
            return Err(CapabilityError::StaleGeneration);
        }
        if now >= record.claims.expires_at {
            return Err(CapabilityError::Expired);
        }
        if record.claims.audience == super::Audience::CreativeRuntime
            && !record.claims.allowed_operations.contains(&operation)
        {
            return Err(CapabilityError::OperationDenied);
        }
        if record.claims.audience == super::Audience::CreativeRuntime {
            let exact = match operation.scope() {
                ResourceScope::Global => resource == &Resource::global(),
                ResourceScope::Principal => {
                    resource.workspace_id.as_deref() == record.claims.workspace_id.as_deref()
                        && resource.attachment_id.as_deref()
                            == record.claims.attachment_id.as_deref()
                        && resource.principal_id.as_deref() == record.claims.principal_id.as_deref()
                }
            };
            if !exact {
                return Err(CapabilityError::ResourceDenied);
            }
        }
        Ok(())
    }
}
