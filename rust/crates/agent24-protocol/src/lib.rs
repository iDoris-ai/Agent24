//! Agent24 v1 protocol types.
//!
//! Locked to the machine-readable contract in `protocol/` (openapi.yaml +
//! events.schema.json) via fixture round-trip tests since B1; task B4 switches
//! generation so this crate becomes the upstream source with a CI zero-drift
//! check. Human-readable spec: `docs/specs/SPEC-002-protocol.md`.
//!
//! Wire conventions (SPEC-002 §0):
//! - snake_case fields; ids are ULID strings; timestamps ISO 8601 UTC strings
//! - nullable fields are ALWAYS present on the wire with value `null`
//!   (hence `Option<T>` without `skip_serializing_if`)
//! - open string enums stay `String` so unknown values never break decoding

pub mod events;
pub mod types;

pub use events::*;
pub use types::*;
