//! Extracting fields with their provenance (ADR-DOC-02 §3.2): what the
//! model is asked, in windows of the pinned text layer and batches of
//! fields, and how its answer is shaped. The model only names a block and
//! quotes it; the OS finds the quote and makes the anchor.

use serde::{Deserialize, Serialize};

pub mod ground;
pub mod handler;
pub mod job;
pub mod merge;
pub mod prompt;
pub mod run;
pub mod window;

/// The version of the instruction, the answer's schema, windows, batches,
/// where quotes are looked for, merging, and the sampling parameters
/// (§3.2): part of the business key, so a change re-extracts. 3: a quote
/// not in the block it names is looked for in the other blocks shown
/// (2026-10-15). 4: the model is asked at temperature 0 (2026-10-15). 5: a
/// call answers freely first, held to the schema only when that is not an
/// answer (ME4-S2c, 2026-10-17).
pub const EXTRACTOR_VERSION: &str = "5";

/// A requested field (`DocumentsExtractField`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub key: String,
    pub description: String,
    /// Absent or one of the contract's types; `null` is refused, not taken
    /// for absent.
    #[serde(
        rename = "type",
        default,
        deserialize_with = "crate::uploads::present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub kind: Option<String>,
}
