//! Knowledge v1 wire types (KB-C1-02a), one-to-one with `protocol/knowledge/v1`
//! (CONTRACT-READONLY-v1 r8). Types are strict, producer-side: unknown fields and
//! values outside the v1 enums are rejected. Shape rules a type cannot carry
//! (cross-field `allOf`, string lengths, `uniqueItems`) stay with the schema and
//! `agent24-protocol/tests/knowledge_rules.rs`.
//!
//! Two kinds of `Option` appear, never mixed:
//! - required but nullable (`"k": null` allowed, missing key rejected, `None`
//!   always encodes as `null`): `#[serde(deserialize_with = "crate::nullable")]`;
//! - optional, never null (missing key = `None`, `null` rejected, `None` encodes
//!   as no key): `#[serde(default, with = "crate::opt", skip_serializing_if = ..)]`.

mod common;
mod error;
mod read;
mod ready;
mod search;

pub use common::*;
pub use error::*;
pub use read::*;
pub use ready::*;
pub use search::*;

use serde::{Deserialize, Deserializer};

/// Required key whose value may be `null`. A bare `Option` would also accept a
/// missing key.
pub(crate) fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}

/// Optional key that must not be `null` when present (use with `default`).
pub(crate) mod opt {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<T>, D::Error> {
        T::deserialize(d).map(Some)
    }

    pub fn serialize<S: Serializer, T: Serialize>(v: &Option<T>, s: S) -> Result<S::Ok, S::Error> {
        v.serialize(s)
    }
}
