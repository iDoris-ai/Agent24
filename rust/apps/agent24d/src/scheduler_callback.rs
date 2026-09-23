//! `_a24/scheduler/{upsert,delete,list}` — ME4-1.4.1, split into stacked cuts
//! (statutory review requested the split; the logic is unchanged from the
//! single-commit version, only the boundaries moved).
//!
//! **This cut (ME4-1.4.1a) lands the wire types only**: the module-facing
//! `ModuleSpec`/params/result shapes and every free validation function
//! (`validate_key`/`validate_label`/`validate_module_spec`/`validate_dow`/
//! `bound_or_timeout`). Nothing in this file is called from anywhere else in
//! the crate yet — the three `Handler` impls that actually use these types,
//! and the `crate::domain`/`main.rs`/`server.rs` wiring that mounts them,
//! land in the next stacked cut (ME4-1.4.1, `feat/me4-1.4.1-scheduler-callback`).
//! Hence `#![allow(dead_code)]` below: every `pub` item here is unread by
//! anything else IN THIS CUT, but is read by the very next one — a real,
//! temporary state, not a place things get abandoned.
//!
//! See `docs/design/ME4-S1-scheduler-callback.md` §6 (D5) for the full
//! design (also covers the handlers this cut does not yet add) and §11 C5.4/
//! C5.5 for the judgements this cut's own tests are named after.
//!
//! `owner` is always the MOUNT identity in the next cut's `Handler`s — never
//! anything read out of `params`. There is no `owner`/`owner_module` field in
//! any of the three params structs below (`deny_unknown_fields` makes one in
//! the request body a parse error, not a silently-ignored field), and `_meta`
//! — the one place `deny_unknown_fields` does not apply — is never read by
//! anything in this crate (design §6.1/§6.4, judgement C5.5). This cut proves
//! the "unknown field rejected" half of that; the next cut's `Fixture`-based
//! tests prove the "read by nothing, including `_meta`" half end to end.

#![allow(dead_code)]

use agent24_os_proto::drain::RequestLifecycle;
use agent24_os_proto::rpc::{ErrorKind, RpcError};
use agent24_protocol::ScheduleSpec;
use agent24_scheduler::next_fire;
use agent24_store::{ModuleScheduleState, UpsertOutcome};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A module's quota of `schedules` rows (design §6.2/§6.4).
pub const MODULE_SCHEDULE_QUOTA: u32 = 256;
const KEY_MAX: usize = 128;
const LABEL_MAX: usize = 128;
const CRON_EXPR_MAX: usize = 128;
const TZ_MAX: usize = 64;
const AT_TS_MAX: usize = 64;
/// ≥ [`MODULE_SCHEDULE_QUOTA`] + headroom (v2, M3): a module reconciling all
/// 256 keys at start must not be rate-limited by its own quota.
pub const SCHEDULER_RATE_CAPACITY: f64 = 300.0;
pub const SCHEDULER_RATE_REFILL_PER_SEC: f64 = 1.0;

// ── §6.1 params ──────────────────────────────────────────────────────────

/// The module-facing spec. Deliberately NOT `agent24_protocol::ScheduleSpec`
/// itself: that type does not `deny_unknown_fields` inside a variant (it is
/// also the REST wire type, and tightening it there would change the REST
/// contract), so a module sending `{"type":"cron","expr":"…","zone":"UTC"}`
/// would silently drop `zone` rather than fail. This one is strict at every
/// level; [`validate_module_spec`] both validates it and converts it into
/// the real [`ScheduleSpec`] that gets stored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModuleSpec {
    Cron {
        expr: String,
        #[serde(default)]
        tz: Option<String>,
    },
    Every {
        secs: u32,
    },
    At {
        ts: String,
    },
}

impl From<ModuleSpec> for ScheduleSpec {
    fn from(s: ModuleSpec) -> Self {
        match s {
            ModuleSpec::Cron { expr, tz } => ScheduleSpec::Cron { expr, tz },
            ModuleSpec::Every { secs } => ScheduleSpec::Every { secs },
            ModuleSpec::At { ts } => ScheduleSpec::At { ts },
        }
    }
}

fn default_true() -> bool {
    true
}

/// `deny_unknown_fields` (design §6.1): a top-level `owner_module` — or any
/// other field this struct does not name — must fail parsing outright, not
/// be silently dropped. There is deliberately no owner field to drop in the
/// first place; see this module's doc comment.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerUpsertParams {
    pub key: String,
    pub spec: ModuleSpec,
    /// Desired-state semantics (design §6.1): absent means `true`, never
    /// "keep whatever it already was".
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Absent → the key.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    /// Never read by this crate (S1-10) — the one deliberately permissive
    /// escape hatch, same rule as `memory_callback.rs`'s `_meta`.
    #[serde(default)]
    pub _meta: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerDeleteParams {
    pub key: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub _meta: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerListParams {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub _meta: Option<Map<String, Value>>,
}

/// `[a-z0-9._-]{1,128}` (S1-10), byte-exact: no case folding, no trimming.
///
/// # Errors
/// A message naming the rule, for `-32602`.
pub fn validate_key(key: &str) -> Result<(), String> {
    let ok = !key.is_empty()
        && key.len() <= KEY_MAX
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if ok {
        Ok(())
    } else {
        Err(format!("key must match [a-z0-9._-]{{1,{KEY_MAX}}}"))
    }
}

/// Day-of-week names, the only spelling a module row's cron accepts (§6.3).
pub const DOW_NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/// The day-of-week field of a module cron: `*`, or a comma list whose items
/// are `NAME` or `NAME-NAME` (ASCII case-insensitive). **No digits at all**:
/// the `cron` crate numbers days 1..=7 with 1 = Sunday, POSIX numbers them
/// 0..=6 with 0 = Sunday — `1-5` means Sun-Thu to one and Mon-Fri to the
/// other, and the kernel cannot tell which a module meant. Names mean the
/// same thing under both. Steps (`/`), `?`, `L`, `#` are refused too.
///
/// # Errors
/// A message for `-32602`.
pub fn validate_dow(field: &str) -> Result<(), String> {
    const RULE: &str = "day-of-week must be `*` or names (MON..SUN, ranges like \
        MON-FRI, lists like MON,WED,FRI) — numbers are refused because their \
        meaning differs between POSIX cron and the kernel's cron engine";
    if field == "*" {
        return Ok(());
    }
    let is_name = |s: &str| DOW_NAMES.iter().any(|n| n.eq_ignore_ascii_case(s));
    let ok = field.split(',').all(|item| match item.split_once('-') {
        Some((a, b)) => is_name(a) && is_name(b),
        None => is_name(item),
    });
    if ok { Ok(()) } else { Err(RULE.to_owned()) }
}

/// Module rows are stricter than REST rows (design §6.3, "取舍"): the shared
/// `next_fire::validate` PLUS 5-field cron only (a 6-field cron has a
/// seconds field and could fire every second — module rows keep the 60s
/// floor `every` already has), the day-of-week rule above, "day-of-month and
/// day-of-week may not both be restricted" (v2 M4 — POSIX ORs them, the
/// `cron` crate ANDs them), bounded string sizes, and — review round 2, M1 —
/// **the expression must actually be able to fire**: `0 0 30 2 *` (Feb 30),
/// `0 0 31 4 *` (April has 30 days), `0 0 31 2,4,6,9,11 *` (31st of a month
/// that never has one) are all syntactically valid 5-field cron and pass
/// every check above, yet `next_fire` returns `None` for every `now` —
/// upserting one would silently create a dead row (`next_run_at = NULL`
/// forever) that still counts against the module's quota. Checked by asking
/// `next_fire(&spec, now)` once and requiring `Some`; **`At` is deliberately
/// exempt** — an already-past one-shot is a normal, expected shape (a module
/// reconciling the same key after it already fired must not have that
/// upsert start failing).
///
/// Returns the canonical [`ScheduleSpec`] to store — for `At`, `ts` is
/// re-formatted to the second-precision `Z` form (v2 L8) so `+00:00` vs `Z`
/// is never mistaken for a spec change on the next upsert.
///
/// Error messages never echo `next_fire`'s internally-padded 6-field form
/// (review round 2, L6) — a `Cron` variant's own error text is built from
/// the module's OWN `expr` string, never the seconds-prefixed one
/// `next_fire::normalize_cron` constructs internally.
///
/// # Errors
/// A message for `-32602`.
pub fn validate_module_spec(spec: &ModuleSpec, now: DateTime<Utc>) -> Result<ScheduleSpec, String> {
    if let ModuleSpec::Cron { expr, tz } = spec {
        if expr.len() > CRON_EXPR_MAX {
            return Err(format!("cron expr longer than {CRON_EXPR_MAX} bytes"));
        }
        if tz.as_ref().is_some_and(|t| t.len() > TZ_MAX) {
            return Err(format!("tz longer than {TZ_MAX} bytes"));
        }
        let fields: Vec<&str> = expr.split_whitespace().collect();
        let [_, _, dom, _, dow] = fields.as_slice() else {
            return Err(
                "a module schedule's cron must have exactly 5 fields (no seconds field)".to_owned(),
            );
        };
        validate_dow(dow)?;
        if *dom != "*" && *dow != "*" {
            return Err(
                "a module schedule's cron may restrict day-of-month or day-of-week, \
                 not both (POSIX ORs them, the kernel's cron engine ANDs them)"
                    .to_owned(),
            );
        }
    }
    if let ModuleSpec::At { ts } = spec
        && ts.len() > AT_TS_MAX
    {
        return Err(format!("at.ts longer than {AT_TS_MAX} bytes"));
    }
    let mut converted: ScheduleSpec = spec.clone().into();
    // L6: for a `Cron`, any failure from here on is reported using the
    // module's OWN `expr`/`tz` — never `next_fire`'s normalized/padded
    // string, and never the underlying `cron`/`chrono_tz` crate's error
    // text (which may itself echo that normalized string back).
    if let ModuleSpec::Cron { expr, tz } = spec {
        next_fire::validate(&converted).map_err(|_| match tz {
            Some(tz) => format!("invalid cron expression or timezone: `{expr}` / `{tz}`"),
            None => format!("invalid cron expression: `{expr}`"),
        })?;
    } else {
        next_fire::validate(&converted).map_err(|e| e.to_string())?;
    }
    if let ScheduleSpec::At { ts } = &mut converted {
        let t = next_fire::parse_iso(ts).map_err(|e| e.to_string())?;
        *ts = next_fire::fmt_iso(t);
    } else {
        // M1: syntactically valid but semantically impossible (Feb 30, a
        // 31st that no listed month has, …) — `next_fire` is the only thing
        // that actually knows real calendar days, `cron::Schedule::from_str`
        // does not.
        match next_fire::next_fire(&converted, now) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(match spec {
                    ModuleSpec::Cron { expr, .. } => {
                        format!("cron expression `{expr}` never matches a real date")
                    }
                    // `Every` always has a next fire by construction
                    // (`after + secs`); unreachable in practice, but no
                    // internal string leaks even if it somehow weren't.
                    ModuleSpec::At { .. } => unreachable!("At is handled in the branch above"),
                    ModuleSpec::Every { secs } => {
                        format!("every.secs={secs} never matches a real date")
                    }
                });
            }
            Err(_) => {
                return Err(match spec {
                    ModuleSpec::Cron { expr, .. } => format!("invalid cron expression: `{expr}`"),
                    ModuleSpec::Every { secs } => format!("invalid every.secs: {secs}"),
                    ModuleSpec::At { .. } => unreachable!("At is handled in the branch above"),
                });
            }
        }
    }
    Ok(converted)
}

/// Unicode bidirectional-control formatting characters (category Cf, NOT
/// caught by `char::is_control`'s Cc-only definition): `LRE`/`RLE`/`PDF`/
/// `LRO`/`RLO` (U+202A–U+202E) and the isolate family `LRI`/`RLI`/`FSI`/`PDI`
/// (U+2066–U+2069). Review round 2, L6: these can make a label DISPLAY in an
/// order that does not match its byte content (the "Trojan Source" class of
/// attack) — a label surfaced verbatim in a desktop UI (design §8.2) must not
/// be able to visually impersonate something else.
const BIDI_CONTROLS: [char; 9] = [
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}',
];

/// # Errors
/// A message for `-32602`.
pub fn validate_label(label: Option<&str>, key: &str) -> Result<String, String> {
    let label = label.unwrap_or(key);
    if label.is_empty()
        || label.chars().count() > LABEL_MAX
        || label
            .chars()
            .any(|c| char::is_control(c) || BIDI_CONTROLS.contains(&c))
    {
        return Err(format!(
            "label must be 1..={LABEL_MAX} characters, no control or bidirectional-override \
             characters"
        ));
    }
    Ok(label.to_owned())
}

/// v2 (M8), the same rule as the S2 model callback (design §6.4 step 3b): a
/// call that CARRIES a `request_id` must be bound to that request.
/// `admit_callback_bound` answers `Ok(None)` for an unknown id while
/// `Running` (the memory handlers degrade that to an unbound call); the
/// scheduler handlers refuse it instead, so a fired handler's late upsert
/// cannot silently become unbound background work.
///
/// # Errors
/// `timeout` with `data.retryable = false`.
pub fn bound_or_timeout(
    request_id: Option<&str>,
    admitted: Option<RequestLifecycle>,
) -> Result<Option<RequestLifecycle>, RpcError> {
    match (request_id, admitted) {
        (Some(_), None) => Err(RpcError::application(
            ErrorKind::Timeout,
            "request_id is not (or no longer) in flight; send no request_id for \
             background work",
        )
        .with_data("retryable", Value::Bool(false))),
        (_, lifecycle) => Ok(lifecycle),
    }
}

// ── results ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct UpsertResult {
    pub outcome: UpsertOutcome,
    pub schedule: ModuleScheduleState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteOutcome {
    Deleted,
    Absent,
}

#[derive(Debug, Serialize)]
pub struct DeleteResult {
    pub outcome: DeleteOutcome,
}

#[derive(Debug, Serialize)]
pub struct ListResult {
    /// Sorted by `key` (the store's `list_module_schedules`); at most
    /// [`MODULE_SCHEDULE_QUOTA`] entries, so no paging (design §6.1).
    pub schedules: Vec<ModuleScheduleState>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::time::{Duration, Instant};

    use agent24_os_proto::drain::Generation;
    use serde_json::json;

    use super::*;

    fn upsert(v: Value) -> Result<SchedulerUpsertParams, String> {
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    // ── C5.5 (the unknown-field half — the "no effect from `_meta`" half ───
    // ── needs the next cut's `Handler`s, and lives there) ───────────────

    #[test]
    fn unknown_fields_fail_at_every_level_meta_is_the_one_exception() {
        assert!(upsert(json!({"key":"a","spec":{"type":"every","secs":60}})).is_ok());
        // top level: an `owner`/`module` key is a parse error, not ignored
        assert!(
            upsert(json!({"key":"a","spec":{"type":"every","secs":60},"owner_module":"x"}))
                .is_err()
        );
        // nested: inside the spec variant too
        assert!(upsert(json!({"key":"a","spec":{"type":"every","secs":60,"x":1}})).is_err());
        assert!(
            upsert(json!({"key":"a","spec":{"type":"cron","expr":"0 9 * * *","zone":"UTC"}}))
                .is_err()
        );
        // `_meta` is the one permissive place, and parses regardless of its
        // contents — it just isn't consulted by anything in this crate
        // (proven end to end by the next cut's `Handler`-level tests).
        let p = upsert(json!({
            "key":"a","spec":{"type":"at","ts":"2030-01-01T00:00:00Z"},
            "_meta":{"owner_module":"other","org":"x"},
        }))
        .unwrap();
        assert!(p.enabled && p.label.is_none());

        assert!(
            serde_json::from_value::<SchedulerDeleteParams>(json!({"key":"a","owner_module":"x"}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<SchedulerListParams>(json!({"owner_module":"x"})).is_err()
        );
    }

    // ── C5.4: key / label / spec validation ─────────────────────────────

    #[test]
    fn key_rule_is_exact() {
        for ok in ["a", "routine.x", "r-1_2", &"k".repeat(128)] {
            assert!(validate_key(ok).is_ok(), "{ok}");
        }
        for bad in ["", "A", "a b", "a/b", "é", &"k".repeat(129)] {
            assert!(validate_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn label_rule_is_exact() {
        assert_eq!(validate_label(None, "k").unwrap(), "k");
        assert_eq!(validate_label(Some("my label"), "k").unwrap(), "my label");
        assert!(validate_label(Some(""), "k").is_err());
        assert!(validate_label(Some("\u{0007}bell"), "k").is_err());
        assert!(validate_label(Some(&"x".repeat(129)), "k").is_err());
        assert!(validate_label(Some(&"x".repeat(128)), "k").is_ok());
        // Review round 2, L6: bidirectional-override/isolate formatting
        // characters — none of them are `char::is_control` — must also be
        // refused (Trojan-Source-style visual spoofing of a displayed label).
        for bidi in ['\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}'] {
            let label = format!("safe{bidi}looking");
            assert!(validate_label(Some(&label), "k").is_err(), "{label:?}");
        }
    }

    /// A fixed "now" for tests that only care that SOME real fire exists,
    /// not exactly when.
    fn now_for_tests() -> DateTime<Utc> {
        next_fire::parse_iso("2026-01-01T00:00:00Z").unwrap()
    }

    #[test]
    fn module_spec_is_stricter_than_rest() {
        let five = ModuleSpec::Cron {
            expr: "0 9 * * *".into(),
            tz: None,
        };
        assert!(validate_module_spec(&five, now_for_tests()).is_ok());
        let six = ModuleSpec::Cron {
            expr: "* * * * * *".into(),
            tz: None,
        };
        assert!(validate_module_spec(&six, now_for_tests()).is_err());
        // positive control: the same 6-field expr IS valid for REST rows
        assert!(next_fire::validate(&six.into()).is_ok());
        assert!(validate_module_spec(&ModuleSpec::Every { secs: 59 }, now_for_tests()).is_err());
        assert!(
            validate_module_spec(
                &ModuleSpec::Cron {
                    expr: "nope".into(),
                    tz: None
                },
                now_for_tests()
            )
            .is_err()
        );
    }

    /// Review round 2, L2: a 5-field cron that IS the right shape but has an
    /// out-of-range field (minute 61) is still a syntax error, same as a
    /// nonsense string — `validate_module_spec` must not accidentally accept
    /// it just because the field COUNT is right.
    #[test]
    fn a_five_field_cron_with_an_out_of_range_field_is_still_rejected() {
        let cron = |e: &str| ModuleSpec::Cron {
            expr: e.into(),
            tz: None,
        };
        assert!(validate_module_spec(&cron("61 7 * * *"), now_for_tests()).is_err());
        // positive control: the same shape, in range, is fine
        assert!(validate_module_spec(&cron("59 7 * * *"), now_for_tests()).is_ok());
    }

    /// Review round 2, L2: an unknown IANA timezone is rejected; a real one
    /// is accepted; the match is byte-exact (IANA names ARE case-sensitive —
    /// `asia/shanghai` is not `Asia/Shanghai` to any real tz database).
    #[test]
    fn tz_must_be_a_real_iana_name_case_sensitively() {
        let cron = |tz: &str| ModuleSpec::Cron {
            expr: "0 9 * * *".into(),
            tz: Some(tz.into()),
        };
        assert!(validate_module_spec(&cron("Mars/Olympus"), now_for_tests()).is_err());
        assert!(validate_module_spec(&cron("asia/shanghai"), now_for_tests()).is_err());
        assert!(validate_module_spec(&cron("Asia/Shanghai"), now_for_tests()).is_ok());
    }

    #[test]
    fn at_ts_has_a_byte_length_ceiling() {
        let too_long = format!("2030-01-01T00:00:00+{}:00", "1".repeat(60));
        assert!(too_long.len() > 64);
        assert!(validate_module_spec(&ModuleSpec::At { ts: too_long }, now_for_tests()).is_err());
        assert!(
            validate_module_spec(
                &ModuleSpec::At {
                    ts: "2030-01-01T00:00:00Z".into()
                },
                now_for_tests()
            )
            .is_ok()
        );
    }

    /// Review round 2, M1: syntactically valid but calendrically impossible
    /// cron expressions are `-32602`, not a silently-dead row. `At` is
    /// deliberately exempt — an already-past one-shot must still upsert
    /// cleanly (a module's routine reconciliation of an already-fired key).
    #[test]
    fn cron_expressions_that_never_match_a_real_date_are_rejected() {
        let cron = |e: &str| ModuleSpec::Cron {
            expr: e.into(),
            tz: None,
        };
        for never in ["0 0 30 2 *", "0 0 31 4 *", "0 0 31 2,4,6,9,11 *"] {
            let err = validate_module_spec(&cron(never), now_for_tests()).unwrap_err();
            assert!(
                err.contains(never),
                "the message must name the module's own expression, not an internal \
                 normalized form: {err:?}"
            );
        }
        // Positive control: the 29th exists in some Februaries (leap years)
        // — `next_fire` finds one eventually, so this is accepted.
        assert!(validate_module_spec(&cron("0 0 29 2 *"), now_for_tests()).is_ok());

        // Positive control: an ALREADY-PAST `At` is accepted, not rejected —
        // it is not "never fires", it is "already fired".
        assert!(
            validate_module_spec(
                &ModuleSpec::At {
                    ts: "2000-01-01T00:00:00Z".into()
                },
                now_for_tests()
            )
            .is_ok()
        );
    }

    #[test]
    fn dow_numbers_are_refused_names_are_not() {
        let cron = |e: &str| ModuleSpec::Cron {
            expr: e.into(),
            tz: None,
        };
        for bad in [
            "0 7 * * 1-5",
            "0 7 * * 0",
            "0 7 * * 7",
            "0 7 * * MON,3",
            "0 7 * * */2",
            "0 7 * * MON-FRI/2",
            "0 7 * * ?",
            "0 7 * * MONDAY",
            "0 7 * * 5L",
            "0 7 * * MON#1",
        ] {
            assert!(
                validate_module_spec(&cron(bad), now_for_tests()).is_err(),
                "{bad}"
            );
        }
        // positive control: the numeric form IS accepted on the REST path
        // (unchanged there this round — see §6.3's trade-off)
        assert!(next_fire::validate(&cron("0 7 * * 1-5").into()).is_ok());
        for good in [
            "0 7 * * *",
            "0 7 * * MON-FRI",
            "0 7 * * mon-fri",
            "0 7 * * SAT,SUN",
            "0 7 * * MON,WED-FRI",
        ] {
            assert!(
                validate_module_spec(&cron(good), now_for_tests()).is_ok(),
                "{good}"
            );
        }
    }

    #[test]
    fn dom_and_dow_together_are_refused_and_at_is_canonical() {
        let cron = |e: &str| ModuleSpec::Cron {
            expr: e.into(),
            tz: None,
        };
        assert!(validate_module_spec(&cron("0 7 1 * MON"), now_for_tests()).is_err());
        assert!(validate_module_spec(&cron("0 7 1-7 * MON-FRI"), now_for_tests()).is_err());
        // positive controls: either one alone
        assert!(validate_module_spec(&cron("0 7 1 * *"), now_for_tests()).is_ok());
        assert!(validate_module_spec(&cron("0 7 * * MON"), now_for_tests()).is_ok());
        let a = validate_module_spec(
            &ModuleSpec::At {
                ts: "2030-01-01T08:00:00+08:00".into(),
            },
            now_for_tests(),
        )
        .unwrap();
        let b = validate_module_spec(
            &ModuleSpec::At {
                ts: "2030-01-01T00:00:00.000Z".into(),
            },
            now_for_tests(),
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a,
            ScheduleSpec::At {
                ts: "2030-01-01T00:00:00Z".into()
            }
        );
    }

    #[test]
    fn mon_fri_first_fire_is_a_monday() {
        use chrono::Datelike;
        // Fixed start: Saturday 2026-09-26 12:00Z.
        let sat = next_fire::parse_iso("2026-09-26T12:00:00Z").unwrap();
        assert_eq!(sat.weekday(), chrono::Weekday::Sat);
        let spec = validate_module_spec(
            &ModuleSpec::Cron {
                expr: "0 7 * * MON-FRI".into(),
                tz: None,
            },
            sat,
        )
        .unwrap();
        let first = next_fire::next_fire(&spec, sat).unwrap().unwrap();
        assert_eq!(first, next_fire::parse_iso("2026-09-28T07:00:00Z").unwrap());
        assert_eq!(first.weekday(), chrono::Weekday::Mon);
        // and the lower-case spelling means the same
        let lower = validate_module_spec(
            &ModuleSpec::Cron {
                expr: "0 7 * * mon-fri".into(),
                tz: None,
            },
            sat,
        )
        .unwrap();
        assert_eq!(next_fire::next_fire(&lower, sat).unwrap(), Some(first));
        // negative control documenting WHY numbers are refused: the engine
        // reads `1-5` as Sun..Thu, so from Saturday it fires on SUNDAY.
        let numeric = ScheduleSpec::Cron {
            expr: "0 7 * * 1-5".into(),
            tz: None,
        };
        let n = next_fire::next_fire(&numeric, sat).unwrap().unwrap();
        assert_eq!(n.weekday(), chrono::Weekday::Sun);
    }

    // ── `bound_or_timeout` (used by the next cut's three `Handler`s) ────

    #[test]
    fn bound_or_timeout_rules() {
        // No request_id at all: unbound, always fine.
        assert!(bound_or_timeout(None, None).unwrap().is_none());

        // A request_id given but not admitted (unknown/ended while Running,
        // or `Starting`/`Draining` already refused earlier): timeout,
        // explicitly not retryable.
        let err = bound_or_timeout(Some("r1"), None).unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Timeout));
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d.get("retryable"))
                .and_then(Value::as_bool),
            Some(false)
        );

        // A genuinely live lifecycle passes through unchanged.
        let g = Generation::serving_at("/tmp/does-not-need-to-exist".into());
        assert!(g.ready());
        let live = g
            .admit_request(
                "r2".to_owned(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let lifecycle = g.request_lifecycle("r2");
        assert!(bound_or_timeout(Some("r2"), lifecycle).unwrap().is_some());
        drop(live);
    }
}
