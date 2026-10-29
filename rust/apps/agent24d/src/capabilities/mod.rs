//! Capability authority implementation.
//!
//! The public surface is re-exported here so callers continue to use
//! `crate::capabilities` while the implementation is organized by concern.

pub mod delegation;
pub mod first_party;
mod mint;
mod operations;
pub mod presence;
mod revoke;
mod route_policy;
mod store;
mod token;
mod types;
mod validate;
mod watch;

pub use mint::CreativeMintRequest;
pub use operations::{Operation, ResourceScope};
pub use route_policy::{
    AssuranceLevel, RouteDecision, RoutePolicy, creative_route_permitted, decide, policies,
    required_operation,
};
pub use store::CapabilityStore;
pub use token::{CapabilityToken, MintedCapability};
pub use types::{
    Audience, Authorization, CapabilityClaims, CapabilityError, MAX_CREATIVE_TTL_SECONDS, Resource,
    UnixSeconds, unix_now,
};
pub use watch::CapabilityWatch;

#[cfg(test)]
mod tests;
