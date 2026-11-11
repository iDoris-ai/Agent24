//! Attached modules: the on-disk registry, the live registry, the handshake
//! listener, and the AgentEar timing bridge. Moved out of the daemon with no
//! behavior change. HTTP routes and `DaemonAttachGrantBuilder` stay in the
//! daemon; the registry asks an [`attach_registry::AttachGrantBuilder`] for
//! each admission.

pub mod agentear_timings;
pub mod attach_listener;
pub mod attach_registry;
pub mod attached;
