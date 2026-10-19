//! `search.schema.json`: `knowledge.search` request and success response (§3).
//! Errors use [`crate::ErrorEnvelope`]; `partial` is a success (R6.5).

use crate::common::{
    ChunkRevision, ChunkType, Display, EffectiveScope, Generation, NonEmpty, PartialFailure,
    PositiveInt, ProtocolVersion, RequestScope,
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

/// `budget` and `applied_budget` (R3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub deadline_ms: PositiveInt,
    pub max_results: PositiveInt,
    pub max_concurrency: PositiveInt,
}

/// `$defs/request`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub protocol_version: ProtocolVersion,
    pub request_id: String,
    pub generation: Generation,
    pub query: String,
    pub scope: RequestScope,
    pub budget: Budget,
}

/// `completeness` (R3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Partial,
}

/// `$defs/response`. `partial_failures` present iff `partial` is left to the schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchResponse {
    pub protocol_version: ProtocolVersion,
    pub request_id: String,
    pub generation: Generation,
    pub completeness: Completeness,
    pub effective_scope: EffectiveScope,
    pub applied_budget: Budget,
    pub hits: Vec<SearchHit>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub partial_failures: Option<NonEmpty<PartialFailure>>,
}

/// `hit.index_state` (R3.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    Fresh,
    Stale,
    Unknown,
}

/// `$defs/hit` (R3.8). `score` is comparable only within one response (R3.6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    pub rank: PositiveInt,
    pub knowledge_base_id: String,
    pub knowledge_id: String,
    pub chunk_id: String,
    pub chunk_type: ChunkType,
    pub revision: HitRevision,
    pub locator: Locator,
    pub index_state: IndexState,
    pub score: f64,
    pub score_space: String,
    pub snippet: Snippet,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub display: Option<Display>,
}

/// `hit.revision`: `chunk_revision` is `null` when unknown (R3.9).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitRevision {
    pub source_revision: String,
    pub index_build_id: String,
    #[serde(deserialize_with = "crate::nullable")]
    pub chunk_revision: Option<ChunkRevision>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snippet {
    pub text: String,
    pub truncated: bool,
}

/// `$defs/locator` (R3.10): offsets only, and always, with `valid: true`.
/// `end >= 1` is in the type; `end > start` is checked in code, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawLocator", into = "RawLocator")]
pub enum Locator {
    Valid { start: u64, end: NonZeroU64 },
    Invalid,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocator {
    valid: bool,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    start: Option<u64>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    end: Option<NonZeroU64>,
}

impl TryFrom<RawLocator> for Locator {
    type Error = &'static str;
    fn try_from(r: RawLocator) -> Result<Self, Self::Error> {
        match (r.valid, r.start, r.end) {
            (true, Some(start), Some(end)) => Ok(Self::Valid { start, end }),
            (false, None, None) => Ok(Self::Invalid),
            _ => Err("locator: start and end appear exactly when valid is true"),
        }
    }
}

impl From<Locator> for RawLocator {
    fn from(l: Locator) -> Self {
        let (valid, start, end) = match l {
            Locator::Valid { start, end } => (true, Some(start), Some(end)),
            Locator::Invalid => (false, None, None),
        };
        Self { valid, start, end }
    }
}
