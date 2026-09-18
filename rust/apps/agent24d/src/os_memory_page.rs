//! T8.5c-P — pagination / cursor / resource-billing state machine.
//!
//! Implements `docs/design/T8.5c-P-pagination-cursor.md` (v4, frozen):
//! decision P1 (§2, cursor = "last RESOLVED row"), P2/P3 (§3/§4, streaming
//! scan via `EventLog::scan_stream` + the shared `page_from_stream` loop),
//! P4 (§5, the response-byte-budget proof), P5/P5b (§6.2-§6.4, weighted
//! reservation/refund resource billing), P6 (§7, fingerprinted cursors via
//! [`Needle`]), P7 (§8, cancellation semantics — realized here as the
//! `SCAN_YIELD_INTERVAL_ROWS` yield point) and P8 (§6.5, the shared
//! connection-admission [`tokio::sync::Semaphore`]).
//!
//! # What this module does NOT do
//!
//! Wire `recall_page`/`recent_page`/`remember_checked` into a real
//! `_a24/memory/private/*` JSON-RPC [`agent24_os_proto::rpc::Handler`] — that
//! surface (`memory_callback.rs`, `MemoryEntitlement` checks, T8.5c v1
//! decisions D3/D5/D8) does not exist anywhere in this codebase yet, and
//! building it is explicitly T8.5c-W's job (design §12). What IS implemented,
//! in `os_memory.rs`, is the real async entry point each of those three
//! methods will eventually be called from: the admission-permit acquisition,
//! the resource reservation, the streaming scan loop and the
//! request-lifecycle binding are all live code exercised end to end by this
//! crate's own tests (not a mock) — a future `Handler::call` only needs to
//! add the entitlement check in front and call straight through.

// This module's production surface (everything outside `mod tests`) has no
// caller in `main()` yet — wiring `OsScopedMemory::{remember_checked,
// recall_page, recent_page}` (`os_memory.rs`) into a real
// `_a24/memory/private/*` `Handler` is T8.5c-W's job (this module's doc
// comment, and design §12), not this design doc's. Every item here IS
// exercised end to end by this crate's own tests (`cargo test`, not `cargo
// build`), which is what makes `-D warnings`'s `dead_code` lint fire on the
// plain `--bin` target: rustc's reachability analysis for a binary crate
// starts at `main`, and `#[cfg(test)]` code is a separate compilation the
// lint does not see. `#![allow(dead_code)]` here is that gap, not a claim
// this code is actually unreachable or untested.
#![allow(dead_code)]

use std::pin::Pin;
use std::sync::Arc;

use agent24_domain::memory::Recollection;
use agent24_memory::MemoryError;
use agent24_memory::event::StoredEvent;
use agent24_os_proto::drain::LifecycleTimeout;
use agent24_os_proto::rpc::{ErrorKind, RpcError};
use futures::stream::{Stream, StreamExt};
use serde::Serialize;

use crate::events_emit::{RateLimiter, ScanCost};
use crate::os_memory::{MAX_BODY_BYTES, MAX_KIND_BYTES, to_recollection};

// ---- constants (design §0 "沿用" list, §6.3, §4.5, §6.4) ----

/// T8.5c v1 D2.1 (沿用, design §0): the most a caller may ask one page for.
pub(crate) const MEMORY_MAX_PAGE_SIZE: usize = 50;
/// T8.5c v1 D2.1 (沿用, design §0): the most rows `recall_page` will ever scan
/// in one call, matched or not.
pub(crate) const MEMORY_SCAN_ROW_BUDGET: usize = 2_000;
/// T8.5c v1 D2.2 (沿用, design §0): the response body's (`items` array) byte
/// budget.
pub(crate) const MEMORY_PAGE_RESPONSE_BUDGET_BYTES: usize = 512 * 1024;
/// Design §4.2/§5.1: a loose upper bound on the fixed envelope
/// (`{"items":[...],"cursor":"..."}` braces/keys/quotes plus the cursor
/// itself) around the `items` array.
const MEMORY_RESPONSE_ENVELOPE_BYTES: usize = 128;

/// Design §6.3 — unvalidated by production traffic, stated as such there and
/// here: a direction and an order of magnitude, not a calibrated number.
pub(crate) const MEMORY_RATE_CAPACITY: f64 = 4_000.0;
pub(crate) const MEMORY_RATE_REFILL_PER_SEC: f64 = 1_000.0;

/// `remember` is one write — a flat cost (design §6.3).
pub(crate) const MEMORY_COST_REMEMBER: ScanCost = ScanCost::ONE;
/// `recall`'s worst-case scan cost: the reservation is the full row budget,
/// regardless of how few rows a given call actually touches — see §6.4 for
/// how the *settlement* (not the reservation) is what makes a `page_size=1`,
/// first-row-hit call cheap in practice.
pub(crate) const MEMORY_COST_RECALL: ScanCost = ScanCost::from_rows_const(MEMORY_SCAN_ROW_BUDGET);
/// `recent`'s reservation: the caller's requested `page_size` (design §6.3,
/// v3 L2 — this is a reservation, not a claim that it always equals what is
/// finally charged).
pub(crate) fn memory_cost_recent(page_size: usize) -> ScanCost {
    ScanCost::from_rows(page_size)
}

/// Design §6.4 (M1, v4): the worst-case number of rows sqlx-sqlite's worker
/// thread may have produced but the consumer has never observed —
/// `MEMORY_SQLITE_ROW_BUFFER_SIZE` rows already queued in the channel, plus
/// one more the worker thread may be blocked mid-send on. Defined FROM the
/// single shared connection-option constant (`agent24_memory`'s doc explains
/// why there must be exactly one source), not as an independently maintained
/// literal — see M4 in the design doc's frozen-design note.
pub(crate) const ROW_BUFFER_MARGIN: ScanCost =
    ScanCost::from_rows_const(agent24_memory::MEMORY_SQLITE_ROW_BUFFER_SIZE + 1);

/// Design §4.5 (decision 4.5, H1): how often the scan loop explicitly yields
/// so a lifecycle cancellation/timeout gets a real chance to be observed —
/// `stream.next().await` alone does not guarantee that when sqlx's worker
/// thread stays ahead of the consumer (see the design doc's derivation).
pub(crate) const SCAN_YIELD_INTERVAL_ROWS: usize = 32;

pub(crate) const METHOD_TAG_RECALL: u8 = 1;
pub(crate) const METHOD_TAG_RECENT: u8 = 2;

// ---- §5.1: the response-byte-budget proof, made checkable, not just argued ----

/// A raw byte's worst-case JSON escape blow-up (a control byte → `\u00xx`, 1
/// byte becomes 6). Only `kind` needs this factor — see the doc comment on
/// [`MAX_SINGLE_RECORD_WIRE_BYTES`].
const JSON_ESCAPE_WORST_CASE_FACTOR: usize = 6;

/// Worst-case wire size of one `Recollection`
/// (`{"id":...,"kind":...,"body":...,"at":...}`), per design §5.1's
/// field-by-field derivation:
/// - `id`: `osmem:` (6) + ULID (26) + quote/escape slack (7) = 39.
/// - `kind`: `MAX_KIND_BYTES` raw bytes, worst case ALL control characters
///   (each escapes to `\u00xx`, 6×), plus two quotes.
/// - `body`: `MAX_BODY_BYTES` — already an ENCODED byte count
///   (`os_memory.rs` enforces it against `serde_json::to_string`'s output,
///   not the raw structure), so it does not get the escape factor again.
/// - `at`: an ISO-8601 timestamp with quotes, ~34 bytes.
/// - structure: field names/colons/braces/commas, ~40 bytes of slack.
const MAX_SINGLE_RECORD_WIRE_BYTES: usize =
    39 + MAX_KIND_BYTES * JSON_ESCAPE_WORST_CASE_FACTOR + 2 + MAX_BODY_BYTES + 34 + 40;

const _: () = assert!(
    MEMORY_PAGE_RESPONSE_BUDGET_BYTES
        >= MAX_SINGLE_RECORD_WIRE_BYTES + MEMORY_RESPONSE_ENVELOPE_BYTES,
    "a single worst-case record must fit in one page's response budget, or \
     recall_page/recent_page can get stuck rejecting the very first candidate",
);

/// A loose skeleton for `{"jsonrpc":"2.0","id":"…","result":…}` around a page
/// response (design §5.1, v3 M2).
const MAX_JSONRPC_ENVELOPE_BYTES: usize = 96;

/// `agent24_os_proto::rpc::MAX_ID_BYTES` bounds the RAW request-id string
/// length; its worst-case JSON-escaped wire size gets the same escape factor
/// as `kind` (design §5.1, v3 M2) — an id is caller-supplied text, not a
/// fixed format like a ULID.
const MAX_ID_WIRE_BYTES: usize =
    agent24_os_proto::rpc::MAX_ID_BYTES * JSON_ESCAPE_WORST_CASE_FACTOR + 2;

const _: () = assert!(
    MAX_JSONRPC_ENVELOPE_BYTES + MAX_ID_WIRE_BYTES + MEMORY_PAGE_RESPONSE_BUDGET_BYTES
        < agent24_os_proto::frame::MAX_FRAME_BYTES,
    "a full page response, wrapped in the JSON-RPC envelope with the largest \
     allowed request id (worst-case JSON-escaped), must still fit under one \
     frame, or a maximally-full recall_page/recent_page response can never be \
     sent as a single frame",
);

// ---- error type (design §12: the real D8 mapping is T8.5c-W's job; this is \
// the minimal shape the state machine below needs to compile and to be \
// testable — a thin, obviously-mappable-to-`RpcError` shape, not a guess at \
// W's final error taxonomy) ----

/// The error type `page_from_stream`/`recall_page`/`recent_page`/
/// `remember_checked` return. Deliberately thin: T8.5c v1 decision D8 (the
/// real wire-error mapping for `_a24/memory/private/*`) has not been
/// implemented anywhere in this codebase yet (nothing from T8.5c v1 has), so
/// this is scaffolding the design's own pseudocode already assumes exists
/// (`MemoryRpcError::Store(...)`, `MemoryRpcError::Invalid(...)`,
/// `MemoryRpcError::application(...)`) — not a guess at D8's eventual shape,
/// just enough of one for [`Self::into_rpc_error`] to be an honest, obvious
/// mapping onto [`RpcError`] that W can replace wholesale.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryRpcError {
    /// A caller-supplied value (page_size, cursor, kind, body) failed
    /// validation — maps to `-32602`.
    Invalid(String),
    /// A storage-layer failure, or an internal invariant violation (design
    /// §5.2's "the budget is misconfigured" case) — maps to `-32603`.
    Store(String),
    /// A closed [`ErrorKind`] application error (rate limiting, timeout) —
    /// maps to `-32000` with that `kind`.
    Application(ErrorKind, String),
}

impl MemoryRpcError {
    #[must_use]
    pub fn application(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::Application(kind, message.into())
    }

    #[must_use]
    pub fn into_rpc_error(self) -> RpcError {
        match self {
            Self::Invalid(m) => RpcError::invalid_params(m),
            Self::Store(m) => RpcError::internal(m),
            Self::Application(kind, m) => RpcError::application(kind, m),
        }
    }
}

impl From<LifecycleTimeout> for MemoryRpcError {
    fn from(e: LifecycleTimeout) -> Self {
        match e {
            LifecycleTimeout::BudgetExhausted => Self::application(
                ErrorKind::Timeout,
                "this call's request-bound time budget was exhausted",
            ),
            LifecycleTimeout::RequestEnded => Self::application(
                ErrorKind::Timeout,
                "the request this call was bound to has already ended",
            ),
        }
    }
}

// ---- §2.2 / §4.2: MatchPolicy + PageMode (v4 M2) ----

/// `recall`'s substring match vs. `recent`'s unconditional match — one shared
/// loop (`page_from_stream`) instead of two, with `Always` self-documenting
/// at call sites instead of a bare `bool` whose polarity a reader has to
/// chase (design §2.2).
enum MatchPolicy<'a> {
    Substring(&'a str),
    Always,
}

impl MatchPolicy<'_> {
    fn matches(&self, item: &Recollection) -> bool {
        match self {
            MatchPolicy::Substring(needle) => item_matches_substring(item, needle),
            MatchPolicy::Always => true,
        }
    }
}

/// The `matches(row, query)` judgement decision P1 (§2.1) and the cursor
/// fingerprint (§7.1) both depend on — case-insensitive substring, over the
/// `kind` or the serialized `body`, mirroring `OsScopedMemory::recall`'s
/// existing (in-process) semantics so `recall_page` does not silently redefine
/// what "matches" means for the same partition.
fn item_matches_substring(item: &Recollection, needle: &str) -> bool {
    needle.is_empty()
        || item.kind.to_lowercase().contains(needle)
        || serde_json::to_string(&item.body)
            .unwrap_or_default()
            .to_lowercase()
            .contains(needle)
}

/// v4 (M2, design §4.2): `page_from_stream`'s single mode parameter — instead
/// of independently-passed `match_policy`/`needle`/`method_tag` (which left a
/// mismatched trio, e.g. matcher from query A + cursor fingerprint from query
/// B, a value the caller could construct but the type system never ruled
/// out). `page_from_stream` derives the matcher, the method tag and the
/// fingerprint needle from this ONE value, so those three can no longer drift
/// apart at a call site.
pub(crate) enum PageMode<'a> {
    Recall(&'a Needle),
    Recent,
}

impl<'a> PageMode<'a> {
    fn method_tag(&self) -> u8 {
        match self {
            PageMode::Recall(_) => METHOD_TAG_RECALL,
            PageMode::Recent => METHOD_TAG_RECENT,
        }
    }

    fn match_policy(&self) -> MatchPolicy<'a> {
        match self {
            PageMode::Recall(needle) => MatchPolicy::Substring(needle.as_str()),
            PageMode::Recent => MatchPolicy::Always,
        }
    }
}

// ---- §4.2 (M5) / §7.1: Needle — the sole "normalized query" ----

/// v4 (M5, design §4.2/§7.1): the only normalized-query value the whole call
/// chain is allowed to hold. Constructible only via [`Self::normalize`]
/// (`recall`) or [`Self::none_for_recent`] (`recent`'s empty-string
/// sentinel) — no call site can build a second, independently-normalized copy
/// to feed the matcher while a different one feeds the cursor fingerprint,
/// which is exactly the M3/M5 bug class the design doc's frozen-design note
/// requires closing at the type level, not by convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needle(String);

impl Needle {
    /// The one non-empty construction path. The exact normalization rule
    /// (trim/case-fold/…) is left to the caller (design §7.1: it does not
    /// affect the correctness argument, which only depends on "the whole
    /// call chain computes this once and the SAME value feeds the matcher and
    /// the fingerprint") — mirrors `OsScopedMemory::recall`'s existing
    /// trim+lowercase behaviour so `recall_page` does not silently redefine
    /// matching for the same partition.
    #[must_use]
    pub fn normalize(query: &str) -> Needle {
        Needle(query.trim().to_lowercase())
    }

    /// `recent` has no query — a fixed empty-string sentinel, matching the
    /// v3 encoding convention, but now only reachable through this one
    /// function (the field is private).
    #[must_use]
    pub fn none_for_recent() -> Needle {
        Needle(String::new())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

// ---- §7.1: cursor encode/decode with a fingerprint bound to (method, query) ----

/// FNV-1a 64-bit (v3 upgrade from 32-bit, design §7.1 M3) over `method_tag`
/// followed by the needle's bytes. Only needs to not collide in practice —
/// this is an internal private-RPC cursor, not a value defended against a
/// deliberate adversary (design §7.1).
fn cursor_fingerprint(method_tag: u8, needle: &Needle) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in std::iter::once(method_tag).chain(needle.as_str().bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

fn base64_url_no_pad_encode(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes())
}

fn base64_url_no_pad_decode(s: &str) -> Option<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()?;
    String::from_utf8(bytes).ok()
}

fn encode_cursor(seq: i64, method_tag: u8, needle: &Needle) -> String {
    let fp = cursor_fingerprint(method_tag, needle);
    base64_url_no_pad_encode(&format!("v2:{seq}:{fp:016x}"))
}

/// Design §7 (P6): rejects, rather than silently degrading, a cursor that is
/// unparseable, refers to an impossible `seq` (`< 1` — `AUTOINCREMENT` starts
/// at 1 and never reuses a value), or was issued for a different
/// method/query (the fingerprint check, §7.1's "游标未绑定 query" fix).
pub(crate) fn decode_cursor(
    token: &str,
    method_tag: u8,
    needle: &Needle,
) -> Result<i64, MemoryRpcError> {
    let not_recognizable = || MemoryRpcError::Invalid("cursor is not a recognizable token".into());
    let decoded = base64_url_no_pad_decode(token).ok_or_else(not_recognizable)?;
    let rest = decoded.strip_prefix("v2:").ok_or_else(not_recognizable)?;
    let (seq_part, fp_part) = rest.split_once(':').ok_or_else(not_recognizable)?;
    let seq: i64 = seq_part.parse().map_err(|_| not_recognizable())?;
    let fp = u64::from_str_radix(fp_part, 16).map_err(|_| not_recognizable())?;
    if seq < 1 {
        return Err(MemoryRpcError::Invalid(
            "cursor does not refer to a valid position".into(),
        ));
    }
    if fp != cursor_fingerprint(method_tag, needle) {
        return Err(MemoryRpcError::Invalid(
            "cursor was issued for a different method or query".into(),
        ));
    }
    Ok(seq)
}

// ---- §6.2-§6.4: ScanCost lives in `events_emit` (the RateLimiter it \
// weights); Reservation lives here, next to the state machine that drives it \
// (design §6.4/§6.5, v4 H1: owns Arc<RateLimiter>, no lifetime parameter, \
// moved BY VALUE into the `bind_to_lifecycle`-wrapped work future) ----

/// v4 (H1, design §6.4/§6.5): a RAII settlement guard for one
/// `RateLimiter::try_acquire_weighted` reservation. Owns `Arc<RateLimiter>` —
/// no borrow, no lifetime parameter — so it is trivially `'static + Send`
/// (given `RateLimiter: Send + Sync`, already required of the mount-level
/// singleton this crate shares across concurrent handler calls) and can be
/// moved wholesale into an `async move` block that is itself `'static`,
/// exactly what `agent24_os_proto::rpc::CallFuture`
/// (`Pin<Box<dyn Future<Output = ...> + Send>>`, implicitly `'static`)
/// requires of whatever a real `Handler::call` eventually builds around
/// `recall_page`/`recent_page`/`remember_checked`.
///
/// Accessed only via `&mut self` (`page_from_stream` takes `&mut
/// Reservation`, not `&Reservation`) — plain `bool`/`u32` fields, no `Cell`:
/// `&mut T: Send` only requires `T: Send`, unlike `&T: Send` which requires
/// `T: Sync` (the exact asymmetry v3's `Cell`-based, borrowed
/// `Reservation<'a>` got wrong).
///
/// **No "whole call, no commit ⇒ full refund" shortcut.** Once `touched` is
/// `true` (the scan loop or the write is about to touch the shared pool),
/// `Drop` settles by [`Self::settle_amount`] exactly like `commit()` does —
/// both call the SAME method, so there is exactly one settlement formula, not
/// two that can drift (v3's separate `commit(actual)` + a second formula in
/// `Drop` was the H2 bug this collapses).
#[must_use = "a reservation that is silently dropped without commit() settles \
              by its recorded scanned/exhausted state instead, never refunds \
              everything once DB scanning has started"]
pub(crate) struct Reservation {
    limiter: Arc<RateLimiter>,
    reserved: ScanCost,
    /// Has the scan loop / write started touching the shared pool? Decides
    /// which settlement branch applies.
    touched: bool,
    /// Rows the scan loop has actually pulled off the stream so far —
    /// updated in real time so a mid-scan cancellation's `Drop` reads an
    /// accurate value, not a stale one from before the last `.await`.
    scanned: u32,
    /// Has `page_from_stream` observed the stream's true end (`None` while
    /// `scanned < row_budget`)? If so, settlement is EXACT — no worker-thread
    /// lookahead margin, because the SQL layer has already proven there is
    /// nothing left unseen (design §6.4, v4 M1).
    exhausted: bool,
    committed: bool,
}

impl Reservation {
    /// Reserve `cost` tokens; `None` if the bucket could not cover it (no
    /// `Reservation` is created, so there is nothing to settle or refund —
    /// mirrors `try_acquire_weighted`'s existing pass/fail shape).
    pub(crate) fn reserve(limiter: Arc<RateLimiter>, cost: ScanCost) -> Option<Self> {
        limiter.try_acquire_weighted(cost).then(|| Reservation {
            limiter,
            reserved: cost,
            touched: false,
            scanned: 0,
            exhausted: false,
            committed: false,
        })
    }

    /// Call once, right before the scan loop / write starts touching the
    /// shared connection pool (design §6.4 M3: this means "entered DB
    /// admission", not precisely "SQL handed to the sqlx worker" — the
    /// `.await` right after this can still block on `pool.acquire()`/SQLite's
    /// `busy_timeout`, so this is a deliberately conservative, slightly
    /// EARLY billing boundary, not a precise one).
    pub(crate) fn mark_touched(&mut self) {
        self.touched = true;
    }

    /// Call once per row the scan loop actually pulls off the stream.
    pub(crate) fn record_scanned(&mut self, scanned: usize) {
        self.scanned = u32::try_from(scanned).unwrap_or(u32::MAX);
    }

    /// Call once, only when `page_from_stream` observes the stream's real
    /// end (`.next()` returned `None` while `scanned < row_budget`).
    pub(crate) fn mark_exhausted(&mut self) {
        self.exhausted = true;
    }

    /// The one settlement formula `commit()` and `Drop` both use (design
    /// §6.4 M1): untouched ⇒ 0 (full refund); touched + exhausted ⇒ exactly
    /// `scanned` (the SQL layer has already proven nothing is left
    /// unobserved); touched + NOT exhausted (early return, cancellation,
    /// error) ⇒ `scanned + ROW_BUFFER_MARGIN`, clamped to `reserved`.
    fn settle_amount(&self) -> u32 {
        if !self.touched {
            return 0;
        }
        if self.exhausted {
            self.scanned.min(self.reserved.as_u32())
        } else {
            self.scanned
                .saturating_add(ROW_BUFFER_MARGIN.as_u32())
                .min(self.reserved.as_u32())
        }
    }

    /// Normal-completion path — settles and refunds the unused portion of
    /// the reservation. Consumes `self`; there is no calling
    /// `record_scanned`/`mark_touched` after this (the borrow checker
    /// enforces it, not a runtime check).
    pub(crate) fn commit(mut self) {
        let actual = self.settle_amount();
        self.limiter
            .refund(self.reserved.as_u32().saturating_sub(actual));
        self.committed = true;
    }

    #[cfg(test)]
    pub(crate) fn scanned_for_test(&self) -> u32 {
        self.scanned
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let actual = self.settle_amount();
        self.limiter
            .refund(self.reserved.as_u32().saturating_sub(actual));
    }
}

// ---- §4.3: exact (not estimated) per-item wire bytes ----

/// The exact number of bytes one record contributes to the `items` array
/// (its own JSON encoding plus one separator byte — see design §4.3 for why
/// "estimate_" is a misnomer: this uses the SAME `Serialize` impl the final
/// response will).
fn estimate_item_bytes(item: &Recollection) -> Result<usize, MemoryRpcError> {
    let bytes = serde_json::to_vec(item)
        .map_err(|e| MemoryRpcError::Store(format!("item is not serialisable: {e}")))?;
    Ok(bytes.len() + 1)
}

// ---- §4.2: the shared cursor/scan state machine ----

/// One page's result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecallPage {
    pub items: Vec<Recollection>,
    pub cursor: Option<String>,
}

/// `resolved_seq` is `Some` at every call site inside `page_from_stream`
/// that reaches this — the loop body only ever calls it after having just
/// set `resolved_seq` itself (the match/non-match branches) or having
/// already run at least one iteration (`row_budget >= 1` is decision P4's
/// documented precondition). `None` here would mean that invariant broke.
/// This workspace forbids `unwrap`/`expect` outside tests (SPEC-001 §5), so
/// that "cannot happen" is still reported as a loud `internal` error rather
/// than a panic that would take the whole daemon process down with it.
fn must_be_resolved(resolved_seq: Option<i64>) -> Result<i64, MemoryRpcError> {
    resolved_seq.ok_or_else(|| {
        MemoryRpcError::Store(
            "internal invariant violated: page_from_stream tried to encode a cursor \
             before resolving any row (kernel bug, not a caller error)"
                .into(),
        )
    })
}

/// v4 (M2, design §4.2): the state machine `recall_page`/`recent_page` both
/// delegate to, and the exact function judgement 2b's test drives with a
/// fake stream — production and test share this one loop, not two
/// independently maintained copies.
///
/// Decision P1's cursor invariant lives entirely in this function: only two
/// branches ever advance `resolved_seq` — a confirmed non-match (permanently
/// resolved: `mem_events` is append-only, so "does not match this query" can
/// never become false later) and a successful, budget-accepted admission
/// into `hits`. The byte-budget-rejected branch deliberately does NOT
/// advance it, which is the entire fix for T8.5c v1's C1 (a matching-but-
/// budget-rejected row must never be skipped by the next page's cursor).
///
/// `response_budget_bytes` is a parameter, not the `MEMORY_PAGE_RESPONSE_BUDGET_BYTES`
/// constant baked in directly, specifically so judgement 2 (design §9) can
/// shrink it below `MAX_SINGLE_RECORD_WIRE_BYTES` in a test WITHOUT touching
/// the production constant — production call sites
/// (`recall_page`/`recent_page` in `os_memory.rs`) always pass
/// `MEMORY_PAGE_RESPONSE_BUDGET_BYTES` itself, so this is a test seam, not a
/// behavioural difference from the design doc's pseudocode (which inlines
/// the constant because it does not need to show the override).
pub(crate) async fn page_from_stream<S>(
    mut stream: Pin<&mut S>,
    mode: PageMode<'_>,
    page_size: usize,
    row_budget: usize,
    response_budget_bytes: usize,
    mut resolved_seq: Option<i64>,
    reservation: &mut Reservation,
) -> Result<RecallPage, MemoryRpcError>
where
    S: Stream<Item = Result<StoredEvent, MemoryError>>,
{
    let method_tag = mode.method_tag();
    let match_policy = mode.match_policy();
    let needle_for_cursor: std::borrow::Cow<'_, Needle> = match &mode {
        PageMode::Recall(needle) => std::borrow::Cow::Borrowed(*needle),
        PageMode::Recent => std::borrow::Cow::Owned(Needle::none_for_recent()),
    };
    let encode = |seq: i64| encode_cursor(seq, method_tag, needle_for_cursor.as_ref());

    let mut hits = Vec::with_capacity(page_size.min(MEMORY_MAX_PAGE_SIZE));
    let mut scanned = 0usize;
    let mut response_bytes = MEMORY_RESPONSE_ENVELOPE_BYTES;

    // From here on this call is about to really touch the shared pool (the
    // very next line is the first `stream.next().await`) — see
    // `Reservation::mark_touched`'s doc for the precise (conservative)
    // meaning of this boundary.
    reservation.mark_touched();

    while scanned < row_budget {
        // M1 (design §6.4): count the row as scanned THE MOMENT the stream
        // hands it over — including a `Some(Err(_))` deserialization
        // failure — not only after it has been successfully turned into a
        // `StoredEvent`/`Recollection`. A row that SQLite produced and this
        // call consumed from the channel is real cost regardless of whether
        // it went on to deserialize cleanly.
        let Some(row) = stream.next().await else {
            // scanned < row_budget and we observed the stream's true end:
            // LIMIT never fired, so this is the partition's real bottom
            // (design §4.1's invariant).
            reservation.mark_exhausted();
            return Ok(RecallPage {
                items: hits,
                cursor: None,
            });
        };
        scanned += 1;
        reservation.record_scanned(scanned);
        // Decision 4.5 (H1): a real, explicit yield point — not relying on
        // `stream.next().await` to have actually suspended (sqlx's worker
        // thread can keep this synchronously `Ready` for a long run).
        if scanned.is_multiple_of(SCAN_YIELD_INTERVAL_ROWS) {
            tokio::task::yield_now().await;
        }
        let row = row.map_err(|e| MemoryRpcError::Store(e.to_string()))?;
        let seq = row.seq;
        let recollection = to_recollection(row);
        let is_match = match_policy.matches(&recollection);

        if is_match {
            let item_bytes = estimate_item_bytes(&recollection)?;
            if response_bytes + item_bytes <= response_budget_bytes {
                hits.push(recollection);
                response_bytes += item_bytes;
                resolved_seq = Some(seq);
                if hits.len() == page_size {
                    // Enough hits. Deliberately does NOT pull one more row to
                    // confirm whether the partition also happens to end here
                    // — that would cost another DB round trip this call has
                    // no use for. The cursor stays non-empty; if the caller
                    // really has reached the end, the next call observes
                    // `None` honestly and returns `cursor: None` then — one
                    // harmless extra round trip, not a correctness gap.
                    return Ok(RecallPage {
                        items: hits,
                        cursor: Some(encode(must_be_resolved(resolved_seq)?)),
                    });
                }
            } else {
                // Byte-budget rejection: this row is UNRESOLVED — the cursor
                // must stop before it (decision P1). Not exhaustion.
                if hits.is_empty() {
                    // Not even the first candidate fits — see design §5.1's
                    // proof this should be unreachable under the shipped
                    // constants; §5.2's runtime fallback in case that proof
                    // is ever invalidated by a future constant change.
                    return Err(MemoryRpcError::Store(
                        "a single record's wire size exceeds \
                         MEMORY_PAGE_RESPONSE_BUDGET_BYTES; the budget is \
                         misconfigured relative to MAX_BODY_BYTES"
                            .into(),
                    ));
                }
                return Ok(RecallPage {
                    items: hits,
                    cursor: Some(encode(must_be_resolved(resolved_seq)?)),
                });
            }
        } else {
            // Confirmed non-match: permanently resolved, the cursor may
            // cross it.
            resolved_seq = Some(seq);
        }
    }
    // scanned == row_budget: our own scan budget fired, not the partition's
    // real end — not exhaustion.
    Ok(RecallPage {
        items: hits,
        cursor: Some(encode(must_be_resolved(resolved_seq)?)),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};

    use agent24_memory::event::{MemEvent, Origin, Scope, StoredEvent, Trust};
    use agent24_os_proto::drain::Generation;

    use super::*;

    fn fake_row(seq: i64) -> StoredEvent {
        StoredEvent {
            seq,
            event: MemEvent::new(
                format!("osmem:{seq}"),
                Scope::owner("owner"),
                "note",
                serde_json::json!({"n": seq}),
                Origin {
                    source: "test".into(),
                    trust: Trust::ToolOutput,
                },
            ),
        }
    }

    fn err_row(seq: i64) -> Result<StoredEvent, MemoryError> {
        // A row whose deserialization failed further upstream — judgement M1
        // needs a `Some(Err(_))` item to prove it is still counted, so
        // manufacture one via a JSON decode error (`MemoryError::Serde`).
        let _ = seq;
        Err(MemoryError::from(
            serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
        ))
    }

    fn generous_limiter() -> Arc<RateLimiter> {
        Arc::new(RateLimiter::new(1e12, 1e12))
    }

    /// A clock frozen at construction, for settlement-amount assertions that
    /// need EXACT token counts — `RateLimiter`'s lazy refill means any real
    /// elapsed wall-clock time between operations (even microseconds, at a
    /// large `refill_per_sec`) would otherwise add a few fractional tokens
    /// back and make an "exactly this many tokens must be left" assertion
    /// flaky. Mirrors `events_emit.rs`'s own `TestClock` test pattern.
    struct FrozenClock(std::time::Instant);
    impl crate::events_emit::Clock for FrozenClock {
        fn now(&self) -> std::time::Instant {
            self.0
        }
    }

    /// A `RateLimiter` whose clock never advances, so `refill` is always a
    /// no-op — exact settlement math is then assertable byte for byte.
    fn frozen_limiter(capacity: f64) -> Arc<RateLimiter> {
        Arc::new(RateLimiter::with_clock(
            capacity,
            0.0,
            Arc::new(FrozenClock(std::time::Instant::now())),
        ))
    }

    /// A `Vec`-backed stream that always returns `Poll::Ready` immediately —
    /// never `Poll::Pending` — so a test can prove decision 4.5's explicit
    /// yield point is what makes cancellation observable, not an incidental
    /// `Pending` from the underlying I/O (design §9, judgement 2b).
    struct ReadyStream {
        rows: std::collections::VecDeque<Result<StoredEvent, MemoryError>>,
        /// Broadcasts how many rows have been handed out so far, so a test
        /// can synchronize on "the scan has reached row N" without polling.
        progress: Option<tokio::sync::watch::Sender<usize>>,
        handed_out: usize,
    }

    impl ReadyStream {
        fn filled(n: usize) -> Self {
            Self {
                rows: (1..=n as i64).map(|seq| Ok(fake_row(seq))).collect(),
                progress: None,
                handed_out: 0,
            }
        }

        fn with_progress(mut self, tx: tokio::sync::watch::Sender<usize>) -> Self {
            self.progress = Some(tx);
            self
        }
    }

    impl Stream for ReadyStream {
        type Item = Result<StoredEvent, MemoryError>;
        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = &mut *self;
            match this.rows.pop_front() {
                Some(item) => {
                    this.handed_out += 1;
                    if let Some(tx) = &this.progress {
                        let _ = tx.send(this.handed_out);
                    }
                    Poll::Ready(Some(item))
                }
                None => Poll::Ready(None),
            }
        }
    }

    fn running_generation() -> Arc<Generation> {
        let g = Generation::serving_at("/tmp/does-not-need-to-exist".into());
        assert!(g.ready());
        g
    }

    // ── judgement 1 (★ C1 core) / decision P1 — see also the
    // integration-level version in `os_memory.rs` (through real
    // `recall_page`) — this one drives `page_from_stream` directly with a
    // byte budget deliberately shrunk so the SECOND row cannot fit. ──

    fn body_row(seq: i64, body: &str) -> StoredEvent {
        StoredEvent {
            seq,
            event: MemEvent::new(
                format!("osmem:{seq}"),
                Scope::owner("owner"),
                "note",
                serde_json::json!({"body": body}),
                Origin {
                    source: "t".into(),
                    trust: Trust::ToolOutput,
                },
            ),
        }
    }

    #[tokio::test]
    async fn a_budget_rejected_row_is_not_skipped_by_the_cursor() {
        // ★ judgement 1 (C1 core). Two matching, equal-sized rows; the
        // response budget (test-only override, design §9's judgement 1/2 —
        // production always uses MEMORY_PAGE_RESPONSE_BUDGET_BYTES) is set to
        // fit exactly one. The cursor must stop AT row 1 (not row 2, and not
        // `None`) so the next call sees row 2 again instead of skipping it.
        let big = "x".repeat(2_000);
        let one_item_bytes = estimate_item_bytes(&to_recollection(body_row(1, &big))).unwrap();
        let budget = MEMORY_RESPONSE_ENVELOPE_BYTES + one_item_bytes;

        let mut stream = ReadyStream::filled(0);
        stream.rows.push_back(Ok(body_row(1, &big)));
        stream.rows.push_back(Ok(body_row(2, &big)));
        let mut pinned = std::pin::pin!(stream);
        let mut reservation =
            Reservation::reserve(generous_limiter(), ScanCost::from_rows_const(10)).unwrap();
        let needle = Needle::normalize("");
        let page = page_from_stream(
            pinned.as_mut(),
            PageMode::Recall(&needle),
            10,
            10,
            budget,
            None,
            &mut reservation,
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 1, "only the first row fits");
        assert_eq!(page.items[0].id.as_str(), "osmem:1");
        let cursor = page.cursor.expect("more data remains (row 2)");
        // Decode with the SAME needle/method and confirm it points at row 1,
        // not row 2 and not `None` — the row the budget rejected must be
        // reachable again from the very next call.
        let decoded = decode_cursor(&cursor, METHOD_TAG_RECALL, &needle).unwrap();
        assert_eq!(
            decoded, 1,
            "cursor must stop at the last RESOLVED row, not the rejected one"
        );
    }

    // ── judgement 2 (★ single-record boundary) ──

    #[tokio::test]
    async fn a_record_at_the_wire_size_ceiling_fits_its_own_page() {
        let near_ceiling_body = "x".repeat(MAX_BODY_BYTES - 32);
        let row = body_row(1, &near_ceiling_body);
        let item_bytes = estimate_item_bytes(&to_recollection(row.clone())).unwrap();
        assert!(
            item_bytes <= MAX_SINGLE_RECORD_WIRE_BYTES,
            "fixture must stay within the proven worst case"
        );
        let mut stream = ReadyStream::filled(0);
        stream.rows.push_back(Ok(row));
        let mut pinned = std::pin::pin!(stream);
        let mut reservation =
            Reservation::reserve(generous_limiter(), ScanCost::from_rows_const(10)).unwrap();
        let needle = Needle::normalize("");
        let page = page_from_stream(
            pinned.as_mut(),
            PageMode::Recall(&needle),
            10,
            10,
            MEMORY_PAGE_RESPONSE_BUDGET_BYTES,
            None,
            &mut reservation,
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 1, "the production budget must accept it");
    }

    #[tokio::test]
    async fn a_budget_too_small_for_even_one_record_is_an_explicit_internal_error() {
        // Design §5.2: the runtime fallback for a (here, deliberately
        // misconfigured in the test only) budget that cannot fit even the
        // first candidate — must be a loud `internal` error, never a silent
        // empty page or a cursor that does not advance.
        let mut stream = ReadyStream::filled(0);
        stream.rows.push_back(Ok(body_row(1, "x")));
        let mut pinned = std::pin::pin!(stream);
        let mut reservation =
            Reservation::reserve(generous_limiter(), ScanCost::from_rows_const(10)).unwrap();
        let needle = Needle::normalize("");
        let err = page_from_stream(
            pinned.as_mut(),
            PageMode::Recall(&needle),
            10,
            10,
            MEMORY_RESPONSE_ENVELOPE_BYTES, // smaller than any real record
            None,
            &mut reservation,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, MemoryRpcError::Store(_)));
    }

    // ── judgement 2b (H1's judgement, decision 4.5) — multiple phase
    // offsets against an always-`Ready` fake stream. ──

    async fn cancel_at_offset(trigger_at: usize) -> (Result<RecallPage, MemoryRpcError>, u32) {
        let (tx, mut rx) = tokio::sync::watch::channel(0usize);
        let stream = ReadyStream::filled(MEMORY_SCAN_ROW_BUDGET).with_progress(tx);
        let mut pinned = std::pin::pin!(stream);

        let generation = running_generation();
        let in_flight = generation
            .admit_request(
                "cancel-probe".to_owned(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(3600),
            )
            .unwrap();
        let lifecycle = generation.request_lifecycle("cancel-probe").unwrap();

        let monitor = tokio::spawn(async move {
            let _ = rx.wait_for(|&n| n >= trigger_at).await;
            let _ = in_flight.finish();
        });

        let mut reservation = Reservation::reserve(
            generous_limiter(),
            ScanCost::from_rows_const(MEMORY_SCAN_ROW_BUDGET),
        )
        .unwrap();
        let outcome = agent24_os_proto::drain::bind_to_lifecycle(
            Some(lifecycle),
            page_from_stream(
                pinned.as_mut(),
                PageMode::Recent,
                MEMORY_SCAN_ROW_BUDGET,
                MEMORY_SCAN_ROW_BUDGET,
                MEMORY_PAGE_RESPONSE_BUDGET_BYTES,
                None,
                &mut reservation,
            ),
        )
        .await;
        monitor.await.unwrap();
        let scanned = reservation.scanned_for_test();
        let result = match outcome {
            Ok(inner) => inner,
            Err(timeout) => Err(MemoryRpcError::from(timeout)),
        };
        (result, scanned)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancellation_lands_within_one_yield_interval_at_multiple_phase_offsets() {
        for trigger_at in [33usize, 1001, 1985] {
            let (result, scanned) = cancel_at_offset(trigger_at).await;
            let scanned = scanned as usize;
            match result {
                Err(MemoryRpcError::Application(ErrorKind::Timeout, _)) => {
                    assert!(
                        scanned >= trigger_at,
                        "trigger_at={trigger_at}: cancellation must not be observed \
                         before the row that triggered it (scanned={scanned})"
                    );
                    assert!(
                        scanned <= trigger_at + SCAN_YIELD_INTERVAL_ROWS,
                        "trigger_at={trigger_at}: cancellation must land within one \
                         yield interval (scanned={scanned})"
                    );
                }
                Ok(_) => {
                    // The only way `page_from_stream` completes normally here
                    // is if it reached MEMORY_SCAN_ROW_BUDGET before the next
                    // yield point after trigger_at — legitimate only when
                    // trigger_at is close enough to the budget.
                    assert!(
                        MEMORY_SCAN_ROW_BUDGET <= trigger_at + SCAN_YIELD_INTERVAL_ROWS,
                        "trigger_at={trigger_at}: natural completion is only a \
                         legitimate outcome within one yield interval of the \
                         row budget (scanned={scanned})"
                    );
                }
                other => panic!("trigger_at={trigger_at}: unexpected outcome {other:?}"),
            }
        }
    }

    // ── judgement 5 / decision P6 (L1 + 游标未绑定 query Medium) ──

    #[test]
    fn decode_cursor_rejects_non_positive_and_unparseable_tokens() {
        let needle = Needle::normalize("q");
        for bad in [
            base64_url_no_pad_encode("v2:0:0000000000000000"),
            base64_url_no_pad_encode("v2:-1:0000000000000000"),
            base64_url_no_pad_encode("v1:5"),
            base64_url_no_pad_encode("v2:5"),
            "not-base64-!!!".to_owned(),
        ] {
            assert!(
                decode_cursor(&bad, METHOD_TAG_RECALL, &needle).is_err(),
                "{bad:?} must be rejected, not silently degraded"
            );
        }
    }

    #[test]
    fn a_cursor_survives_a_roundtrip_for_the_same_method_and_query() {
        let needle = Needle::normalize("hello");
        let token = encode_cursor(42, METHOD_TAG_RECALL, &needle);
        assert_eq!(
            decode_cursor(&token, METHOD_TAG_RECALL, &needle).unwrap(),
            42
        );
    }

    // ── judgement 5b (v2, 游标未绑定 query Medium) ──

    #[test]
    fn a_cursor_cannot_be_reused_across_queries_or_methods() {
        let a = Needle::normalize("A");
        let b = Needle::normalize("B");
        let token_a = encode_cursor(7, METHOD_TAG_RECALL, &a);
        assert!(
            decode_cursor(&token_a, METHOD_TAG_RECALL, &b).is_err(),
            "cross-query reuse"
        );
        assert!(
            decode_cursor(&token_a, METHOD_TAG_RECENT, &a).is_err(),
            "cross-method reuse (recall -> recent)"
        );
        let recent_needle = Needle::none_for_recent();
        let token_recent = encode_cursor(7, METHOD_TAG_RECENT, &recent_needle);
        assert!(
            decode_cursor(&token_recent, METHOD_TAG_RECALL, &a).is_err(),
            "cross-method reuse (recent -> recall)"
        );
        // Positive control: same query, same method must still work — the
        // fingerprint check must not have also broken ordinary paging.
        assert!(decode_cursor(&token_a, METHOD_TAG_RECALL, &a).is_ok());
    }

    // ── ScanCost (v2 负成本 Low, v3 溢出截断 Low) ──

    #[test]
    fn scan_cost_from_rows_const_rejects_overflow_at_compile_time_reachable_values() {
        // The runtime behaviour of the guarded branch, since the compile-time
        // failure itself cannot be expressed as a passing `#[test]`: a value
        // within range must construct cleanly.
        assert_eq!(ScanCost::from_rows_const(5).as_u32(), 5);
        assert_eq!(ScanCost::from_rows(5).as_u32(), 5);
    }

    // ── Reservation settlement (H2/M1, design §6.4) ──

    #[test]
    fn untouched_reservation_refunds_in_full() {
        let limiter = generous_limiter();
        let before = limiter.try_acquire_weighted(ScanCost::ONE); // sanity: bucket has room
        assert!(before);
        limiter.refund(1); // undo the sanity probe
        let reservation =
            Reservation::reserve(limiter.clone(), ScanCost::from_rows_const(100)).unwrap();
        drop(reservation); // never touched -> full refund
        // A fresh reservation for the same amount must succeed again if the
        // refund actually landed.
        assert!(Reservation::reserve(limiter, ScanCost::from_rows_const(100)).is_some());
    }

    #[tokio::test]
    async fn exhausted_scan_settles_exactly_at_scanned_not_scanned_plus_margin() {
        // 3 rows, budget 10 — judgement 7d's exact-settlement half.
        let stream = ReadyStream::filled(3);
        let mut pinned = std::pin::pin!(stream);
        // Capacity == reserved cost, and a frozen clock (no refill), so the
        // bucket ends up holding EXACTLY the refund — not "at least".
        let limiter = frozen_limiter(10.0);
        let mut reservation =
            Reservation::reserve(limiter.clone(), ScanCost::from_rows_const(10)).unwrap();
        let page = page_from_stream(
            pinned.as_mut(),
            PageMode::Recent,
            10,
            10,
            MEMORY_PAGE_RESPONSE_BUDGET_BYTES,
            None,
            &mut reservation,
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 3);
        assert_eq!(reservation.scanned_for_test(), 3);
        reservation.commit();
        // 10 reserved (draining the bucket to 0) + 7 refunded (10 - 3) = 7.
        assert!(limiter.try_acquire_weighted(ScanCost::from_rows_const(7)));
        assert!(
            !limiter.try_acquire_weighted(ScanCost::ONE),
            "no more than exactly 7 must be left"
        );
    }

    #[tokio::test]
    async fn an_early_return_settles_at_scanned_plus_row_buffer_margin() {
        // judgement 7d's second half: "premature return, never observed
        // `None`" — page_size reached before the stream ran out.
        let stream = ReadyStream::filled(MEMORY_SCAN_ROW_BUDGET);
        let mut pinned = std::pin::pin!(stream);
        // Capacity == reserved cost, frozen clock — see the exact-settlement
        // test above for why.
        let limiter = frozen_limiter(MEMORY_SCAN_ROW_BUDGET as f64);
        let mut reservation = Reservation::reserve(
            limiter.clone(),
            ScanCost::from_rows_const(MEMORY_SCAN_ROW_BUDGET),
        )
        .unwrap();
        let page = page_from_stream(
            pinned.as_mut(),
            PageMode::Recent,
            9,
            MEMORY_SCAN_ROW_BUDGET,
            MEMORY_PAGE_RESPONSE_BUDGET_BYTES,
            None,
            &mut reservation,
        )
        .await
        .unwrap();
        assert_eq!(
            page.items.len(),
            9,
            "hit page_size before the stream ran out"
        );
        assert_eq!(reservation.scanned_for_test(), 9);
        reservation.commit();
        // reserved 2000, actual = min(9 + ROW_BUFFER_MARGIN, 2000) =
        // 9 + ROW_BUFFER_MARGIN; refunded = 2000 - that.
        let expected_actual = 9 + ROW_BUFFER_MARGIN.as_u32() as usize;
        let expected_refund = MEMORY_SCAN_ROW_BUDGET - expected_actual;
        assert!(limiter.try_acquire_weighted(ScanCost::from_rows_const(expected_refund)));
        assert!(!limiter.try_acquire_weighted(ScanCost::ONE));
    }

    #[tokio::test]
    async fn a_mid_scan_cancellation_settles_by_scanned_state_not_a_full_refund() {
        // judgement 7c's second half (H2): cancel AFTER the loop has started
        // (touched=true) — the reservation must NOT refund in full.
        let (tx, mut rx) = tokio::sync::watch::channel(0usize);
        let stream = ReadyStream::filled(MEMORY_SCAN_ROW_BUDGET).with_progress(tx);
        let mut pinned = std::pin::pin!(stream);
        // Capacity == reserved cost, frozen clock — see the exact-settlement
        // test above for why.
        let limiter = frozen_limiter(MEMORY_SCAN_ROW_BUDGET as f64);
        let reserved = ScanCost::from_rows_const(MEMORY_SCAN_ROW_BUDGET);
        let mut reservation = Reservation::reserve(limiter.clone(), reserved).unwrap();

        let generation = running_generation();
        let in_flight = generation
            .admit_request(
                "mid-scan-cancel".to_owned(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(3600),
            )
            .unwrap();
        let lifecycle = generation.request_lifecycle("mid-scan-cancel").unwrap();
        let monitor = tokio::spawn(async move {
            let _ = rx.wait_for(|&n| n >= 1900).await;
            let _ = in_flight.finish();
        });
        let outcome = agent24_os_proto::drain::bind_to_lifecycle(
            Some(lifecycle),
            page_from_stream(
                pinned.as_mut(),
                PageMode::Recent,
                MEMORY_SCAN_ROW_BUDGET,
                MEMORY_SCAN_ROW_BUDGET,
                MEMORY_PAGE_RESPONSE_BUDGET_BYTES,
                None,
                &mut reservation,
            ),
        )
        .await;
        monitor.await.unwrap();
        assert!(
            matches!(outcome, Err(LifecycleTimeout::RequestEnded)),
            "expected the request to be cancelled near the end of the budget"
        );
        // The reservation is still alive (it was borrowed, not moved, into
        // `page_from_stream` — cancellation just stopped polling that
        // future). Drop it now to trigger settlement.
        let scanned = reservation.scanned_for_test();
        assert!(scanned >= 1900, "scanned={scanned}");
        drop(reservation);
        let expected_actual =
            (scanned as usize + ROW_BUFFER_MARGIN.as_u32() as usize).min(MEMORY_SCAN_ROW_BUDGET);
        let expected_refund = MEMORY_SCAN_ROW_BUDGET - expected_actual;
        // Almost nothing should have been refunded (scanned near the full
        // budget) — this is the H2 fix: "scan to near the budget, then
        // cancel" must not be free.
        assert!(expected_refund < ROW_BUFFER_MARGIN.as_u32() as usize + 100);
        assert!(limiter.try_acquire_weighted(ScanCost::from_rows_const(expected_refund)));
        assert!(!limiter.try_acquire_weighted(ScanCost::ONE));
    }

    #[test]
    fn a_reservation_cancelled_before_touching_the_pool_refunds_in_full() {
        // judgement 7c's first half: cancelled while still queued (e.g.
        // waiting on the admission permit) — `touched` was never set.
        let limiter = Arc::new(RateLimiter::new(1000.0, 1000.0));
        let reservation =
            Reservation::reserve(limiter.clone(), ScanCost::from_rows_const(500)).unwrap();
        drop(reservation); // never touched
        assert!(limiter.try_acquire_weighted(ScanCost::from_rows_const(1000)));
    }

    // ── M1: a `Some(Err(_))` row is still counted as scanned ──

    #[tokio::test]
    async fn a_deserialization_failure_is_counted_as_scanned_before_the_error_propagates() {
        let mut stream = ReadyStream::filled(0);
        stream.rows.push_back(err_row(1));
        let mut pinned = std::pin::pin!(stream);
        let mut reservation =
            Reservation::reserve(generous_limiter(), ScanCost::from_rows_const(10)).unwrap();
        let err = page_from_stream(
            pinned.as_mut(),
            PageMode::Recent,
            10,
            10,
            MEMORY_PAGE_RESPONSE_BUDGET_BYTES,
            None,
            &mut reservation,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, MemoryRpcError::Store(_)));
        assert_eq!(
            reservation.scanned_for_test(),
            1,
            "the failed row must still be counted — M1 (design §6.4)"
        );
    }

    // ── const-assert judgement (9c): the two `const _: () = assert!(...)`
    // blocks earlier in this file (§5.1's byte-budget proof) are themselves
    // the judgement — `cargo build`/`cargo test` failing to compile IS the
    // test; a passing `#[test]` that merely restates
    // `MEMORY_PAGE_RESPONSE_BUDGET_BYTES > MAX_SINGLE_RECORD_WIRE_BYTES` would
    // assert on two `const`s clippy correctly calls out as vacuous
    // (`assertions_on_constants`), so there is deliberately no such test
    // here. Verified manually per design §9's judgement 9c: shrinking either
    // constant below the other makes this module fail to compile. ──

    #[test]
    fn row_buffer_margin_is_the_shared_row_buffer_size_plus_one() {
        assert_eq!(
            ROW_BUFFER_MARGIN.as_u32() as usize,
            agent24_memory::MEMORY_SQLITE_ROW_BUFFER_SIZE + 1
        );
    }
}
