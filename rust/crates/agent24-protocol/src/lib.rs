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
pub mod state_file;
pub mod types;

pub use events::*;
pub use types::*;

// DEP-A1 反向验证临时测试：只在 macOS 上失败，确认 rust-macos job 会变红；验证后立即删除。
#[cfg(test)]
mod dep_a1_reverse_check {
    #[test]
    #[cfg(target_os = "macos")]
    fn dep_a1_force_fail_on_macos() {
        panic!("DEP-A1 reverse check: rust-macos job must turn red");
    }
}
