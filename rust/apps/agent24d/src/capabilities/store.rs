use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::token::{CapabilityToken, MintedCapability, digest};
use super::types::CapabilityClaims;

/// Same capability id and deny reason write at most one audit row per window.
///
/// The key is `(capability_id, reason)`, not the route. Two routes that
/// refuse the same token for the same reason share one slot. The denial
/// row's operation is always `inbound.ingest` (the only profile this process
/// issues), so the route name is not a stored fact. Folding the route into
/// the key would repeat that same row once per path.
pub(crate) const DENY_AUDIT_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub(crate) struct Record {
    pub(crate) claims: CapabilityClaims,
    pub(crate) revoked: bool,
    /// Set before the `Revoked` row is written, and left set when that write
    /// fails. Cleared only after the row is stored. A caller that sees it
    /// set, or that cannot take the per-id gate, must not answer success.
    pub(crate) revoke_audit_pending: bool,
}

impl Record {
    pub(crate) fn expired(&self, now: super::types::UnixSeconds) -> bool {
        // Shell tokens stay on their own monotonic deadline. A wall-clock
        // jump must not expire them (`daily_token_ttl_ignores_wall_clock_jumps`).
        if let Some(claims) = self.claims.first_party.as_ref() {
            return tokio::time::Instant::now() >= claims.deadline;
        }
        // Bridge tokens die when either clock says so. `Instant` does not
        // move while macOS is asleep, so `expires_at` covers that gap. A
        // backward jump leaves `expires_at` in the future and the monotonic
        // deadline still fires.
        if let Some(deadline) = self.claims.deadline {
            return tokio::time::Instant::now() >= deadline || now >= self.claims.expires_at;
        }
        now >= self.claims.expires_at
    }
}

pub(crate) struct State {
    pub(crate) records: HashMap<[u8; 32], Record>,
    pub(crate) ids: HashMap<String, [u8; 32]>,
    pub(crate) epoch: u64,
}

pub(crate) struct Shared {
    pub(crate) state: Mutex<State>,
    pub(crate) changed: watch::Sender<u64>,
    pub(crate) deny_audit_at:
        Mutex<HashMap<(String, agent24_store::CapabilityDenyReason), tokio::time::Instant>>,
    /// One in-flight revoke audit per capability id. A second caller that
    /// finds the gate held does not answer success.
    pub(crate) revoke_gates: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}

/// In-memory capability authority. It stores only token digests and claims.
#[derive(Clone)]
pub struct CapabilityStore {
    pub(crate) daemon_generation: Arc<String>,
    pub(crate) shared: Arc<Shared>,
}

impl CapabilityStore {
    pub fn new(daemon_generation: impl Into<String>) -> Self {
        Self {
            daemon_generation: Arc::new(daemon_generation.into()),
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    records: HashMap::new(),
                    ids: HashMap::new(),
                    epoch: 0,
                }),
                changed: watch::channel(0).0,
                deny_audit_at: Mutex::new(HashMap::new()),
                revoke_gates: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The timestamp just stored when this id and reason should be written.
    /// A repeat inside [`DENY_AUDIT_INTERVAL`] returns `None`. A poisoned
    /// lock also returns `None` and logs a warning: the denial is not
    /// written. Returning `Some` here would append a row with no throttle
    /// record, so a poisoned lock would fail open.
    pub(crate) fn claim_deny_audit(
        &self,
        capability_id: &str,
        reason: agent24_store::CapabilityDenyReason,
    ) -> Option<tokio::time::Instant> {
        let Ok(mut slots) = self.shared.deny_audit_at.lock() else {
            tracing::warn!(
                capability_id,
                ?reason,
                "capability denial audit skipped; deny-audit lock is poisoned"
            );
            return None;
        };
        let now = tokio::time::Instant::now();
        let key = (capability_id.to_owned(), reason);
        let fresh = slots
            .get(&key)
            .is_none_or(|at| now.saturating_duration_since(*at) >= DENY_AUDIT_INTERVAL);
        if fresh {
            slots.insert(key, now);
            slots.retain(|_, at| now.saturating_duration_since(*at) < DENY_AUDIT_INTERVAL);
            Some(now)
        } else {
            None
        }
    }

    /// Drop a slot only when it is still the timestamp this caller stored.
    /// A newer claim from a write that succeeded stays in place.
    pub(crate) fn release_deny_audit(
        &self,
        capability_id: &str,
        reason: agent24_store::CapabilityDenyReason,
        claimed_at: tokio::time::Instant,
    ) {
        let Ok(mut slots) = self.shared.deny_audit_at.lock() else {
            return;
        };
        let key = (capability_id.to_owned(), reason);
        if slots.get(&key).is_some_and(|at| *at == claimed_at) {
            slots.remove(&key);
        }
    }

    pub fn daemon_generation(&self) -> &str {
        self.daemon_generation.as_str()
    }

    pub fn claims_by_id(&self, capability_id: &str) -> Option<super::CapabilityClaims> {
        let state = self.shared.state.lock().ok()?;
        let digest = *state.ids.get(capability_id)?;
        state
            .records
            .get(&digest)
            .map(|record| record.claims.clone())
    }

    pub(crate) fn insert(
        &self,
        claims: CapabilityClaims,
    ) -> Result<MintedCapability, super::types::CapabilityError> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| super::types::CapabilityError::Unauthorized)?;
        self.insert_locked(&mut state, claims)
    }

    pub(crate) fn insert_locked(
        &self,
        state: &mut State,
        claims: CapabilityClaims,
    ) -> Result<MintedCapability, super::types::CapabilityError> {
        let token = CapabilityToken::random();
        let digest = digest(&token);
        if state.ids.contains_key(&claims.capability_id) {
            return Err(super::types::CapabilityError::Unauthorized);
        }
        state.ids.insert(claims.capability_id.clone(), digest);
        state.records.insert(
            digest,
            Record {
                claims: claims.clone(),
                revoked: false,
                revoke_audit_pending: false,
            },
        );
        Ok(MintedCapability { token, claims })
    }
}
