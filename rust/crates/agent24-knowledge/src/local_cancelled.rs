//! `local-cancelled.schema.json`: the Provider-local `cancelled` result (R7.3,
//! §11.4). Never on the wire, so it is not an [`crate::ErrorEnvelope`] variant.

use crate::common::{ConstBool, Generation};
use serde::{Deserialize, Serialize};

/// The local result returned when a response is dropped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalCancelled {
    pub error: CancelledError,
}

/// `error`: `code` is the const `cancelled`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelledError {
    pub code: CancelledCode,
    pub message: String,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub details: CancelledDetails,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelledCode {
    Cancelled,
}

/// `details`: `generation` echoes the dropped request; `current_generation`
/// is the client's value. Both ids are required and never `null`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelledDetails {
    pub retryable: ConstBool<false>,
    pub request_id: String,
    pub generation: Generation,
    pub current_generation: Generation,
}
