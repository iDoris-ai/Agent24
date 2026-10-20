//! `error.schema.json`: the wire error envelope (§6). `cancelled` is
//! client-local and not here (`local-cancelled.schema.json`).

use crate::common::{AtLeastTwo, ConstBool, Generation, NonEmpty, PartialFailure, PositiveInt};
use serde::{Deserialize, Serialize, Serializer};

/// `error.code`: the closed v1 set (R6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Stale,
    Timeout,
    Unavailable,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorEnvelope {
    pub error: WireError,
}

/// `error`. `code` is not stored: it is the variant of `details`, so the
/// details shape always matches the code.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawError")]
pub struct WireError {
    pub message: String,
    pub hint: Option<String>,
    pub details: ErrorDetails,
}

impl WireError {
    pub fn code(&self) -> ErrorCode {
        self.details.code()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawError {
    code: ErrorCode,
    message: String,
    #[serde(default, deserialize_with = "crate::opt::deserialize")]
    hint: Option<String>,
    details: serde_json::Value,
}

#[derive(Serialize)]
struct RawErrorOut<'a> {
    code: ErrorCode,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
    details: &'a ErrorDetails,
}

impl TryFrom<RawError> for WireError {
    type Error = serde_json::Error;
    fn try_from(raw: RawError) -> Result<Self, Self::Error> {
        use serde_json::from_value as de;
        let d = raw.details;
        let details = match raw.code {
            ErrorCode::InvalidRequest => ErrorDetails::InvalidRequest(de(d)?),
            ErrorCode::Unauthorized => ErrorDetails::Unauthorized(de(d)?),
            ErrorCode::Forbidden => ErrorDetails::Forbidden(de(d)?),
            ErrorCode::NotFound => ErrorDetails::NotFound(de(d)?),
            ErrorCode::Stale => ErrorDetails::Stale(de(d)?),
            ErrorCode::Timeout => ErrorDetails::Timeout(de(d)?),
            ErrorCode::Unavailable => ErrorDetails::Unavailable(de(d)?),
            ErrorCode::Internal => ErrorDetails::Internal(de(d)?),
        };
        Ok(Self {
            message: raw.message,
            hint: raw.hint,
            details,
        })
    }
}

impl Serialize for WireError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let (code, message, hint) = (self.code(), self.message.as_str(), self.hint.as_deref());
        RawErrorOut {
            code,
            message,
            hint,
            details: &self.details,
        }
        .serialize(s)
    }
}

/// `error.details`, one variant per code (the per-code `$defs`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ErrorDetails {
    InvalidRequest(InvalidRequestDetails),
    Unauthorized(UnauthorizedDetails),
    Forbidden(ForbiddenDetails),
    NotFound(NotFoundDetails),
    Stale(StaleDetails),
    Timeout(TimeoutDetails),
    Unavailable(UnavailableDetails),
    Internal(InternalDetails),
}

impl ErrorDetails {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidRequest(_) => ErrorCode::InvalidRequest,
            Self::Unauthorized(_) => ErrorCode::Unauthorized,
            Self::Forbidden(_) => ErrorCode::Forbidden,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Stale(_) => ErrorCode::Stale,
            Self::Timeout(_) => ErrorCode::Timeout,
            Self::Unavailable(_) => ErrorCode::Unavailable,
            Self::Internal(_) => ErrorCode::Internal,
        }
    }
}

/// Every details object carries `$defs/base`: `retryable` plus the echoes,
/// which are required keys and `null` only where R6.1 allows (which codes may
/// echo `null` is a cross-field rule left to the schema). `retryable` is
/// fixed per code (§6) except for `unavailable`, where it is a free `bool`.
macro_rules! details {
    ($(#[$m:meta])* $name:ident: $retryable:ty { $($body:tt)* }) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub retryable: $retryable,
            #[serde(deserialize_with = "crate::nullable")]
            pub request_id: Option<String>,
            #[serde(deserialize_with = "crate::nullable")]
            pub generation: Option<Generation>,
            $($body)*
        }
    };
}

macro_rules! reasons {
    ($name:ident: $($v:ident),+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($v),+ }
    };
}

reasons!(InvalidRequestReason: MixedScoreSpace, UnsupportedProtocolVersion);
reasons!(UnauthorizedReason: CredentialMissing, CredentialInvalid, CredentialExpired, CredentialRevoked);
reasons!(ForbiddenReason: EmptyScope, OperationNotAllowed, OperationDisabled, PolicyDenied);
reasons!(StaleReason: AnchorMissing);
reasons!(TimeoutReason: Timeout);
reasons!(UnavailableReason: NotReady, Dependency, Overloaded, IndexBuilding, UnsupportedKbType, IndexFailed, IndexPending);

details!(
    /// `reason` iff `groups` / `supported` (§6) is left to the schema.
    InvalidRequestDetails: ConstBool<false> {
        #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
        pub reason: Option<InvalidRequestReason>,
        #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
        pub groups: Option<AtLeastTwo<ScoreSpaceGroup>>,
        #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
        pub supported: Option<NonEmpty<PositiveInt>>,
    }
);
details!(UnauthorizedDetails: ConstBool<false> { pub reason: UnauthorizedReason, });
details!(ForbiddenDetails: ConstBool<false> { pub reason: ForbiddenReason, });
details!(NotFoundDetails: ConstBool<false> {});
details!(StaleDetails: ConstBool<false> {
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub reason: Option<StaleReason>,
    pub current_revision: CurrentRevision,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub current_chunk_revision: Option<u64>,
});
details!(TimeoutDetails: ConstBool<true> {
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub reason: Option<TimeoutReason>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub partial_failures: Option<NonEmpty<PartialFailure>>,
});
details!(UnavailableDetails: bool {
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnavailableReason>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub partial_failures: Option<NonEmpty<PartialFailure>>,
});
details!(InternalDetails: ConstBool<false> {});

/// `invalid_request.groups[]` (R3.6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreSpaceGroup {
    pub score_space: String,
    pub knowledge_base_ids: NonEmpty<String>,
}

/// `stale.current_revision` (R4.4): `index_build_id` is `null` while no build
/// is active.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentRevision {
    pub source_revision: String,
    #[serde(deserialize_with = "crate::nullable")]
    pub index_build_id: Option<String>,
}
