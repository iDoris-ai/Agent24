//! Shared pure utilities: ULID generation and ISO 8601 timestamps.
//! (Moved here from agent24d in C2 so agent/store/daemon share one source.)

use rand::RngCore;

/// ULID (Crockford base32): 48-bit ms timestamp + 80-bit CSPRNG randomness.
pub fn ulid() -> String {
    const B32: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut out = [0u8; 26];
    let mut t = ms;
    for i in (0..10).rev() {
        out[i] = B32[(t % 32) as usize];
        t /= 32;
    }
    let mut rnd = [0u8; 16];
    rand::rng().fill_bytes(&mut rnd);
    for i in 0..16 {
        out[10 + i] = B32[(rnd[i] % 32) as usize];
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// ISO 8601 UTC, second precision (civil-from-days — no chrono dependency).
pub fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn ulid_shape_and_uniqueness() {
        let a = ulid();
        let b = ulid();
        assert_eq!(a.len(), 26);
        assert!(
            a.bytes()
                .all(|c| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&c))
        );
        assert_ne!(a, b);
    }

    #[test]
    fn iso8601_shape() {
        let ts = now_iso8601();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[10..11], "T");
    }
}
