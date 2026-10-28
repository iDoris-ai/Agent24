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
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| CapabilityError::Unauthorized)?;
        let record = state
            .records
            .get(&digest)
            .ok_or(CapabilityError::Unauthorized)?;
        if !process_live(record) {
            let id = record.claims.capability_id.clone();
            state.records.remove(&digest);
            state.ids.remove(&id);
            return Err(CapabilityError::Unauthorized);
        }
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

    /// Bearer is known, unrevoked, unexpired, and for this daemon.
    /// Operation and resource are not checked: 404 and 405 must not reveal the
    /// route table to a missing, unknown, or revoked token.
    pub fn require_live_bearer(
        &self,
        bearer: &str,
        now: UnixSeconds,
    ) -> Result<(), CapabilityError> {
        let token = CapabilityToken::parse_bearer(bearer)?;
        let digest = digest(&token);
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| CapabilityError::Unauthorized)?;
        let record = state
            .records
            .get(&digest)
            .ok_or(CapabilityError::Unauthorized)?;
        if !process_live(record) {
            let id = record.claims.capability_id.clone();
            state.records.remove(&digest);
            state.ids.remove(&id);
            return Err(CapabilityError::Unauthorized);
        }
        let record = state
            .records
            .get(&digest)
            .ok_or(CapabilityError::Unauthorized)?;
        if record.revoked {
            return Err(CapabilityError::Revoked);
        }
        if record.claims.daemon_generation != *self.daemon_generation {
            return Err(CapabilityError::StaleGeneration);
        }
        if now >= record.claims.expires_at {
            return Err(CapabilityError::Expired);
        }
        Ok(())
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
        if record.claims.audience != super::Audience::ProductHost
            && !record.claims.allowed_operations.contains(&operation)
        {
            return Err(CapabilityError::OperationDenied);
        }
        if record.claims.audience != super::Audience::ProductHost {
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

fn process_live(record: &Record) -> bool {
    record.claims.first_party.as_ref().is_none_or(|claims| {
        let peer = &claims.peer;
        agent24_os_proc::is_alive(peer.pid)
            && agent24_os_proc::start_time(peer.pid)
                .is_ok_and(|s| s.sec == peer.start.sec && s.usec == peer.start.usec)
    })
}
