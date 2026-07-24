//! Pure next-fire computation for the three schedule specs (no I/O, no clock —
//! every input is explicit so this is exhaustively unit-testable, including
//! DST transitions).

use std::str::FromStr;

use agent24_protocol::ScheduleSpec;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule as CronSchedule;

/// `every.secs` bounds (SPEC-002 §1.5): at least a minute, at most a day.
pub const MIN_EVERY_SECS: u32 = 60;
pub const MAX_EVERY_SECS: u32 = 86_400;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpecError {
    #[error("invalid schedule: {0}")]
    Invalid(String),
}

/// Parse an ISO-8601/RFC-3339 timestamp into a UTC instant.
pub fn parse_iso(ts: &str) -> Result<DateTime<Utc>, SpecError> {
    DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| SpecError::Invalid(format!("invalid timestamp {ts}: {e}")))
}

/// Format a UTC instant as the wire timestamp (second precision, `Z`).
pub fn fmt_iso(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The `cron` crate wants a seconds field first (6-7 fields). Accept the POSIX
/// 5-field form by prepending `0` seconds; pass 6/7-field through unchanged.
fn normalize_cron(expr: &str) -> Result<String, SpecError> {
    let fields = expr.split_whitespace().count();
    match fields {
        5 => Ok(format!("0 {expr}")),
        6 | 7 => Ok(expr.to_owned()),
        n => Err(SpecError::Invalid(format!(
            "cron expression must have 5-7 fields, got {n}"
        ))),
    }
}

/// Validate a spec without computing a time (used by CRUD before persisting).
pub fn validate(spec: &ScheduleSpec) -> Result<(), SpecError> {
    match spec {
        ScheduleSpec::Cron { expr, tz } => {
            if let Some(tz) = tz {
                Tz::from_str(tz)
                    .map_err(|_| SpecError::Invalid(format!("unknown timezone: {tz}")))?;
            }
            let normalized = normalize_cron(expr)?;
            CronSchedule::from_str(&normalized)
                .map_err(|e| SpecError::Invalid(format!("invalid cron expression: {e}")))?;
            Ok(())
        }
        ScheduleSpec::Every { secs } => {
            if !(MIN_EVERY_SECS..=MAX_EVERY_SECS).contains(secs) {
                return Err(SpecError::Invalid(format!(
                    "every.secs must be {MIN_EVERY_SECS}..={MAX_EVERY_SECS}, got {secs}"
                )));
            }
            Ok(())
        }
        ScheduleSpec::At { ts } => {
            parse_iso(ts)?;
            Ok(())
        }
    }
}

/// The next firing strictly after `after`, or `None` when the spec will never
/// fire again (a one-shot `at` already in the past). `after` is the reference
/// instant — passing "now" gives skip-missed semantics (a schedule that lay
/// due while the daemon was down jumps straight to its next future slot rather
/// than replaying every missed occurrence).
pub fn next_fire(
    spec: &ScheduleSpec,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, SpecError> {
    match spec {
        ScheduleSpec::Cron { expr, tz } => {
            let tz = match tz {
                Some(tz) => Tz::from_str(tz)
                    .map_err(|_| SpecError::Invalid(format!("unknown timezone: {tz}")))?,
                None => chrono_tz::UTC,
            };
            let normalized = normalize_cron(expr)?;
            let schedule = CronSchedule::from_str(&normalized)
                .map_err(|e| SpecError::Invalid(format!("invalid cron expression: {e}")))?;
            // Compute occurrences in the schedule's own timezone (so DST is
            // honoured), then convert back to UTC.
            let after_local = after.with_timezone(&tz);
            Ok(schedule
                .after(&after_local)
                .next()
                .map(|dt| dt.with_timezone(&Utc)))
        }
        ScheduleSpec::Every { secs } => {
            if !(MIN_EVERY_SECS..=MAX_EVERY_SECS).contains(secs) {
                return Err(SpecError::Invalid(format!(
                    "every.secs must be {MIN_EVERY_SECS}..={MAX_EVERY_SECS}, got {secs}"
                )));
            }
            Ok(Some(after + chrono::Duration::seconds(i64::from(*secs))))
        }
        ScheduleSpec::At { ts } => {
            let at = parse_iso(ts)?;
            Ok((at > after).then_some(at))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        parse_iso(s).unwrap()
    }

    #[test]
    fn every_advances_by_secs() {
        let spec = ScheduleSpec::Every { secs: 3600 };
        let next = next_fire(&spec, utc("2026-07-24T10:00:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!(fmt_iso(next), "2026-07-24T11:00:00Z");
    }

    #[test]
    fn every_out_of_range_is_invalid() {
        assert!(
            next_fire(
                &ScheduleSpec::Every { secs: 59 },
                utc("2026-07-24T10:00:00Z")
            )
            .is_err()
        );
        assert!(
            next_fire(
                &ScheduleSpec::Every { secs: 86_401 },
                utc("2026-07-24T10:00:00Z")
            )
            .is_err()
        );
        assert!(validate(&ScheduleSpec::Every { secs: 60 }).is_ok());
    }

    #[test]
    fn five_field_cron_daily_at_8() {
        // 08:00 every day, UTC
        let spec = ScheduleSpec::Cron {
            expr: "0 8 * * *".to_owned(),
            tz: None,
        };
        let next = next_fire(&spec, utc("2026-07-24T09:00:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!(fmt_iso(next), "2026-07-25T08:00:00Z");
        let same_day = next_fire(&spec, utc("2026-07-24T07:00:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!(fmt_iso(same_day), "2026-07-24T08:00:00Z");
    }

    #[test]
    fn six_field_cron_with_seconds() {
        // at 30s past every minute
        let spec = ScheduleSpec::Cron {
            expr: "30 * * * * *".to_owned(),
            tz: None,
        };
        let next = next_fire(&spec, utc("2026-07-24T10:00:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!(fmt_iso(next), "2026-07-24T10:00:30Z");
    }

    #[test]
    fn cron_honours_timezone() {
        // 08:00 Asia/Shanghai (UTC+8) == 00:00 UTC
        let spec = ScheduleSpec::Cron {
            expr: "0 8 * * *".to_owned(),
            tz: Some("Asia/Shanghai".to_owned()),
        };
        let next = next_fire(&spec, utc("2026-07-24T10:00:00Z"))
            .unwrap()
            .unwrap();
        // next 08:00 Shanghai after 18:00 Shanghai is the following day 00:00 UTC
        assert_eq!(fmt_iso(next), "2026-07-25T00:00:00Z");
    }

    #[test]
    fn cron_across_us_dst_spring_forward() {
        // America/New_York springs forward 2026-03-08 02:00→03:00.
        // A 02:30 daily job doesn't exist that day; cron rolls to the next
        // valid occurrence rather than firing twice or never.
        let spec = ScheduleSpec::Cron {
            expr: "30 2 * * *".to_owned(),
            tz: Some("America/New_York".to_owned()),
        };
        // reference: 2026-03-08 00:00 EST == 05:00 UTC
        let next = next_fire(&spec, utc("2026-03-08T05:00:00Z"))
            .unwrap()
            .unwrap();
        // 02:30 EST doesn't exist; the next real 02:30 is the following day
        // 2026-03-09 02:30 EDT == 06:30 UTC
        assert_eq!(fmt_iso(next), "2026-03-09T06:30:00Z");
    }

    #[test]
    fn at_fires_once_then_never() {
        let spec = ScheduleSpec::At {
            ts: "2026-07-24T12:00:00Z".to_owned(),
        };
        // before: returns the instant
        let before = next_fire(&spec, utc("2026-07-24T11:00:00Z")).unwrap();
        assert_eq!(before.map(fmt_iso), Some("2026-07-24T12:00:00Z".to_owned()));
        // at/after: never again
        assert_eq!(next_fire(&spec, utc("2026-07-24T12:00:00Z")).unwrap(), None);
        assert_eq!(next_fire(&spec, utc("2026-07-24T13:00:00Z")).unwrap(), None);
    }

    #[test]
    fn invalid_cron_and_tz_rejected() {
        assert!(
            validate(&ScheduleSpec::Cron {
                expr: "nonsense".to_owned(),
                tz: None
            })
            .is_err()
        );
        assert!(
            validate(&ScheduleSpec::Cron {
                expr: "0 8 * * *".to_owned(),
                tz: Some("Mars/Olympus".to_owned())
            })
            .is_err()
        );
        assert!(
            validate(&ScheduleSpec::At {
                ts: "not-a-date".to_owned()
            })
            .is_err()
        );
    }
}
