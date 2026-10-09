//! Business-key idempotency (ADR-DOC-02 §5.4): `request_sha256` is the
//! SHA-256 of the business parameters in RFC 8785 (JCS) canonical form, so
//! the same request hashes the same however its JSON was written.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Largest integer JSON carries exactly (2^53 − 1). Within it JCS writes an
/// integer as plain decimal; beyond it, or for a fraction, JCS needs the
/// ECMAScript number formatting this module does not implement.
pub const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("not a safe integer for a request hash: {0}")]
pub struct UnsupportedNumber(String);

/// `sha256:<hex>` of `params` in canonical form. Parameters are strings,
/// safe integers, booleans, null, arrays and objects; any other number is
/// refused rather than hashed differently from JCS.
pub fn request_sha256(params: &Value) -> Result<String, UnsupportedNumber> {
    let mut canonical = String::new();
    write_canonical(params, &mut canonical)?;
    let digest = Sha256::digest(canonical.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

fn write_canonical(v: &Value, out: &mut String) -> Result<(), UnsupportedNumber> {
    match v {
        Value::Number(n) => match n.as_i64() {
            Some(i) if i.unsigned_abs() <= MAX_SAFE_INTEGER.unsigned_abs() => {
                out.push_str(&i.to_string());
            }
            _ => return Err(UnsupportedNumber(n.to_string())),
        },
        // serde_json writes strings as JCS does: `"`, `\` and controls
        // escaped (\b \f \n \r \t, else \u00xx lowercase), all else verbatim.
        Value::Null | Value::Bool(_) | Value::String(_) => out.push_str(&v.to_string()),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // JCS orders keys by their UTF-16 code units.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by_cached_key(|k| k.encode_utf16().collect::<Vec<u16>>());
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    fn canonical(v: &Value) -> String {
        let mut out = String::new();
        write_canonical(v, &mut out).unwrap();
        out
    }

    #[test]
    fn keys_are_sorted_and_whitespace_dropped() {
        let v: Value =
            serde_json::from_str(r#"{ "b": 1, "a": { "d": [true, null], "c": "x" } }"#).unwrap();
        assert_eq!(canonical(&v), r#"{"a":{"c":"x","d":[true,null]},"b":1}"#);
    }

    #[test]
    fn objects_inside_arrays_are_sorted_too() {
        let v = json!([{ "b": 1, "a": [{ "d": 2, "c": 3 }] }]);
        assert_eq!(canonical(&v), r#"[{"a":[{"c":3,"d":2}],"b":1}]"#);
    }

    #[test]
    fn strings_use_the_jcs_escapes() {
        // Short escapes where JCS has them, else \u00xx in lowercase hex;
        // `/`, DEL, U+2028/2029 and non-BMP characters stay as they are.
        let v = json!("\u{8}\u{c}\n\r\t\u{1}\u{b}\u{1f}\"\\/é😀\u{7f}\u{2028}\u{2029}");
        let want = concat!(
            "\"",
            r#"\b\f\n\r\t\u0001\u000b\u001f\"\\"#,
            "/é😀\u{7f}\u{2028}\u{2029}",
            "\""
        );
        assert_eq!(canonical(&v), want);
    }

    #[test]
    fn keys_sort_by_utf16_code_units_not_by_code_points() {
        // By code point U+FF61 < U+1F600, but in UTF-16 😀 is the pair
        // 0xD83D 0xDE00, which sorts before 0xFF61.
        let v = json!({ "\u{ff61}": 1, "😀": 2 });
        assert_eq!(canonical(&v), "{\"😀\":2,\"\u{ff61}\":1}");
    }

    #[test]
    fn only_safe_integers_are_hashed() {
        for ok in [
            json!(0),
            json!(-1),
            json!(MAX_SAFE_INTEGER),
            json!(-MAX_SAFE_INTEGER),
        ] {
            assert_eq!(canonical(&ok), ok.to_string());
        }
        // JCS writes 1000000000000000128 as 1000000000000000100 and 1.0 as
        // 1; refuse such numbers rather than hash them differently.
        for bad in [
            json!(MAX_SAFE_INTEGER + 1),
            json!(-MAX_SAFE_INTEGER - 1),
            json!(1_000_000_000_000_000_128_i64),
            json!(u64::MAX),
            json!(1.0),
            json!(0.5),
        ] {
            assert!(request_sha256(&json!({ "n": [bad] })).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_hash_ignores_member_order_and_is_prefixed() {
        let a = request_sha256(&json!({ "total_size": 10, "sha256": "x" })).unwrap();
        let b = request_sha256(&json!({ "sha256": "x", "total_size": 10 })).unwrap();
        assert_eq!(a, b);
        // `printf '%s' '{"sha256":"x","total_size":10}' | shasum -a 256`
        assert_eq!(
            a,
            "sha256:0e5b9cbf6e4efbcade2a106a2f8477fc125239a417b8854618ee727ab98a074c"
        );
    }
}
