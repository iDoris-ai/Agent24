//! Knowledge v1 wire types (KB-C1-02a), one-to-one with `protocol/knowledge/v1`
//! (CONTRACT-READONLY-v1 r9). Types are strict, producer-side: unknown fields and
//! values outside the v1 enums are rejected. Shape rules a type cannot carry
//! (cross-field `allOf`, string lengths, `uniqueItems`) stay with the schema and
//! `agent24-protocol/tests/knowledge_rules.rs`.
//!
//! Three kinds of `Option` appear, never mixed:
//! - required but nullable (`"k": null` allowed, missing key rejected, `None`
//!   always encodes as `null`): `#[serde(deserialize_with = "crate::nullable")]`;
//! - optional, never null (missing key = `None`, `null` rejected, `None` encodes
//!   as no key): `#[serde(default, with = "crate::opt", skip_serializing_if = ..)]`;
//! - optional, informational, `null` accepted (missing key or `null` = `None`,
//!   `None` encodes as no key): plain `#[serde(default, skip_serializing_if = ..)]`.
//!   Only `ChunkExpectedRevision::chunk_revision` (r9: a hit's `null` may be sent
//!   back, the server ignores the value).
//!
//! For the transport and the Mock, see `transport.rs` and `mock.rs`.

mod capabilities;
mod common;
mod error;
mod local_cancelled;
mod mock;
mod read;
mod ready;
mod search;
mod transport;

pub use capabilities::*;
pub use common::*;
pub use error::*;
pub use local_cancelled::*;
pub use mock::*;
pub use read::*;
pub use ready::*;
pub use search::*;
pub use transport::*;

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
