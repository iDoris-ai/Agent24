use super::*;
use std::time::Duration;

pub(super) fn host(store: &CapabilityStore) -> MintedCapability {
    store
        .mint_product_host("host-1", Duration::from_secs(300), 100)
        .unwrap()
}

pub(super) fn creative(store: &CapabilityStore, host: &MintedCapability) -> MintedCapability {
    store
        .mint_creative(
            host.token(),
            CreativeMintRequest::new(
                "ws-a",
                "att-a",
                "principal-a",
                "sidecar-1",
                Duration::from_secs(100),
                100,
            ),
        )
        .unwrap()
}

mod authority;
mod lifecycle;
mod token;
