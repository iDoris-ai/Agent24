//! `capabilities.schema.json`: the `integration.agent24_knowledge` extension of
//! the `/system/capabilities` data layer (§9). Upstream keys around it pass
//! through, so only [`KnowledgeCapabilities`] and [`Limits`] are closed;
//! `uniqueItems` and string lengths stay with the schema.

use crate::common::{NonEmpty, PositiveInt};
use serde::{Deserialize, Serialize};

/// The data layer of `/system/capabilities`: upstream keys (`edition`,
/// `capabilities`, ...) are ignored on decode and not re-encoded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapabilitiesData {
    pub integration: Integration,
}

/// `integration`: other integrations' keys are ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Integration {
    pub agent24_knowledge: KnowledgeCapabilities,
}

/// `integration.agent24_knowledge` (R9.2–R9.4): exactly these five keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeCapabilities {
    pub protocol_versions: NonEmpty<PositiveInt>,
    pub mode: CapabilityMode,
    pub supported_operations: Vec<SupportedOperation>,
    pub server_version: String,
    pub limits: Limits,
}

/// `mode`: only `integration` is compatible (R9.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityMode {
    Integration,
    Standalone,
}

/// `supported_operations[]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportedOperation {
    Search,
    Read,
}

/// `limits`: server upper bounds that requests are clamped to (R3.3, R4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_results: PositiveInt,
    pub max_concurrency: PositiveInt,
    pub max_deadline_ms: PositiveInt,
    pub max_read_bytes: PositiveInt,
    pub max_page_limit: PositiveInt,
    pub max_snippet_bytes: PositiveInt,
}
