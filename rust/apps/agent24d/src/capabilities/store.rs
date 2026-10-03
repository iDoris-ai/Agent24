use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use super::token::{CapabilityToken, MintedCapability, digest};
use super::types::CapabilityClaims;

#[derive(Clone)]
pub(crate) struct Record {
    pub(crate) claims: CapabilityClaims,
    pub(crate) revoked: bool,
}

pub(crate) struct State {
    pub(crate) records: HashMap<[u8; 32], Record>,
    pub(crate) ids: HashMap<String, [u8; 32]>,
    pub(crate) epoch: u64,
}

pub(crate) struct Shared {
    pub(crate) state: Mutex<State>,
    pub(crate) changed: watch::Sender<u64>,
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
            }),
        }
    }

    pub fn daemon_generation(&self) -> &str {
        self.daemon_generation.as_str()
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
            },
        );
        Ok(MintedCapability { token, claims })
    }
}
