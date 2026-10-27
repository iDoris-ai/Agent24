//! `ready.schema.json`: `knowledge.ready` request and success response (§8).
//! Errors use [`crate::ErrorEnvelope`]. Cross-field rules between `operations`
//! and the per-KB states (R8.2, R8.4) stay with the schema.

use crate::common::{Generation, NonEmpty, ProtocolVersion, RequestScope};
use serde::{Deserialize, Serialize};

/// `$defs/request`. `scope` is optional (R8.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyRequest {
    pub protocol_version: ProtocolVersion,
    pub request_id: String,
    pub generation: Generation,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub scope: Option<RequestScope>,
}

/// `operation_state.state` (R8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Ready,
    NotReady,
    Disabled,
}

/// `$defs/operation_state`: infrastructure only; `reason` is free text (R8.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationState {
    pub state: OperationStatus,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `operations.ingest.state`: always `disabled` in read-only v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestStatus {
    Disabled,
}

/// `operations.ingest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestState {
    pub state: IngestStatus,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `response.operations`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operations {
    pub search: OperationState,
    pub read: OperationState,
    pub ingest: IngestState,
}

/// `kb_state.reason` (R8.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KbNotReadyReason {
    OperationDisabled,
    Dependency,
    IndexBuilding,
    IndexFailed,
    UnsupportedKbType,
}

/// `$defs/kb_state` (R8.4): `ready` never carries a reason, `not_ready` always does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawKbState", into = "RawKbState")]
pub enum KbState {
    Ready,
    NotReady(KbNotReadyReason),
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawKbStatus {
    Ready,
    NotReady,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKbState {
    state: RawKbStatus,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    reason: Option<KbNotReadyReason>,
}

impl TryFrom<RawKbState> for KbState {
    type Error = &'static str;
    fn try_from(r: RawKbState) -> Result<Self, Self::Error> {
        match (r.state, r.reason) {
            (RawKbStatus::Ready, None) => Ok(Self::Ready),
            (RawKbStatus::NotReady, Some(reason)) => Ok(Self::NotReady(reason)),
            _ => Err("kb_state: reason appears exactly when state is not_ready"),
        }
    }
}

impl From<KbState> for RawKbState {
    fn from(s: KbState) -> Self {
        let (state, reason) = match s {
            KbState::Ready => (RawKbStatus::Ready, None),
            KbState::NotReady(reason) => (RawKbStatus::NotReady, Some(reason)),
        };
        Self { state, reason }
    }
}

/// `$defs/knowledge_base`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeBaseReadiness {
    pub knowledge_base_id: String,
    pub score_space: String,
    pub search: KbState,
    pub read: KbState,
}

/// `$defs/response`. `knowledge_bases` is present exactly when the request
/// carried scope (R8.2); that pairing is left to the code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyResponse {
    pub protocol_version: ProtocolVersion,
    pub request_id: String,
    pub generation: Generation,
    pub live: bool,
    pub operations: Operations,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub knowledge_bases: Option<NonEmpty<KnowledgeBaseReadiness>>,
}
