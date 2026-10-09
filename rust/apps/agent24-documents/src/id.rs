//! Ids: a type prefix + a ULID (ADR-DOC-02 §3), e.g. `doc_01K74Z3QJ8V5N2W9RTX6YB4MCD`.
//!
//! A ULID is 48 bits of Unix milliseconds followed by 80 random bits, written
//! as 26 Crockford base32 characters. 26 characters hold 130 bits, so the two
//! unused leading bits make the first character always 0–7 — the pattern the
//! OpenAPI schemas check.

use std::time::{SystemTime, UNIX_EPOCH};

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, thiserror::Error)]
#[error("no randomness available for an id: {0}")]
pub struct IdError(#[from] getrandom::Error);

/// The id kinds the OS mints; the prefix is part of the stored id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    Document,
    Upload,
    Job,
    Extraction,
}

impl IdKind {
    fn prefix(self) -> &'static str {
        match self {
            IdKind::Document => "doc",
            IdKind::Upload => "upl",
            IdKind::Job => "job",
            IdKind::Extraction => "ext",
        }
    }
}

/// A new id of `kind`.
pub fn new_id(kind: IdKind) -> Result<String, IdError> {
    // A clock set before 1970 gives time 0: such ids no longer sort by
    // creation time, but stay unique through the 80 random bits, and nothing
    // reads the time back out of an id.
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        & ((1u128 << 48) - 1);
    let mut random = [0u8; 10];
    getrandom::fill(&mut random)?;
    let mut value = millis << 80;
    for (i, b) in random.iter().enumerate() {
        value |= u128::from(*b) << (8 * (9 - i));
    }
    Ok(format!("{}_{}", kind.prefix(), encode(value)))
}

/// Whether `s` is an id of `kind` in the form [`new_id`] mints.
#[must_use]
pub fn is_id(kind: IdKind, s: &str) -> bool {
    s.strip_prefix(kind.prefix())
        .and_then(|rest| rest.strip_prefix('_'))
        .is_some_and(|ulid| {
            ulid.len() == 26
                && matches!(ulid.as_bytes()[0], b'0'..=b'7')
                && ulid.bytes().all(|b| CROCKFORD.contains(&b))
        })
}

/// 128 bits as 26 base32 characters, most significant first (the top two
/// bits of the 130 encoded are always zero).
fn encode(value: u128) -> String {
    (0..26)
        .rev()
        .map(|i| CROCKFORD[((value >> (5 * i)) & 0x1f) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The OpenAPI patterns, e.g. `^doc_[0-7][0-9A-HJKMNP-TV-Z]{25}$`.
    fn matches_schema(id: &str, prefix: &str) -> bool {
        let Some(rest) = id.strip_prefix(prefix).and_then(|r| r.strip_prefix('_')) else {
            return false;
        };
        rest.len() == 26
            && matches!(rest.as_bytes()[0], b'0'..=b'7')
            && rest.bytes().all(|b| CROCKFORD.contains(&b))
    }

    #[test]
    fn every_kind_matches_its_openapi_pattern() {
        for (kind, prefix) in [
            (IdKind::Document, "doc"),
            (IdKind::Upload, "upl"),
            (IdKind::Job, "job"),
            (IdKind::Extraction, "ext"),
        ] {
            let id = new_id(kind).unwrap();
            assert!(matches_schema(&id, prefix), "{id}");
        }
    }

    #[test]
    fn is_id_accepts_minted_ids_only() {
        let id = new_id(IdKind::Upload).unwrap();
        assert!(is_id(IdKind::Upload, &id));
        assert!(!is_id(IdKind::Job, &id));
        for bad in [
            "upl_",
            "upl_8ZZZZZZZZZZZZZZZZZZZZZZZZZ",
            "upl_01K74Z3QJ8V5N2W9RTX6YB4MCI", // I is not Crockford
            "upl_01k74z3qj8v5n2w9rtx6yb4mcd",
            "upl_01K74Z3QJ8V5N2W9RTX6YB4MC",
            "upl-01K74Z3QJ8V5N2W9RTX6YB4MCD",
        ] {
            assert!(!is_id(IdKind::Upload, bad), "{bad}");
        }
    }

    #[test]
    fn ids_are_unique() {
        let ids: std::collections::HashSet<String> =
            (0..2000).map(|_| new_id(IdKind::Job).unwrap()).collect();
        assert_eq!(ids.len(), 2000);
    }

    #[test]
    fn the_first_ten_characters_are_the_creation_time_in_milliseconds() {
        let now = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
        };
        let before = now();
        let id = new_id(IdKind::Document).unwrap();
        let after = now();
        let millis = id["doc_".len()..].bytes().take(10).fold(0u128, |acc, b| {
            let digit = CROCKFORD.iter().position(|&c| c == b).unwrap();
            (acc << 5) | digit as u128
        });
        assert!(
            (before..=after).contains(&millis),
            "{before} <= {millis} <= {after}"
        );
    }

    #[test]
    fn encoding_matches_independent_vectors() {
        // The ULID spec's own time vector (github.com/ulid/spec).
        assert_eq!(&encode(1_469_918_176_385u128 << 80)[..10], "01ARYZ6S41");
        // Computed separately, with the alphabet typed out again.
        assert_eq!(
            encode(0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210),
            "014D2PF2DBSQQZXQ5TK1V58CGG"
        );
    }

    #[test]
    fn encoding_is_crockford_big_endian() {
        assert_eq!(encode(0), "0".repeat(26));
        assert_eq!(encode(1), format!("{}1", "0".repeat(25)));
        assert_eq!(encode(u128::MAX), format!("7{}", "Z".repeat(25)));
        // the time part is the 10-character prefix, so ids sort by time
        let earlier = encode(1u128 << 80);
        let later = encode(2u128 << 80);
        assert!(earlier < later);
    }
}
