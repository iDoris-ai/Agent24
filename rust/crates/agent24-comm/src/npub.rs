//! COMM-2a: a minimal bech32 validity check for Nostr `npub1...` ids.
//!
//! COMM-HYPHAE.md §4/G6: Hyphae itself classifies a malformed npub as
//! `other_error` (exit 4), which the REST layer's closed error set (§4)
//! would otherwise have to surface as `upstream` — an unhelpful answer for
//! what is really a client-side typo. The design calls for comm to catch
//! this itself, BEFORE ever invoking Hyphae, and answer `invalid` (400)
//! instead. [`is_valid_npub`] only checks that the string is well-formed
//! bech32 with human-readable part `npub` and a verifying checksum; it does
//! not decode the 32-byte public key the payload encodes — that is Hyphae's
//! job, and re-implementing Nostr's key format here would be scope well
//! beyond a syntax check.

const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const EXPECTED_HRP: &str = "npub";

/// BIP-173 `bech32_polymod` — the checksum both decoding (below) and the
/// test fixtures' encoder (`tests::encode`) are built on.
fn polymod(values: &[u8]) -> u32 {
    const GEN: [u32; 5] = [
        0x3b6a_57b2,
        0x2650_8e6d,
        0x1ea1_19fa,
        0x3d42_33dd,
        0x2a14_62b3,
    ];
    let mut chk: u32 = 1;
    for &v in values {
        let top = (chk >> 25) as u8;
        chk = (chk & 0x01ff_ffff) << 5 ^ u32::from(v);
        for (i, g) in GEN.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

fn hrp_expand(hrp: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> = hrp.iter().map(|b| b >> 5).collect();
    v.push(0);
    v.extend(hrp.iter().map(|b| b & 31));
    v
}

/// `true` iff `s` is syntactically valid bech32 whose human-readable part is
/// exactly `npub` (lowercase — bech32 forbids mixed case) and whose
/// checksum verifies.
pub fn is_valid_npub(s: &str) -> bool {
    // BIP-173: bech32 strings are 8..=90 chars total, all-lowercase (or
    // all-uppercase, which Nostr npubs never use) ASCII.
    if s.len() < 8 || s.len() > 90 || !s.is_ascii() {
        return false;
    }
    if s.chars().any(|c| c.is_ascii_uppercase()) {
        return false;
    }
    let Some(sep) = s.rfind('1') else {
        return false;
    };
    // hrp must be non-empty, and at least 6 chars (the checksum) must follow it.
    if sep == 0 || s.len() - sep - 1 < 6 {
        return false;
    }
    let hrp = &s[..sep];
    if hrp != EXPECTED_HRP {
        return false;
    }
    let mut data = Vec::with_capacity(s.len() - sep - 1);
    for c in s[sep + 1..].bytes() {
        match CHARSET.iter().position(|&x| x == c) {
            Some(v) => data.push(v as u8),
            None => return false,
        }
    }
    let mut values = hrp_expand(hrp.as_bytes());
    values.extend_from_slice(&data);
    polymod(&values) == 1
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Encodes `hrp`+`data` (5-bit values, 0..=31 each) into a valid bech32
    /// string — the inverse of [`is_valid_npub`]'s decode. Exists only to
    /// build self-consistent fixtures below, so these tests do not depend on
    /// a hand-copied "known good" npub string being correct.
    fn encode(hrp: &str, data: &[u8]) -> String {
        let mut values = hrp_expand(hrp.as_bytes());
        values.extend_from_slice(data);
        values.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let poly = polymod(&values) ^ 1;
        let mut out = format!("{hrp}1");
        for v in data {
            out.push(CHARSET[*v as usize] as char);
        }
        for i in 0..6 {
            let v = (poly >> (5 * (5 - i))) & 31;
            out.push(CHARSET[v as usize] as char);
        }
        out
    }

    #[test]
    fn well_formed_npub_is_valid() {
        let sample = encode(EXPECTED_HRP, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert!(is_valid_npub(&sample), "{sample}");
    }

    #[test]
    fn wrong_human_readable_part_is_rejected() {
        let sample = encode("nsec", &[1, 2, 3]);
        assert!(!is_valid_npub(&sample));
    }

    #[test]
    fn corrupted_checksum_is_rejected() {
        let mut sample = encode(EXPECTED_HRP, &[1, 2, 3]).into_bytes();
        let last = sample.len() - 1;
        let current = CHARSET.iter().position(|&c| c == sample[last]).unwrap();
        sample[last] = CHARSET[(current + 1) % CHARSET.len()];
        assert!(!is_valid_npub(std::str::from_utf8(&sample).unwrap()));
    }

    #[test]
    fn mixed_case_is_rejected() {
        let sample = encode(EXPECTED_HRP, &[1, 2, 3]);
        let upper = sample.to_ascii_uppercase();
        assert!(!is_valid_npub(&upper));
    }

    #[test]
    fn garbage_and_edge_cases_are_rejected() {
        assert!(!is_valid_npub("not-a-valid-npub"));
        assert!(!is_valid_npub(""));
        assert!(!is_valid_npub("npub1"));
        assert!(!is_valid_npub("1qqqqqq"));
    }
}
