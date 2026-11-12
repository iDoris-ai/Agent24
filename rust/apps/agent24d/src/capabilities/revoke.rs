use super::store::CapabilityStore;
use super::token::CapabilityToken;

/// What the single state-lock step of a bridge revoke decided.
pub(crate) enum RevokeAuditStart {
    /// Already revoked and the `Revoked` row was stored.
    Stored,
    /// Revoked in memory, `pending` set. The caller writes one row.
    Write,
}

impl CapabilityStore {
    pub fn epoch(&self) -> u64 {
        self.shared
            .state
            .lock()
            .map_or(u64::MAX, |state| state.epoch)
    }

    pub fn revoke(&self, token: &CapabilityToken) -> bool {
        let digest = super::token::digest(token);
        let Ok(mut state) = self.shared.state.lock() else {
            return false;
        };
        let Some(record) = state.records.get_mut(&digest) else {
            return false;
        };
        if record.revoked {
            return false;
        }
        record.revoked = true;
        state.epoch = state.epoch.wrapping_add(1);
        let _ = self.shared.changed.send(state.epoch);
        true
    }

    pub fn revoke_by_id(&self, capability_id: &str) -> bool {
        let Ok(mut state) = self.shared.state.lock() else {
            return false;
        };
        let Some(digest) = state.ids.get(capability_id).copied() else {
            return false;
        };
        let Some(record) = state.records.get_mut(&digest) else {
            return false;
        };
        if record.revoked {
            return false;
        }
        record.revoked = true;
        state.epoch = state.epoch.wrapping_add(1);
        let _ = self.shared.changed.send(state.epoch);
        true
    }

    /// Under the state lock: a revoked id whose audit row is already stored
    /// (`pending` clear) is [`RevokeAuditStart::Stored`]. Otherwise the id is
    /// marked revoked and `pending` is set before the caller writes the row.
    /// `None` is a missing id or a poisoned lock — not a stored row.
    pub(crate) fn start_revoke_audit(&self, capability_id: &str) -> Option<RevokeAuditStart> {
        let Ok(mut state) = self.shared.state.lock() else {
            return None;
        };
        let digest = state.ids.get(capability_id).copied()?;
        let fresh = {
            let record = state.records.get_mut(&digest)?;
            if record.revoked && !record.revoke_audit_pending {
                return Some(RevokeAuditStart::Stored);
            }
            let fresh = !record.revoked;
            record.revoked = true;
            record.revoke_audit_pending = true;
            fresh
        };
        if fresh {
            state.epoch = state.epoch.wrapping_add(1);
            let _ = self.shared.changed.send(state.epoch);
        }
        Some(RevokeAuditStart::Write)
    }

    pub(crate) fn revoke_audit_pending(&self, capability_id: &str) -> bool {
        let Ok(state) = self.shared.state.lock() else {
            return false;
        };
        state
            .ids
            .get(capability_id)
            .and_then(|digest| state.records.get(digest))
            .is_some_and(|record| record.revoke_audit_pending)
    }

    pub(crate) fn set_revoke_audit_pending(&self, capability_id: &str, pending: bool) {
        let Ok(mut state) = self.shared.state.lock() else {
            return;
        };
        let Some(digest) = state.ids.get(capability_id).copied() else {
            return;
        };
        if let Some(record) = state.records.get_mut(&digest) {
            record.revoke_audit_pending = pending;
        }
    }

    pub fn revoke_by_generation(&self, generation: &str) -> usize {
        self.revoke_where(|claims| {
            claims.daemon_generation == generation
                || claims.host_generation.as_deref() == Some(generation)
                || claims.sidecar_generation.as_deref() == Some(generation)
        })
    }

    pub fn revoke_by_daemon_generation(&self, generation: &str) -> usize {
        self.revoke_where(|claims| claims.daemon_generation == generation)
    }

    pub fn revoke_by_host_generation(&self, generation: &str) -> usize {
        self.revoke_where(|claims| claims.host_generation.as_deref() == Some(generation))
    }

    pub fn revoke_by_sidecar_generation(&self, generation: &str) -> usize {
        self.revoke_where(|claims| claims.sidecar_generation.as_deref() == Some(generation))
    }

    fn revoke_where<F>(&self, predicate: F) -> usize
    where
        F: Fn(&super::CapabilityClaims) -> bool,
    {
        let Ok(mut state) = self.shared.state.lock() else {
            return 0;
        };
        let mut count = 0;
        for record in state.records.values_mut() {
            if !record.revoked && predicate(&record.claims) {
                record.revoked = true;
                count += 1;
            }
        }
        if count != 0 {
            state.epoch = state.epoch.wrapping_add(1);
            let _ = self.shared.changed.send(state.epoch);
        }
        count
    }
}
