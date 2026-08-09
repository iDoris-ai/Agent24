//! Sin90 — Agent24's built-in Personal-OS domain (pure core).
//!
//! DESIGN RULE (ADR-026, mirrors `agent24-core`): this crate is pure domain —
//! entity types, the legal state machines, and Proposal validation. It depends
//! only on `serde` + `thiserror`, never on sqlx/tokio/axum/any vendor SDK.
//! Persistence lives in `agent24-sin90-store` (its own `sin90.db`); the daemon
//! mounts Sin90 as a loadable module. The dependency arrow points ONE way:
//! Sin90 → kernel, never the reverse (SIN90-domain.md §0).
//!
//! Three parts, same shape as the kernel's core:
//! - [`types`] — entities + snake_case status enums (wire shapes).
//! - [`transitions`] — the ONLY legal Personal-OS state machines.
//! - [`proposal`] — the "AI does not write the DB" validation gate.

pub mod proposal;
pub mod transitions;
pub mod types;

pub use proposal::*;
pub use transitions::*;
pub use types::*;
