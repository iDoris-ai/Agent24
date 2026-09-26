//! `_a24/scheduler/{upsert,delete,list}` — the callback handlers an
//! out-of-process module calls to own its own `schedules` rows (ME4-1.4.1).
//! See `docs/design/ME4-S1-scheduler-callback.md` §6 (D5), §10.2/§10.3, §13.
//!
//! Landed as two stacked cuts (statutory review requested the split; the
//! logic is unchanged from the original single-commit version, only the
//! boundaries moved): ME4-1.4.1a (`feat/me4-1.4.1a-scheduler-callback-types`)
//! added everything above this comment's own module doc used to describe —
//! the wire types and every free validation function. **This cut adds the
//! three `Handler` impls** that actually call them, plus the
//! `crate::domain`/`main.rs`/`server.rs` wiring that mounts them.
//!
//! This is the FIRST caller that actually produces module `schedules` rows —
//! `agent24-scheduler`'s `upsert_module`/`delete_module`/`list_module`
//! (ME4-1.2.1, narrow wrappers added this cut — review round 2, L4) and the
//! tick's own recording branch (ME4-1.2.2b3) both predate this file and have
//! been exercised only by their own crates' tests and by hand-inserted rows
//! until now.
//!
//! Shape follows `memory_callback.rs` (three `Handler`s, one struct built
//! fresh per generation inside `crate::domain`'s `MethodsFor` closure) and
//! `events_emit.rs` (the token-bucket rate limiter, the
//! `LifecycleTimeout`/`CallbackRefused` mappings) — deliberately, not by
//! coincidence: design §6.4 says "照 `memory_callback.rs`，不复制 FU-70"
//! (`approval_callback.rs`'s callback handlers skip `bind_to_lifecycle`
//! entirely; this file must not repeat that gap).
//!
//! `owner` is always the MOUNT identity (`crate::domain::build_methods_for`'s
//! `name`, the same string used for `_a24/approval/*`'s `module` field) —
//! never anything read out of `params`. There is no `owner`/`owner_module`
//! field in any of the three params structs (`deny_unknown_fields` makes one
//! in the request body a parse error, not a silently-ignored field), and
//! `_meta` — the one place `deny_unknown_fields` does not apply — is never
//! read by anything in this module (design §6.1/§6.4, judgement C5.5).

use std::sync::Arc;

use agent24_domain::{Capability, Grants};
use agent24_os_proto::drain::{Generation, LifecycleTimeout, RequestLifecycle, bind_to_lifecycle};
use agent24_os_proto::rpc::{CallFuture, ErrorKind, Handler, RpcError};
use agent24_protocol::ScheduleSpec;
use agent24_scheduler::Scheduler;
use agent24_scheduler::next_fire;
use agent24_store::{ModuleScheduleDesired, ModuleScheduleState, StoreError, UpsertOutcome};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::events_emit::RateLimiter;
use crate::events_emit::refused_error;

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

/// The "capability withheld" response, worded identically for all three
/// methods — same shape as `memory_callback.rs`'s `forbidden()`.
fn forbidden() -> RpcError {
    RpcError::application(
        ErrorKind::Forbidden,
        "this module was not granted the scheduler capability",
    )
}

/// `bind_to_lifecycle`'s two timeout branches, mapped the same way
/// `events_emit.rs`/`os_memory_page.rs` map them: worded so "budget
/// exhausted" never presumes the request ended, and vice versa.
fn lifecycle_timeout_error(e: LifecycleTimeout) -> RpcError {
    match e {
        LifecycleTimeout::BudgetExhausted => RpcError::application(
            ErrorKind::Timeout,
            "this callback's request-bound time budget was exhausted",
        ),
        LifecycleTimeout::RequestEnded => RpcError::application(
            ErrorKind::Timeout,
            "the request this callback was bound to has already ended",
        ),
    }
}

/// Design §6.4 step 6: a storage error's message is fixed and never carries
/// SQL, a path, or an owner name (SPEC §3, "`error.data` 不得出现内核内部信息").
/// [`StoreError::QuotaExceeded`] is handled by each `call()` separately
/// (`quota_exceeded`, not `-32603`) — this is the catch-all for everything
/// else (`Sqlx`/`Migrate`/`Serde`/`Transition`/`NotFound`/`Conflict`).
/// Review round 2, L5: the real error and the owner it happened to ARE
/// logged server-side (`tracing`, never sent over the wire) so an operator
/// can actually diagnose it — sanitizing the wire response is not the same
/// as throwing the information away.
fn storage_error(owner: &str, e: StoreError) -> RpcError {
    tracing::warn!(error = %e, owner, "scheduler callback storage error");
    RpcError::internal("storage error")
}

// ── upsert ───────────────────────────────────────────────────────────────

/// `_a24/scheduler/upsert`. One is built fresh every time a module's
/// generation's `MethodsFor` closure runs (`crate::domain::build_methods_for`);
/// `owner` is that mount's identity (the module's own name), never anything
/// read from `params`.
pub struct SchedulerUpsertHandler {
    pub generation: Arc<Generation>,
    pub owner: String,
    pub granted: Grants,
    pub scheduler: Arc<Scheduler>,
    pub limiter: Arc<RateLimiter>,
}

impl Handler for SchedulerUpsertHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        let parsed: SchedulerUpsertParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        validate_key(&parsed.key)?;
        validate_label(parsed.label.as_deref(), &parsed.key)?;
        validate_module_spec(&parsed.spec, Utc::now())?;
        Ok(())
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = serde_json::from_value::<SchedulerUpsertParams>(params);
        let generation = self.generation.clone();
        let granted = self.granted.clone();
        let scheduler = self.scheduler.clone();
        let limiter = self.limiter.clone();
        let owner = self.owner.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params valid at check_params but not at call(): {e}"
                ))
            })?;

            if !granted.has(Capability::Scheduler) {
                return Err(forbidden());
            }

            let lifecycle = match generation.admit_callback_bound(parsed.request_id.as_deref()) {
                Ok(lifecycle) => lifecycle,
                Err(refused) => return Err(refused_error(refused)),
            };
            let lifecycle = bound_or_timeout(parsed.request_id.as_deref(), lifecycle)?;

            if !limiter.try_acquire() {
                return Err(RpcError::application(
                    ErrorKind::RateLimited,
                    "this module's scheduler callback rate limit is exhausted",
                ));
            }

            // Re-derive the canonical label/spec — `check_params` already
            // proved these succeed; re-running them here (rather than
            // threading the result through) is the same shape
            // `memory_callback.rs` uses for its own re-parse. A failure here
            // would mean `check_params` and `call()` disagree, which is a
            // kernel bug, not a caller mistake.
            let now = Utc::now();
            let label = validate_label(parsed.label.as_deref(), &parsed.key).map_err(|e| {
                RpcError::internal(format!(
                    "params valid at check_params but not at call(): {e}"
                ))
            })?;
            let spec = validate_module_spec(&parsed.spec, now).map_err(|e| {
                RpcError::internal(format!(
                    "params valid at check_params but not at call(): {e}"
                ))
            })?;
            let key = parsed.key;
            let enabled = parsed.enabled;

            // Review round 2, M1 companion: `check_params` already proved
            // `next_fire(&spec, <a nearby "now">)` is `Some` for a Cron/Every
            // spec (or that the spec is an `At`, exempt from that check) —
            // an `Err` HERE, using THIS call's own `now`, is therefore a
            // genuine internal inconsistency, not a caller mistake, and must
            // not be silently swallowed into "no next run" (`.ok().flatten()`
            // used to do exactly that).
            let next_if_recomputed = match next_fire::next_fire(&spec, now) {
                Ok(next) => next.map(next_fire::fmt_iso),
                Err(e) => {
                    return Err(RpcError::internal(format!(
                        "schedule spec passed validation but next_fire failed \
                         unexpectedly: {e}"
                    )));
                }
            };
            let now_str = next_fire::fmt_iso(now);

            let owner_for_op = owner.clone();
            let store_op = async move {
                let new_id = format!("sch_{}", agent24_core::util::ulid());
                let desired = ModuleScheduleDesired {
                    spec,
                    enabled,
                    label,
                };
                scheduler
                    .upsert_module(
                        &new_id,
                        &owner_for_op,
                        &key,
                        &desired,
                        next_if_recomputed.as_deref(),
                        &now_str,
                        MODULE_SCHEDULE_QUOTA,
                    )
                    .await
            };

            match bind_to_lifecycle(lifecycle, store_op).await {
                Ok(Ok((outcome, schedule))) => {
                    serde_json::to_value(UpsertResult { outcome, schedule })
                        .map_err(|e| RpcError::internal(format!("result not serialisable: {e}")))
                }
                Ok(Err(StoreError::QuotaExceeded)) => Err(RpcError::application(
                    ErrorKind::QuotaExceeded,
                    "this module already owns the maximum number of schedules",
                )),
                Ok(Err(e)) => Err(storage_error(&owner, e)),
                Err(e) => Err(lifecycle_timeout_error(e)),
            }
        })
    }
}

// ── delete ───────────────────────────────────────────────────────────────

pub struct SchedulerDeleteHandler {
    pub generation: Arc<Generation>,
    pub owner: String,
    pub granted: Grants,
    pub scheduler: Arc<Scheduler>,
    pub limiter: Arc<RateLimiter>,
}

impl Handler for SchedulerDeleteHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        let parsed: SchedulerDeleteParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        // Review round 2, L1: a malformed key is rejected here too, not just
        // for `upsert` — `delete{key:"A"}` and an over-long key are both
        // `-32602`, the same rule `validate_key` already states.
        validate_key(&parsed.key)?;
        Ok(())
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = serde_json::from_value::<SchedulerDeleteParams>(params);
        let generation = self.generation.clone();
        let granted = self.granted.clone();
        let scheduler = self.scheduler.clone();
        let limiter = self.limiter.clone();
        let owner = self.owner.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params valid at check_params but not at call(): {e}"
                ))
            })?;

            if !granted.has(Capability::Scheduler) {
                return Err(forbidden());
            }

            let lifecycle = match generation.admit_callback_bound(parsed.request_id.as_deref()) {
                Ok(lifecycle) => lifecycle,
                Err(refused) => return Err(refused_error(refused)),
            };
            let lifecycle = bound_or_timeout(parsed.request_id.as_deref(), lifecycle)?;

            if !limiter.try_acquire() {
                return Err(RpcError::application(
                    ErrorKind::RateLimited,
                    "this module's scheduler callback rate limit is exhausted",
                ));
            }

            let key = parsed.key;
            // `delete_module`'s own `WHERE owner_module = ?` scopes this to
            // the caller's own rows (design §6.2): another module's key is
            // simply absent, never an error, and never even visible enough
            // to distinguish from "never existed" — that IS the
            // A-deletes-B's-key isolation judgement (C5.2).
            let owner_for_op = owner.clone();
            let store_op = async move { scheduler.delete_module(&owner_for_op, &key).await };

            match bind_to_lifecycle(lifecycle, store_op).await {
                Ok(Ok(true)) => serde_json::to_value(DeleteResult {
                    outcome: DeleteOutcome::Deleted,
                })
                .map_err(|e| RpcError::internal(format!("result not serialisable: {e}"))),
                Ok(Ok(false)) => serde_json::to_value(DeleteResult {
                    outcome: DeleteOutcome::Absent,
                })
                .map_err(|e| RpcError::internal(format!("result not serialisable: {e}"))),
                Ok(Err(e)) => Err(storage_error(&owner, e)),
                Err(e) => Err(lifecycle_timeout_error(e)),
            }
        })
    }
}

// ── list ─────────────────────────────────────────────────────────────────

pub struct SchedulerListHandler {
    pub generation: Arc<Generation>,
    pub owner: String,
    pub granted: Grants,
    pub scheduler: Arc<Scheduler>,
    pub limiter: Arc<RateLimiter>,
}

impl Handler for SchedulerListHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        serde_json::from_value::<SchedulerListParams>(params.clone())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = serde_json::from_value::<SchedulerListParams>(params);
        let generation = self.generation.clone();
        let granted = self.granted.clone();
        let scheduler = self.scheduler.clone();
        let limiter = self.limiter.clone();
        let owner = self.owner.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params valid at check_params but not at call(): {e}"
                ))
            })?;

            if !granted.has(Capability::Scheduler) {
                return Err(forbidden());
            }

            let lifecycle = match generation.admit_callback_bound(parsed.request_id.as_deref()) {
                Ok(lifecycle) => lifecycle,
                Err(refused) => return Err(refused_error(refused)),
            };
            let lifecycle = bound_or_timeout(parsed.request_id.as_deref(), lifecycle)?;

            if !limiter.try_acquire() {
                return Err(RpcError::application(
                    ErrorKind::RateLimited,
                    "this module's scheduler callback rate limit is exhausted",
                ));
            }

            let owner_for_op = owner.clone();
            let store_op = async move { scheduler.list_module(&owner_for_op).await };

            match bind_to_lifecycle(lifecycle, store_op).await {
                Ok(Ok(schedules)) => serde_json::to_value(ListResult { schedules })
                    .map_err(|e| RpcError::internal(format!("result not serialisable: {e}"))),
                Ok(Err(e)) => Err(storage_error(&owner, e)),
                Err(e) => Err(lifecycle_timeout_error(e)),
            }
        })
    }
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

    use std::sync::Mutex as StdMutex;

    use agent24_scheduler::{DeferReason, FireOutcome, RunTrigger, ScheduleInvocation, Scheduler};

    use crate::events_emit::Clock;

    /// A clock a test can freeze and advance instead of sleeping — the exact
    /// shape `events_emit.rs`'s own rate-limiter tests use (that one is
    /// private to that module, so this is a second, identical copy rather
    /// than a shared export).
    #[derive(Clone)]
    struct TestClock(Arc<StdMutex<Instant>>);

    impl TestClock {
        fn frozen_at(now: Instant) -> Self {
            Self(Arc::new(StdMutex::new(now)))
        }

        fn advance(&self, by: Duration) {
            let mut t = self.0.lock().unwrap();
            *t += by;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }
    }

    /// ME4-1.4.1 has no delivery pump yet (that's ME4-1.3.1) — every fire
    /// these tests' `Scheduler` might in principle be asked to trigger is
    /// answered the same way `KernelTrigger` answers a module row today
    /// (`server.rs`): `Deferred(MountPending)`. Nothing here drives the tick
    /// loop or a real fire, so this is never actually called.
    struct NoopTrigger;

    #[async_trait::async_trait]
    impl RunTrigger for NoopTrigger {
        async fn trigger(&self, _invocation: &ScheduleInvocation) -> FireOutcome {
            FireOutcome::Deferred {
                reason: DeferReason::MountPending,
            }
        }
    }

    fn running_generation() -> Arc<Generation> {
        let g = Generation::serving_at("/tmp/does-not-need-to-exist".into());
        assert!(g.ready(), "a freshly-serving generation must become Ready");
        g
    }

    fn granted() -> Grants {
        Grants::granting(&[Capability::Scheduler], &[Capability::Scheduler])
    }

    fn ungranted() -> Grants {
        Grants::default()
    }

    fn every_secs(secs: u32) -> Value {
        json!({"type": "every", "secs": secs})
    }

    /// One shared, real, file-free store + one shared, real token bucket —
    /// the same pairing `crate::domain::build_methods_for` builds once per
    /// mount (§6.4/§6.5) — behind handler builders parameterised on `owner`
    /// and `granted`, so a test can freely construct two different modules'
    /// (`"a"`/`"b"`) handlers over the SAME underlying rows, or two
    /// different `Generation`s (simulating a restart) over the SAME
    /// `Arc<RateLimiter>`.
    struct Fixture {
        scheduler: Arc<Scheduler>,
        limiter: Arc<RateLimiter>,
        /// Review round 2, L4: `Scheduler` no longer exposes a raw `&Store`
        /// (the callback handlers go through its three narrow wrappers
        /// instead) — a test that needs the raw-SQL escape hatch
        /// (`agent24_store::test_hooks::pool`, the same one
        /// `agent24-scheduler`'s own tests use) keeps its OWN clone of the
        /// `Store` it handed to `Scheduler::new`, made before that move.
        store: agent24_store::Store,
    }

    impl Fixture {
        async fn new() -> Self {
            let store = agent24_store::Store::open_memory().await.unwrap();
            let scheduler = Scheduler::new(
                store.clone(),
                Arc::new(NoopTrigger),
                Arc::new(|_body: agent24_protocol::EventBody| {}),
            );
            Self {
                scheduler,
                limiter: Arc::new(RateLimiter::new(
                    SCHEDULER_RATE_CAPACITY,
                    SCHEDULER_RATE_REFILL_PER_SEC,
                )),
                store,
            }
        }

        /// Same as [`Self::new`], but the token bucket runs on an injected
        /// clock instead of the real one — for a test that must assert
        /// something stays exhausted regardless of how much WALL-CLOCK time
        /// the test process itself takes (a slow CI runner must not turn a
        /// deterministic `rate_limited` assertion into a flake).
        async fn with_clock(clock: Arc<dyn Clock>) -> Self {
            let store = agent24_store::Store::open_memory().await.unwrap();
            let scheduler = Scheduler::new(
                store.clone(),
                Arc::new(NoopTrigger),
                Arc::new(|_body: agent24_protocol::EventBody| {}),
            );
            Self {
                scheduler,
                limiter: Arc::new(RateLimiter::with_clock(
                    SCHEDULER_RATE_CAPACITY,
                    SCHEDULER_RATE_REFILL_PER_SEC,
                    clock,
                )),
                store,
            }
        }

        fn upsert(
            &self,
            generation: &Arc<Generation>,
            owner: &str,
            granted: Grants,
        ) -> SchedulerUpsertHandler {
            SchedulerUpsertHandler {
                generation: generation.clone(),
                owner: owner.to_owned(),
                granted,
                scheduler: self.scheduler.clone(),
                limiter: self.limiter.clone(),
            }
        }

        fn delete(
            &self,
            generation: &Arc<Generation>,
            owner: &str,
            granted: Grants,
        ) -> SchedulerDeleteHandler {
            SchedulerDeleteHandler {
                generation: generation.clone(),
                owner: owner.to_owned(),
                granted,
                scheduler: self.scheduler.clone(),
                limiter: self.limiter.clone(),
            }
        }

        fn list(
            &self,
            generation: &Arc<Generation>,
            owner: &str,
            granted: Grants,
        ) -> SchedulerListHandler {
            SchedulerListHandler {
                generation: generation.clone(),
                owner: owner.to_owned(),
                granted,
                scheduler: self.scheduler.clone(),
                limiter: self.limiter.clone(),
            }
        }
    }

    // ── C5.1: forbidden without the grant; a real success with it ──────────

    #[tokio::test]
    async fn c5_1_forbidden_without_grant_succeeds_with_it() {
        let fx = Fixture::new().await;
        let g = running_generation();

        let err = fx
            .upsert(&g, "mod-a", ungranted())
            .call(json!({"key": "k", "spec": every_secs(3600)}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Forbidden));

        let ok = fx
            .upsert(&g, "mod-a", granted())
            .call(json!({"key": "k", "spec": every_secs(3600)}))
            .await
            .unwrap();
        assert_eq!(ok["outcome"], "created");
    }

    // ── C5.2: A deletes B's key → absent, B's row untouched and invisible ──

    #[tokio::test]
    async fn c5_2_a_deleting_bs_key_is_absent_and_leaves_bs_row_alone() {
        let fx = Fixture::new().await;
        let g = running_generation();

        fx.upsert(&g, "b", granted())
            .call(json!({"key": "shared", "spec": every_secs(3600)}))
            .await
            .unwrap();

        let deleted = fx
            .delete(&g, "a", granted())
            .call(json!({"key": "shared"}))
            .await
            .unwrap();
        assert_eq!(deleted["outcome"], "absent");

        let list_a = fx.list(&g, "a", granted()).call(json!({})).await.unwrap();
        assert!(list_a["schedules"].as_array().unwrap().is_empty());

        let list_b = fx.list(&g, "b", granted()).call(json!({})).await.unwrap();
        assert_eq!(list_b["schedules"].as_array().unwrap().len(), 1);
        assert_eq!(list_b["schedules"][0]["key"], "shared");

        // Positive control: B deleting its own key really does delete it.
        let deleted_by_owner = fx
            .delete(&g, "b", granted())
            .call(json!({"key": "shared"}))
            .await
            .unwrap();
        assert_eq!(deleted_by_owner["outcome"], "deleted");
    }

    // ── C5.3: the 257th key is quota_exceeded; existing keys stay usable ───

    #[tokio::test]
    async fn c5_3_the_257th_key_is_quota_exceeded() {
        let fx = Fixture::new().await;
        let g = running_generation();
        let h = fx.upsert(&g, "m", granted());

        for i in 0..256 {
            let res = h
                .call(json!({"key": format!("k{i}"), "spec": every_secs(3600)}))
                .await
                .unwrap();
            assert_eq!(res["outcome"], "created", "key k{i}");
        }

        let err = h
            .call(json!({"key": "k256", "spec": every_secs(3600)}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::QuotaExceeded));

        // Positive control: an EXISTING key is still upsertable at full quota.
        let ok = h
            .call(json!({"key": "k0", "spec": every_secs(3600)}))
            .await
            .unwrap();
        assert_eq!(ok["outcome"], "unchanged");
    }

    // ── L1: delete also rejects a malformed key (not just upsert) ──────────

    #[tokio::test]
    async fn delete_also_rejects_a_malformed_key() {
        let fx = Fixture::new().await;
        let g = running_generation();
        let h = fx.delete(&g, "m", granted());

        assert!(h.check_params(&json!({"key": "A"})).is_err());
        assert!(h.check_params(&json!({"key": &"k".repeat(129)})).is_err());
        // Positive control: a valid (if never-created) key parses fine and
        // is simply `absent`.
        assert!(h.check_params(&json!({"key": "fine"})).is_ok());
        let res = h.call(json!({"key": "fine"})).await.unwrap();
        assert_eq!(res["outcome"], "absent");
    }

    // ── C5.5: `_meta` owner injection is inert; top-level/nested unknown ───
    // ── fields are -32602 ───────────────────────────────────────────────

    #[tokio::test]
    async fn c5_5_meta_owner_is_inert_top_level_and_nested_unknown_fields_reject() {
        let fx = Fixture::new().await;
        let g = running_generation();

        // `_meta.owner_module` carries a DIFFERENT module's name — the row
        // must still land under the CALLER's own identity ("a"), never "b".
        let res = fx
            .upsert(&g, "a", granted())
            .call(json!({
                "key": "k",
                "spec": every_secs(3600),
                "_meta": {"owner_module": "b", "org": "attacker-org"},
            }))
            .await
            .unwrap();
        assert_eq!(res["outcome"], "created");

        let list_b = fx.list(&g, "b", granted()).call(json!({})).await.unwrap();
        assert!(
            list_b["schedules"].as_array().unwrap().is_empty(),
            "`_meta.owner_module` must have no effect on which module owns the row"
        );
        let list_a = fx.list(&g, "a", granted()).call(json!({})).await.unwrap();
        assert_eq!(list_a["schedules"][0]["key"], "k");

        let h = fx.upsert(&g, "a", granted());
        assert!(
            h.check_params(&json!({
                "key": "k2", "spec": every_secs(3600), "owner_module": "x",
            }))
            .is_err(),
            "a top-level `owner_module` field must fail to parse"
        );
        assert!(
            h.check_params(&json!({
                "key": "k3",
                "spec": {"type": "every", "secs": 3600, "x": 1},
            }))
            .is_err(),
            "an unknown field INSIDE the spec variant must fail to parse"
        );
        assert!(
            h.check_params(&json!({
                "key": "k4",
                "spec": {"type": "cron", "expr": "0 9 * * *", "zone": "UTC"},
            }))
            .is_err(),
            "an unknown field inside a cron spec must fail to parse"
        );
    }

    // ── C5.6: `list` echoes the full expected state, never `schedule_id` ───

    #[tokio::test]
    async fn c5_6_list_returns_full_expected_state_and_never_schedule_id() {
        let fx = Fixture::new().await;
        let g = running_generation();

        fx.upsert(&g, "m", granted())
            .call(json!({"key": "k", "spec": every_secs(3600)}))
            .await
            .unwrap();

        // Review round 2, L7: `user_suspended` is set through the REAL
        // `Scheduler::suspend` (the same method REST `POST .../suspend`
        // calls) rather than raw SQL. `system_disabled_reason` still has no
        // production API in this task's scope (that's the delivery pump,
        // ME4-1.3.1) — it, and the delivery row `last_fire` reads from, are
        // still forced directly via `agent24_store::test_hooks::pool`, the
        // same escape hatch `agent24-scheduler`'s own tests use.
        let rows = fx.scheduler.list().await.unwrap();
        let schedule_id = rows
            .iter()
            .find(|r| {
                r.owner
                    .as_ref()
                    .is_some_and(|o| o.module == "m" && o.key == "k")
            })
            .unwrap()
            .id
            .clone();
        fx.scheduler
            .suspend(&schedule_id, chrono::Utc::now())
            .await
            .unwrap();

        let pool = agent24_store::test_hooks::pool(&fx.store);
        sqlx::query(
            "UPDATE schedules SET system_disabled_reason = 'consecutive_failures' \
             WHERE owner_module = 'm' AND module_key = 'k'",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO schedule_deliveries \
             (fire_id, schedule_id, owner_module, module_key, scheduled_for, fired_at, \
              fire_trigger, status, attempts, next_attempt_at, expires_at, last_error, \
              created_at, updated_at) \
             VALUES ('fire_probe', ?, 'm', 'k', '2026-01-01T00:00:00Z', \
                     '2026-01-01T00:00:00Z', 'tick', 'delivered', 1, NULL, \
                     '2026-01-02T00:00:00Z', NULL, '2026-01-01T00:00:00Z', \
                     '2026-01-01T00:00:00Z')",
        )
        .bind(&schedule_id)
        .execute(pool)
        .await
        .unwrap();

        let listed = fx.list(&g, "m", granted()).call(json!({})).await.unwrap();
        let row = &listed["schedules"][0];
        assert_eq!(row["key"], "k");
        assert_eq!(row["user_suspended"], true);
        assert_eq!(row["system_disabled_reason"], "consecutive_failures");
        assert_eq!(row["last_fire"]["tick"]["fire_id"], "fire_probe");
        assert_eq!(row["last_fire"]["tick"]["status"], "delivered");
        assert!(row["last_fire"]["run_now"].is_null());
        assert!(
            row.get("schedule_id").is_none(),
            "the internal schedule id must never be echoed to the module: {row}"
        );
    }

    // ── C5.7: the token bucket is 300, per MOUNT, and does not reset ───────
    // ── across a restart (a new `Generation`, the SAME `Arc<RateLimiter>`) ─
    //
    // Uses a FROZEN, injected clock (`TestClock`), not the real one: with a
    // real clock, a slow CI runner could let the 1/s refill put a token or
    // two back into the bucket between the 300th and 301st call, turning the
    // "still exhausted" assertion below into an occasional flake — the whole
    // reason `RateLimiter::with_clock` exists (`events_emit.rs`'s own tests
    // use the identical technique). A frozen clock makes "no time at all has
    // passed" true by construction, however long the test process itself
    // takes. (Review round 2 removed an earlier "real clock recovers after a
    // real 1.1s sleep" companion test here — same flakiness risk this whole
    // approach exists to avoid; the positive control below, advancing the
    // SAME frozen clock by exactly 1s, already proves the refill happens.)

    #[tokio::test]
    async fn c5_7_rate_limit_300_then_a_restart_does_not_reset_it() {
        let clock = Arc::new(TestClock::frozen_at(Instant::now()));
        let fx = Fixture::with_clock(clock.clone()).await;
        let g = running_generation();
        let upsert = fx.upsert(&g, "m", granted());
        let list = fx.list(&g, "m", granted());

        // A full reconciliation — 256 keys — plus one `list`, must ALL
        // succeed (v2, M3): the quota alone must never rate-limit a
        // from-scratch reconcile.
        for i in 0..256 {
            let res = upsert
                .call(json!({"key": format!("k{i}"), "spec": every_secs(3600)}))
                .await
                .unwrap();
            assert_eq!(res["outcome"], "created");
        }
        list.call(json!({})).await.unwrap();

        // 257 tokens spent so far; spend 43 more (an idempotent re-upsert,
        // so it never touches the quota) to reach exactly 300.
        for _ in 0..43 {
            let res = upsert
                .call(json!({"key": "k0", "spec": every_secs(3600)}))
                .await
                .unwrap();
            assert_eq!(res["outcome"], "unchanged");
        }

        // The 301st call is rate_limited — deterministically: the clock has
        // not moved at all.
        let err = list.call(json!({})).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::RateLimited));

        // Positive control: advancing the SAME frozen clock by exactly one
        // second refills exactly one token (the refill rate is 1/s, not
        // "some unspecified positive amount") — one more call succeeds, and
        // the one immediately after that is rate_limited again.
        clock.advance(Duration::from_secs(1));
        list.call(json!({})).await.unwrap();
        let err_again = list.call(json!({})).await.unwrap_err();
        assert_eq!(err_again.kind, Some(ErrorKind::RateLimited));

        // "Restart": a brand-new `Generation`, but the SAME `Arc<RateLimiter>`
        // (design §6.4/§6.5 — the bucket is per-MOUNT, built outside the
        // `MethodsFor` closure, unlike `_a24/events/emit`'s), on the SAME
        // still-frozen clock. Still exhausted.
        let g2 = running_generation();
        let list_after_restart = fx.list(&g2, "m", granted());
        let err2 = list_after_restart.call(json!({})).await.unwrap_err();
        assert_eq!(
            err2.kind,
            Some(ErrorKind::RateLimited),
            "the bucket must not reset across a restart"
        );
    }

    // ── C5.8: Draining semantics, and Running + a stale request_id ─────────

    #[tokio::test]
    async fn c5_8_draining_admits_bound_ids_running_rejects_a_stale_one() {
        let fx = Fixture::new().await;

        let g = running_generation();
        let live = g
            .admit_request(
                "r1".to_owned(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(g.begin_drain(Instant::now(), Duration::from_secs(10)));

        // Draining, no request_id at all: refused.
        let err = fx
            .upsert(&g, "m", granted())
            .call(json!({"key": "k1", "spec": every_secs(3600)}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Draining));

        // Draining, a genuinely live request_id: admitted (background work
        // bound to a live request is exactly what draining keeps serving).
        let ok = fx
            .upsert(&g, "m", granted())
            .call(json!({"key": "k1", "spec": every_secs(3600), "request_id": "r1"}))
            .await
            .unwrap();
        assert_eq!(ok["outcome"], "created");
        drop(live);

        // Running, a request_id that has already ended: v2 M8 — `timeout`,
        // `data.retryable == false` (never silently degraded to unbound).
        let g2 = running_generation();
        let live2 = g2
            .admit_request(
                "r2".to_owned(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(live2.finish().is_ok());
        let err2 = fx
            .upsert(&g2, "m", granted())
            .call(json!({"key": "k2", "spec": every_secs(3600), "request_id": "r2"}))
            .await
            .unwrap_err();
        assert_eq!(err2.kind, Some(ErrorKind::Timeout));
        assert_eq!(
            err2.data
                .as_ref()
                .and_then(|d| d.get("retryable"))
                .and_then(Value::as_bool),
            Some(false)
        );

        // Positive control: the SAME call with NO request_id at all succeeds
        // (Running, unbound background work is the ordinary case).
        let ok2 = fx
            .upsert(&g2, "m", granted())
            .call(json!({"key": "k3", "spec": every_secs(3600)}))
            .await
            .unwrap();
        assert_eq!(ok2["outcome"], "created");
    }

    // ── delete / list share the same admission/forbidden/rate-limit gate ──

    #[tokio::test]
    async fn delete_and_list_are_forbidden_without_the_grant_too() {
        let fx = Fixture::new().await;
        let g = running_generation();

        let err = fx
            .delete(&g, "m", ungranted())
            .call(json!({"key": "k"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Forbidden));

        let err = fx
            .list(&g, "m", ungranted())
            .call(json!({}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Forbidden));
    }

    /// Deleting a key that never existed is `absent`, not an error (design
    /// §6.2) — the positive/negative pair C5.2 doesn't cover on its own.
    #[tokio::test]
    async fn delete_of_a_never_existing_key_is_absent_not_an_error() {
        let fx = Fixture::new().await;
        let g = running_generation();
        let res = fx
            .delete(&g, "m", granted())
            .call(json!({"key": "never-existed"}))
            .await
            .unwrap();
        assert_eq!(res["outcome"], "absent");
    }

    // ── J-S7: SDK wire parity (ME4-S3 §6) ───────────────────────────────
    //
    // The SDK's `SchedulerClient` runs against `agent24_os_sdk::testing::
    // fake_kernel`; the fake kernel's peer hands the raw params straight to
    // THIS module's real `SchedulerUpsertHandler`/`SchedulerListHandler::
    // call` (the same `Fixture` the tests above use), and the handler's own
    // result is fed back for the SDK to parse.

    async fn respond_rpc_result(
        peer: &mut agent24_os_sdk::testing::FakePeer,
        req: &Value,
        result: Result<Value, RpcError>,
    ) {
        match result {
            Ok(v) => agent24_os_sdk::testing::respond(peer, req, v).await,
            Err(e) => {
                let mut data = e.data.clone().unwrap_or_default();
                if let Some(kind) = e.kind {
                    data.insert("kind".to_owned(), Value::String(kind.as_str().to_owned()));
                }
                if data.is_empty() {
                    agent24_os_sdk::testing::respond_error(
                        peer,
                        req,
                        i64::from(e.code),
                        "",
                        &e.message,
                    )
                    .await;
                } else {
                    agent24_os_sdk::testing::respond_error_with_data(
                        peer,
                        req,
                        i64::from(e.code),
                        &e.message,
                        Value::Object(data),
                    )
                    .await;
                }
            }
        }
    }

    #[tokio::test]
    async fn sdk_wire_parity_scheduler_upsert_then_list() {
        let fx = Fixture::new().await;
        let g = running_generation();
        let upsert_handler = fx.upsert(&g, "sin90", granted());
        let list_handler = fx.list(&g, "sin90", granted());

        let (conn, mut peer) =
            agent24_os_sdk::testing::fake_kernel(vec!["_a24/scheduler/".to_owned()]).await;
        let client = agent24_os_sdk::SchedulerClient::new(&conn).expect("offer covers scheduler");
        let spec = agent24_os_sdk::ScheduleSpec::Every { secs: 3600 };
        let req = agent24_os_sdk::UpsertRequest {
            key: "k1",
            spec: &spec,
            enabled: true,
            label: None,
        };

        let (upserted, ()) = tokio::join!(client.upsert(&req, None), async {
            let req = agent24_os_sdk::testing::read_request(&mut peer).await;
            let result = upsert_handler.call(req["params"].clone()).await;
            respond_rpc_result(&mut peer, &req, result).await;
        });
        let upserted = upserted.expect("SDK upsert must succeed against the real handler");
        assert_eq!(upserted.outcome, agent24_os_sdk::UpsertOutcome::Created);

        let (listed, ()) = tokio::join!(client.list(None), async {
            let req = agent24_os_sdk::testing::read_request(&mut peer).await;
            let result = list_handler.call(req["params"].clone()).await;
            respond_rpc_result(&mut peer, &req, result).await;
        });
        let listed = listed.expect("SDK list must succeed against the real handler");
        assert!(
            listed.schedules.iter().any(|s| s.key == "k1"),
            "the schedule just upserted must appear in list through the same real handler"
        );
    }
}
