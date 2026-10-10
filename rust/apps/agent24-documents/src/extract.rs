//! Extracting fields with their provenance (ADR-DOC-02 §3.2): what the
//! model is asked, in windows of the pinned text layer and batches of
//! fields, and how its answer is shaped. The model only names a block and
//! quotes it; the OS finds the quote and makes the anchor.

use serde::{Deserialize, Serialize};

pub mod window;

/// The version of the instruction, the answer's schema, windows, batches
/// and merging (§3.2): part of the business key, so a change re-extracts.
pub const EXTRACTOR_VERSION: &str = "1";

/// A requested field (`DocumentsExtractField`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub key: String,
    pub description: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}
