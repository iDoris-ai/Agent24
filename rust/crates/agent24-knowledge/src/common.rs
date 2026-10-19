//! `common.schema.json`.

use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

/// `positive_int`.
pub type PositiveInt = NonZeroU64;
/// `generation` (R7.2): echoed, not checked, by the server.
pub type Generation = u64;
/// `chunk_revision` (R5.2): edit count, 0 = unedited.
pub type ChunkRevision = u64;

/// `protocol_version`: the const `1` (R3.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct ProtocolVersion;

impl TryFrom<u64> for ProtocolVersion {
    type Error = String;
    fn try_from(v: u64) -> Result<Self, String> {
        match v {
            1 => Ok(Self),
            _ => Err(format!("protocol_version {v} is not 1")),
        }
    }
}

impl From<ProtocolVersion> for u64 {
    fn from(_: ProtocolVersion) -> u64 {
        1
    }
}

/// An array with `minItems: 1`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<T>", into = "Vec<T>")]
pub struct NonEmpty<T: Clone>(Vec<T>);

impl<T: Clone> NonEmpty<T> {
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

impl<T: Clone> TryFrom<Vec<T>> for NonEmpty<T> {
    type Error = &'static str;
    fn try_from(v: Vec<T>) -> Result<Self, Self::Error> {
        (!v.is_empty())
            .then(|| Self(v))
            .ok_or("array must not be empty")
    }
}

impl<T: Clone> From<NonEmpty<T>> for Vec<T> {
    fn from(v: NonEmpty<T>) -> Vec<T> {
        v.0
    }
}

/// An array with `minItems: 2`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<T>", into = "Vec<T>")]
pub struct AtLeastTwo<T: Clone>(Vec<T>);

impl<T: Clone> AtLeastTwo<T> {
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

impl<T: Clone> TryFrom<Vec<T>> for AtLeastTwo<T> {
    type Error = &'static str;
    fn try_from(v: Vec<T>) -> Result<Self, Self::Error> {
        (v.len() >= 2)
            .then(|| Self(v))
            .ok_or("array must have at least two items")
    }
}

impl<T: Clone> From<AtLeastTwo<T>> for Vec<T> {
    fn from(v: AtLeastTwo<T>) -> Vec<T> {
        v.0
    }
}

/// A boolean the schema fixes to `const: V` (e.g. `retryable` per error code, §6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "bool", into = "bool")]
pub struct ConstBool<const V: bool>;

impl<const V: bool> ConstBool<V> {
    pub const fn get(self) -> bool {
        V
    }
}

impl<const V: bool> TryFrom<bool> for ConstBool<V> {
    type Error = String;
    fn try_from(v: bool) -> Result<Self, String> {
        (v == V)
            .then_some(Self)
            .ok_or_else(|| format!("must be {V}, got {v}"))
    }
}

impl<const V: bool> From<ConstBool<V>> for bool {
    fn from(_: ConstBool<V>) -> bool {
        V
    }
}

/// `request_scope` (Q, R2.5) and `effective_scope` (E, R2.1); same shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub knowledge_base_ids: NonEmpty<String>,
}
pub type RequestScope = Scope;
pub type EffectiveScope = Scope;

/// `chunk_type`: v1 serves text only (R3.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkType {
    Text,
}

/// `display`: metadata only, never a citation (R3.8, R4.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Display {
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, with = "crate::opt", skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

/// `partial_failure.code`: the subset of the error codes R3.5 allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialFailureCode {
    Timeout,
    Unavailable,
}

/// `partial_failure.reason` (R3.5, r8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialFailureReason {
    Timeout,
    Dependency,
    NotReady,
    Overloaded,
    IndexBuilding,
    UnsupportedKbType,
    IndexFailed,
}

/// `partial_failure` (R3.5, R3.7b). The code/reason/retryable pairing is a
/// cross-field rule left to the schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialFailure {
    pub knowledge_base_id: String,
    pub code: PartialFailureCode,
    pub reason: PartialFailureReason,
    pub retryable: bool,
}
