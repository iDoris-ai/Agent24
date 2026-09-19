//! Capability authority implementation.
//!
//! The public surface is re-exported here so callers continue to use
//! `crate::capabilities` while the implementation is organized by concern.

mod mint;
mod operations;
mod revoke;
mod store;
mod token;
mod types;
mod validate;
mod watch;

pub use mint::CreativeMintRequest;
pub use operations::{Operation, ResourceScope};
pub use store::CapabilityStore;
pub use token::{CapabilityToken, MintedCapability};
pub use types::{
    Audience, Authorization, CapabilityClaims, CapabilityError, MAX_CREATIVE_TTL_SECONDS, Resource,
    UnixSeconds, unix_now,
};
pub use watch::CapabilityWatch;

#[cfg(test)]
mod tests;
