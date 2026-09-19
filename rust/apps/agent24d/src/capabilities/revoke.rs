use super::store::CapabilityStore;
use super::token::CapabilityToken;

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
