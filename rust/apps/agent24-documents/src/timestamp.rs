//! Timestamps as the OS stores them (`strftime('%Y-%m-%dT%H:%M:%fZ')`).

/// A timestamp in the one form the OS writes, `strftime('%Y-%m-%dT%H:%M:%fZ')`,
/// that is also a real RFC 3339 date-time. Checked here, not by SQLite: its
/// date functions keep some impossible dates (hour 24, signed years, 29
/// February 300).
pub fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let shape = b.len() == 24
        && b.iter().enumerate().all(|(i, &c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'.',
            23 => c == b'Z',
            _ => c.is_ascii_digit(),
        });
    if !shape {
        return false;
    }
    let num = |from: usize, to: usize| {
        b[from..to]
            .iter()
            .fold(0_u32, |n, d| n * 10 + u32::from(d - b'0'))
    };
    let (year, month, day) = (num(0, 4), num(5, 7), num(8, 10));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day) && num(11, 13) < 24 && num(14, 16) < 60 && num(17, 19) < 60
}

#[cfg(test)]
mod tests {
    use super::is_timestamp;

    #[test]
    fn a_timestamp_is_a_real_date_time_in_the_form_the_os_writes() {
        for good in [
            "2026-10-09T09:33:50.415Z",
            "2028-02-29T00:00:00.000Z",
            "2000-02-29T23:59:59.999Z",
            "0000-02-29T00:00:00.000Z",
            "0300-03-01T00:00:00.000Z",
            "9999-12-31T23:59:59.999Z",
        ] {
            assert!(is_timestamp(good), "{good}");
        }
        for bad in [
            "0300-02-29T00:00:00.000Z",
            "1900-02-29T00:00:00.000Z",
            "2026-02-30T00:00:00.000Z",
            "2026-04-31T00:00:00.000Z",
            "2026-13-01T00:00:00.000Z",
            "2026-00-01T00:00:00.000Z",
            "2026-10-00T00:00:00.000Z",
            "2026-10-09T24:00:00.000Z",
            "2026-10-09T23:60:00.000Z",
            "2026-10-09T23:59:60.000Z",
            "-100-10-09T00:00:00.000Z",
            "2026-10-09T00:00:00.000",
            "2026-10-09T00:00:00.0000",
            "2026-10-09T00:00:00Z",
            "2026-10-09 00:00:00.000Z",
            " 2026-10-09T00:00:00.000Z",
            "2026-10-09T00:00:00.000Z\0",
            "\u{ff12}026-10-09T00:00:00.000Z",
            "not-a-date",
            "",
        ] {
            assert!(!is_timestamp(bad), "{bad:?}");
        }
    }
}
