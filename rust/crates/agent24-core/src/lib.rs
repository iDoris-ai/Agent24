//! Agent24 domain core (C1 scope: state machines).
//!
//! DESIGN RULE (ADR-026): this crate depends only on `agent24-protocol` and
//! `thiserror` — never on axum, sqlx, Rig, tokio or any vendor SDK. Every
//! status transition in the system goes through these pure functions so the
//! legal state machines live in exactly one place.

pub mod transitions;
pub mod util;

pub use transitions::*;
