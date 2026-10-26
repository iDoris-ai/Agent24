//! What the Provider decodes from the server (KB-C1-04 6a): the same bodies as
//! the wire types, read the way READONLY obliges a client to. Unknown fields
//! are ignored at every level, and an unknown `reason` reads as none (§0.3).
//! R6.2's fallbacks: an unknown `code`, `partial_failures[].code` included,
//! branches as `internal` and is retried only when it says `retryable: true`
//! itself; an unknown `index_state` reads as `stale`, `completeness` as
//! `partial`, a readiness `state` as `not_ready`.
//! Every other value outside v1 is still refused: a new `chunk_type` or
//! `protocol_version` is a semantic change, not a compatible one (§0.3).
//!
//! The wire types stay strict and producer-side; nothing here encodes. A field
//! the Provider never reads is not decoded: the server's `message` and `hint`
//! (R6.3), a readiness `reason` (free text) and `ingest` (S1.3 leaves it out).

use serde::de::{DeserializeOwned, IntoDeserializer, value};
use serde::{Deserialize, Deserializer};
use std::num::NonZeroU64;

use crate::{
    AtLeastTwo, ChunkRevision, ChunkType, Completeness, ConstBool, ErrorCode, ForbiddenReason,
    Generation, IndexState, InvalidRequestReason, KbNotReadyReason, Locator, NonEmpty,
    OperationStatus, PartialFailureReason, PositiveInt, ProtocolVersion, StaleReason,
    TimeoutReason, UnauthorizedReason, UnavailableReason,
};

/// A string from a v1 set, or none when the value is outside it. Anything but
/// a string or `null` is still malformed.
fn known<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    let s = Option::<String>::deserialize(d)?;
    Ok(s.and_then(|s| T::deserialize(IntoDeserializer::<value::Error>::into_deserializer(s)).ok()))
}

fn completeness<'de, D: Deserializer<'de>>(d: D) -> Result<Completeness, D::Error> {
    Ok(known(d)?.unwrap_or(Completeness::Partial))
}

fn index_state<'de, D: Deserializer<'de>>(d: D) -> Result<IndexState, D::Error> {
    Ok(known(d)?.unwrap_or(IndexState::Stale))
}

fn readiness<'de, D: Deserializer<'de>>(d: D) -> Result<OperationStatus, D::Error> {
    Ok(known(d)?.unwrap_or(OperationStatus::NotReady))
}

/// `request_scope` and `effective_scope`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Scope {
    pub knowledge_base_ids: NonEmpty<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Display {
    pub title: Option<String>,
    pub file_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct Budget {
    pub deadline_ms: PositiveInt,
    pub max_results: PositiveInt,
    pub max_concurrency: PositiveInt,
}

/// `partial_failure.code`; an unknown one branches as `internal` (R6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartialFailureCode {
    Timeout,
    Unavailable,
    Internal,
}

impl<'de> Deserialize<'de> for PartialFailureCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(d)?.as_str() {
            "timeout" => Self::Timeout,
            "unavailable" => Self::Unavailable,
            _ => Self::Internal,
        })
    }
}

/// `retryable` is the item's own; without one it is not retried (R6.2).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(from = "RawPartialFailure")]
pub struct PartialFailure {
    pub knowledge_base_id: String,
    pub code: PartialFailureCode,
    pub reason: Option<PartialFailureReason>,
    pub retryable: bool,
}

#[derive(Deserialize)]
struct RawPartialFailure {
    knowledge_base_id: String,
    code: PartialFailureCode,
    #[serde(default, deserialize_with = "known")]
    reason: Option<PartialFailureReason>,
    #[serde(default)]
    retryable: bool,
}

/// The reasons R3.5 makes not retryable: an item that gives one is not
/// retried, whatever its `retryable` says.
pub(crate) fn never_retried(reason: Option<PartialFailureReason>) -> bool {
    matches!(
        reason,
        Some(PartialFailureReason::IndexFailed | PartialFailureReason::UnsupportedKbType)
    )
}

impl From<RawPartialFailure> for PartialFailure {
    fn from(r: RawPartialFailure) -> Self {
        Self {
            retryable: r.retryable && !never_retried(r.reason),
            knowledge_base_id: r.knowledge_base_id,
            code: r.code,
            reason: r.reason,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SearchResponse {
    pub protocol_version: ProtocolVersion,
    pub request_id: String,
    pub generation: Generation,
    #[serde(deserialize_with = "completeness")]
    pub completeness: Completeness,
    pub effective_scope: Scope,
    pub applied_budget: Budget,
    pub hits: Vec<SearchHit>,
    pub partial_failures: Option<NonEmpty<PartialFailure>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SearchHit {
    pub rank: PositiveInt,
    pub knowledge_base_id: String,
    pub knowledge_id: String,
    pub chunk_id: String,
    pub chunk_type: ChunkType,
    pub revision: HitRevision,
    #[serde(deserialize_with = "locator")]
    pub locator: Locator,
    #[serde(deserialize_with = "index_state")]
    pub index_state: IndexState,
    pub score: f64,
    pub score_space: String,
    pub snippet: Snippet,
    pub display: Option<Display>,
}

/// `chunk_revision` is required but may be `null` (R3.9).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct HitRevision {
    pub source_revision: String,
    pub index_build_id: String,
    #[serde(deserialize_with = "crate::nullable")]
    pub chunk_revision: Option<ChunkRevision>,
    pub content_tag: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Snippet {
    pub text: String,
    pub truncated: bool,
}

/// Offsets appear exactly when `valid` is true, as on the wire (R3.10).
fn locator<'de, D: Deserializer<'de>>(d: D) -> Result<Locator, D::Error> {
    #[derive(Deserialize)]
    struct Raw {
        valid: bool,
        start: Option<u64>,
        end: Option<NonZeroU64>,
    }
    let r = Raw::deserialize(d)?;
    match (r.valid, r.start, r.end) {
        (true, Some(start), Some(end)) => Ok(Locator::Valid { start, end }),
        (false, None, None) => Ok(Locator::Invalid),
        _ => Err(serde::de::Error::custom(
            "locator: start and end appear exactly when valid is true",
        )),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DocRevision {
    pub source_revision: String,
    pub index_build_id: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ReadResponse {
    pub protocol_version: ProtocolVersion,
    pub request_id: String,
    pub generation: Generation,
    pub effective_scope: Scope,
    pub knowledge_id: String,
    pub revision: DocRevision,
    pub display: Option<Display>,
    pub segments: Vec<Segment>,
    pub truncated: bool,
    #[serde(deserialize_with = "crate::nullable")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Segment {
    pub chunk_id: String,
    pub chunk_type: ChunkType,
    pub chunk_revision: ChunkRevision,
    pub content_tag: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ReadyResponse {
    pub protocol_version: ProtocolVersion,
    pub request_id: String,
    pub generation: Generation,
    pub live: bool,
    pub operations: Operations,
    pub knowledge_bases: Option<NonEmpty<KnowledgeBaseReadiness>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct Operations {
    pub search: OperationState,
    pub read: OperationState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct OperationState {
    #[serde(deserialize_with = "readiness")]
    pub state: OperationStatus,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct KnowledgeBaseReadiness {
    pub knowledge_base_id: String,
    pub score_space: String,
    pub search: KbState,
    pub read: KbState,
}

/// Anything but `ready` is not ready, with whatever known reason it gives;
/// a reason next to `ready` is ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KbState {
    Ready,
    NotReady(Option<KbNotReadyReason>),
}

impl<'de> Deserialize<'de> for KbState {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(deserialize_with = "readiness")]
            state: OperationStatus,
            #[serde(default, deserialize_with = "known")]
            reason: Option<KbNotReadyReason>,
        }
        let r = Raw::deserialize(d)?;
        Ok(match r.state {
            OperationStatus::Ready => Self::Ready,
            _ => Self::NotReady(r.reason),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ErrorEnvelope {
    pub error: WireError,
}

/// `error`: the details of its code, `Other` for a code outside v1.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawError")]
pub struct WireError {
    pub details: ErrorDetails,
}

impl WireError {
    /// The branch to take: an unknown code is `internal` (R6.2).
    pub fn code(&self) -> ErrorCode {
        match &self.details {
            ErrorDetails::InvalidRequest(_) => ErrorCode::InvalidRequest,
            ErrorDetails::Unauthorized(_) => ErrorCode::Unauthorized,
            ErrorDetails::Forbidden(_) => ErrorCode::Forbidden,
            ErrorDetails::NotFound(_) => ErrorCode::NotFound,
            ErrorDetails::Stale(_) => ErrorCode::Stale,
            ErrorDetails::Timeout(_) => ErrorCode::Timeout,
            ErrorDetails::Unavailable(_) => ErrorCode::Unavailable,
            ErrorDetails::Internal(_) | ErrorDetails::Other(_) => ErrorCode::Internal,
        }
    }

    /// Fixed per code (§6) but for `unavailable`, and for an unknown code its
    /// own value, false when it gives none.
    pub fn retryable(&self) -> bool {
        match &self.details {
            ErrorDetails::Timeout(_) => true,
            ErrorDetails::Unavailable(d) => d.retryable,
            ErrorDetails::Other(d) => d.retryable.unwrap_or(false),
            _ => false,
        }
    }
}

#[derive(Deserialize)]
struct RawError {
    code: String,
    details: serde_json::Value,
}

impl TryFrom<RawError> for WireError {
    type Error = serde_json::Error;
    fn try_from(raw: RawError) -> Result<Self, Self::Error> {
        use serde_json::from_value as de;
        let d = raw.details;
        let details = match raw.code.as_str() {
            "invalid_request" => ErrorDetails::InvalidRequest(de(d)?),
            "unauthorized" => ErrorDetails::Unauthorized(de(d)?),
            "forbidden" => ErrorDetails::Forbidden(de(d)?),
            "not_found" => ErrorDetails::NotFound(de(d)?),
            "stale" => ErrorDetails::Stale(de(d)?),
            "timeout" => ErrorDetails::Timeout(de(d)?),
            "unavailable" => ErrorDetails::Unavailable(de(d)?),
            "internal" => ErrorDetails::Internal(de(d)?),
            _ => ErrorDetails::Other(de(d)?),
        };
        Ok(Self { details })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ErrorDetails {
    InvalidRequest(InvalidRequestDetails),
    Unauthorized(UnauthorizedDetails),
    Forbidden(ForbiddenDetails),
    NotFound(NotFoundDetails),
    Stale(StaleDetails),
    Timeout(TimeoutDetails),
    Unavailable(UnavailableDetails),
    Internal(InternalDetails),
    Other(OtherDetails),
}

/// `retryable` must be the code's own value (§6); the echoes may be `null`
/// or missing.
macro_rules! details {
    ($name:ident: $retryable:ty { $($body:tt)* }) => {
        #[derive(Clone, Debug, PartialEq, Deserialize)]
        pub struct $name {
            pub retryable: $retryable,
            pub request_id: Option<String>,
            pub generation: Option<Generation>,
            $($body)*
        }
    };
}

details!(InvalidRequestDetails: ConstBool<false> {
    #[serde(default, deserialize_with = "known")]
    pub reason: Option<InvalidRequestReason>,
    pub groups: Option<AtLeastTwo<ScoreSpaceGroup>>,
    pub supported: Option<NonEmpty<PositiveInt>>,
});
details!(UnauthorizedDetails: ConstBool<false> {
    /// None is an unknown reason: the invalid-credential class (STATE S3.1).
    #[serde(default, deserialize_with = "known")]
    pub reason: Option<UnauthorizedReason>,
});
details!(ForbiddenDetails: ConstBool<false> {
    #[serde(default, deserialize_with = "known")]
    pub reason: Option<ForbiddenReason>,
});
details!(NotFoundDetails: ConstBool<false> {});
details!(StaleDetails: ConstBool<false> {
    #[serde(default, deserialize_with = "known")]
    pub reason: Option<StaleReason>,
    pub current_revision: CurrentRevision,
    pub current_chunk_revision: Option<ChunkRevision>,
    pub current_content_tag: Option<String>,
});
details!(TimeoutDetails: ConstBool<true> {
    #[serde(default, deserialize_with = "known")]
    pub reason: Option<TimeoutReason>,
    pub partial_failures: Option<NonEmpty<PartialFailure>>,
});
details!(UnavailableDetails: bool {
    #[serde(default, deserialize_with = "known")]
    pub reason: Option<UnavailableReason>,
    pub retry_after_ms: Option<u64>,
    pub partial_failures: Option<NonEmpty<PartialFailure>>,
});
details!(InternalDetails: ConstBool<false> {});
details!(OtherDetails: Option<bool> {});

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ScoreSpaceGroup {
    pub score_space: String,
    pub knowledge_base_ids: NonEmpty<String>,
}

/// `index_build_id` is required but `null` while no build is active (R4.4).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CurrentRevision {
    pub source_revision: String,
    #[serde(deserialize_with = "crate::nullable")]
    pub index_build_id: Option<String>,
}
