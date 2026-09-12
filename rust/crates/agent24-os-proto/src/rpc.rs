//! ME-3c — the callback channel after the handshake (SPEC-ME3-OUT-OF-PROCESS §3,
//! §8 ME-3c).
//!
//! The handshake (ME-3b-2b, [`crate::initialize`]) decides whether a connection
//! may exist at all. This module decides everything after that: how one frame is
//! classified ([`dispatch`]), and how a connection runs many calls at once
//! ([`serve`]).
//!
//! # The two rule sets, and which one applies
//!
//! During the handshake **every failure disconnects**. After it, **only a
//! framing overrun disconnects**: a malformed line, bad params, a repeated
//! `initialize`, an unknown method — each fails that one line and the connection
//! keeps going. The asymmetry is SPEC's (§3, and the ME-3c row of §8), and it is
//! the reason this module exists separately from `initialize`: the same bytes
//! get a different fate depending on which side of the handshake they arrive.
//!
//! # The offer set is empty here
//!
//! No business method is registered by this slice: [`Methods::none`] is what a
//! kernel at this stage serves, so every call is `-32601`. **Methods are not
//! registered early to make a `forbidden` test possible** — SPEC §8 forbids that
//! explicitly (it would collide with "an unimplemented `scoped/*` must be
//! method-not-found"). "A handler exists but the caller lacks the grant →
//! forbidden" belongs to ME-3d/3e. Tests here register test-only methods on
//! their own [`Methods`]; production code never does.
//!
//! # Not in this slice
//!
//! Wiring [`serve`] into the daemon (the supervisor owns connections; ME3-SUP),
//! and the DRAINING admission check for callbacks (`drain::Generation::
//! admit_callback`, ME-3b-5): with no business method there is nothing for it to
//! admit, and the first handler (ME-3d/3e) is where it gets its call site.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::frame::{FrameError, MAX_FRAME_BYTES};
use crate::initialize::INITIALIZE_METHOD;

/// How many calls one connection may have in flight at once. Beyond it a call is
/// answered `busy` at once rather than queued — a queue is memory the peer
/// controls, which is the thing being bounded.
///
/// **SPEC gives no number** (§3: "并发上限见 §5"; §5 has none), so this is a
/// choice, recorded as one (⚖️) in SPEC's ME-3c table. What it bounds is memory:
/// each in-flight call holds its parsed params (from a frame of at most
/// [`MAX_FRAME_BYTES`]) and, when done, a queued response within the frame
/// limit. **Parsed params are not frame-sized**: a 1 MiB `{"a":[0,0,…]}` parses
/// to 524 278 `Value`s of 32 bytes, over 15 MiB (review F4 @ #176), so once a
/// method accepts array params this ceiling is near 1 GiB per connection. No
/// method does yet (the offer set is empty); before the first one does (ME-3d),
/// bound in-flight params by bytes or cap one call's params (FU-53). 64 is the proxy's per-module
/// ceiling too; that is symmetry, **not** a guarantee that callbacks cannot
/// outnumber requests — nothing ties the two, and background work (§5) has no
/// request at all.
pub const MAX_IN_FLIGHT_PER_CONNECTION: usize = 64;

/// How long the kernel works on one callback before answering `timeout`. Not
/// retried: a callback may have side effects (SPEC §3). SPEC gives no number;
/// 30s is the proxy's total deadline, chosen for symmetry (⚖️). It does **not**
/// make a callback end with the request it was made for — a callback started at
/// second 29 of a request can outlive it by nearly 30s. Bounding a callback by
/// its request's remaining time needs the `request_id` link (ME-3b-5's
/// `admit_callback`, wired in by the first business method).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one response may take to be written before the connection is given
/// up on. A module that stops reading must not be able to stall the kernel's
/// side of the connection.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest request id accepted (SPEC §3: ids are strings; it does not bound
/// them). Unbounded ids make every per-id structure — the in-flight table, the
/// `duplicate_id` echo — sized by the peer.
pub const MAX_ID_BYTES: usize = 256;

/// Cancellation, LSP-style (SPEC §3): a **notification**, `params: {id}`.
pub const CANCEL_METHOD: &str = "$/cancelRequest";

/// JSON-RPC codes this channel uses. Protocol-level failures keep JSON-RPC's own
/// codes; every application-level failure is `-32000` with a closed `kind`.
pub mod code {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    pub const APPLICATION: i32 = -32000;
}

/// `error.data.kind` — a **closed** set (SPEC §3). A kind outside this list is a
/// kind a module cannot have been written against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Forbidden,
    Busy,
    Cancelled,
    Timeout,
    QuotaExceeded,
    InvalidLease,
    UnknownCapability,
    VersionMismatch,
    AuthFailed,
    ManifestMismatch,
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 10] = [
        Self::Forbidden,
        Self::Busy,
        Self::Cancelled,
        Self::Timeout,
        Self::QuotaExceeded,
        Self::InvalidLease,
        Self::UnknownCapability,
        Self::VersionMismatch,
        Self::AuthFailed,
        Self::ManifestMismatch,
    ];

    /// The wire string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::Busy => "busy",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::QuotaExceeded => "quota_exceeded",
            Self::InvalidLease => "invalid_lease",
            Self::UnknownCapability => "unknown_capability",
            Self::VersionMismatch => "version_mismatch",
            Self::AuthFailed => "auth_failed",
            Self::ManifestMismatch => "manifest_mismatch",
        }
    }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    /// Application errors carry a closed [`ErrorKind`] in `data.kind`.
    pub kind: Option<ErrorKind>,
    /// Anything else in `data` (merged beside `kind`).
    pub data: Option<Map<String, Value>>,
}

impl RpcError {
    fn protocol(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            kind: None,
            data: None,
        }
    }

    #[must_use]
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::protocol(code::PARSE_ERROR, message)
    }

    #[must_use]
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::protocol(code::INVALID_REQUEST, message)
    }

    #[must_use]
    pub fn method_not_found(method: &str) -> Self {
        Self::protocol(
            code::METHOD_NOT_FOUND,
            format!(
                "method not found: this daemon does not provide `{}`",
                clip(method)
            ),
        )
    }

    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::protocol(code::INVALID_PARAMS, message)
    }

    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::protocol(code::INTERNAL_ERROR, message)
    }

    /// `-32000` with a closed kind.
    #[must_use]
    pub fn application(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            code: code::APPLICATION,
            message: message.into(),
            kind: Some(kind),
            data: None,
        }
    }

    #[must_use]
    pub fn with_data(mut self, key: &str, value: Value) -> Self {
        self.data
            .get_or_insert_with(Map::new)
            .insert(key.to_owned(), value);
        self
    }

    fn to_json(&self) -> Value {
        let mut error = Map::new();
        error.insert("code".into(), json!(self.code));
        error.insert("message".into(), json!(self.message));
        let mut data = self.data.clone().unwrap_or_default();
        if let Some(kind) = self.kind {
            data.insert("kind".into(), json!(kind.as_str()));
        }
        if !data.is_empty() {
            error.insert("data".into(), Value::Object(data));
        }
        Value::Object(error)
    }
}

/// One response line. `id: None` is JSON `null` — used when the request's id
/// could not be determined, or (for a reused in-flight id) must not be echoed:
/// see [`dispatch`].
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub id: Option<String>,
    pub outcome: Result<Value, RpcError>,
}

impl Response {
    fn error(id: Option<String>, e: RpcError) -> Self {
        Self {
            id,
            outcome: Err(e),
        }
    }

    /// The NDJSON line, newline included.
    #[must_use]
    pub fn to_line(&self) -> Vec<u8> {
        let mut obj = Map::new();
        obj.insert("jsonrpc".into(), json!("2.0"));
        obj.insert(
            "id".into(),
            self.id.clone().map_or(Value::Null, Value::String),
        );
        match &self.outcome {
            Ok(v) => obj.insert("result".into(), v.clone()),
            Err(e) => obj.insert("error".into(), e.to_json()),
        };
        let mut line = serde_json::to_vec(&Value::Object(obj)).unwrap_or_default();
        line.push(b'\n');
        line
    }
}

/// The future a handler returns.
pub type CallFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;

/// One business method. None is registered by this slice (see the module docs).
pub trait Handler: Send + Sync {
    /// Validate `params` **before** anything runs. `Err` → `-32602` and the
    /// handler is never called (SPEC §8: "params 解析失败固定返回 -32602 且不
    /// dispatch handler").
    ///
    /// # Errors
    ///
    /// A human-readable reason, put into the `-32602` message.
    fn check_params(&self, params: &Value) -> Result<(), String>;

    /// Run the call. Dropped — not awaited to completion — on cancellation,
    /// timeout, or the connection ending.
    ///
    /// **All of the call's work must live in the returned future.** `serve`
    /// guarantees that the future is dropped; it cannot reach a task the handler
    /// spawned and detached, nor a `spawn_blocking` already running. A handler
    /// that does either has work that outlives cancellation and the connection,
    /// and the guarantee above does not extend to it.
    ///
    /// **And the future must yield.** Cancellation, the call timeout and the
    /// shutdown when the connection ends all take effect at an `.await`; a
    /// `poll` that loops or blocks synchronously cannot be interrupted by
    /// anything in this process — `serve` then does not return until it does.
    /// Guarding against a handler that never yields needs a process boundary: a
    /// thread can isolate it, but a Rust thread cannot be safely killed.
    fn call(&self, params: Value) -> CallFuture;
}

/// The methods a connection serves.
#[derive(Clone, Default)]
pub struct Methods {
    map: HashMap<String, Arc<dyn Handler>>,
}

impl Methods {
    /// What this slice serves: nothing. Every call is `-32601`.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Register a method. Later slices (ME-3d/3e) add their handlers here.
    #[must_use]
    pub fn with(mut self, name: &str, handler: Arc<dyn Handler>) -> Self {
        self.map.insert(name.to_owned(), handler);
        self
    }

    fn get(&self, name: &str) -> Option<Arc<dyn Handler>> {
        self.map.get(name).cloned()
    }
}

/// What to do with one frame.
pub enum Dispatch {
    /// Answer now; nothing runs.
    Respond(Response),
    /// Run `handler` for request `id`.
    Call {
        id: String,
        params: Value,
        handler: Arc<dyn Handler>,
    },
    /// Cancel the in-flight request `id` (if there is one).
    Cancel { id: String },
    /// A notification this channel does not act on. Notifications are never
    /// answered (JSON-RPC 2.0), so an unknown or malformed one is dropped.
    Ignore,
}

/// Classify one post-handshake frame. Pure: no I/O, no clock.
///
/// `in_flight` answers "is this id running on this connection right now" — a
/// lookup, not a set, so classifying a frame does not cost a copy of every
/// in-flight id (with 64 long ids in flight, a copy per frame was measured at
/// ~22ms per 33-byte notification).
///
/// The order is part of the contract:
///
/// 1. **Not JSON** → `-32700`, `id: null`. Syntax first: a frame that is both
///    truncated and has a repeated key is malformed JSON, not "a duplicate".
/// 2. **Not an object** → `-32600`, `id: null` (a batch, or a scalar).
/// 3. **A repeated key in the envelope** (including `params` itself appearing
///    twice) → `-32600`, `id: null` — which of two ids is meant is itself the
///    problem. Without any `id` member at all it is a notification, and is not
///    answered.
/// 4. **The id**: a string of at most [`MAX_ID_BYTES`], else `-32600`, `id: null`.
/// 5. **An id already in flight** → `-32600`, `id: null`, with the id in
///    `error.data.duplicate_id` — checked **before** any other validation.
///    Echoing the id would pair the error with the call that is still running
///    (SPEC §3 says the second request fails; it does not say how, and every
///    later check that echoes the id would make that pairing again). `null` is
///    JSON-RPC's own answer for "this request's id cannot be used"; the original
///    request still gets its own response.
/// 6. Everything else — envelope members, `jsonrpc`, `method`, `params`,
///    `initialize` again, unknown methods, a repeated key inside `params`,
///    `check_params` — fails that one request with its id echoed.
#[must_use]
pub fn dispatch(frame: &[u8], methods: &Methods, in_flight: &dyn Fn(&str) -> bool) -> Dispatch {
    let respond_null = |e: RpcError| Dispatch::Respond(Response::error(None, e));

    let value: Value = match serde_json::from_slice(frame) {
        Ok(v) => v,
        Err(e) => return respond_null(RpcError::parse_error(e.to_string())),
    };
    let Value::Object(obj) = value else {
        let what = if value.is_array() {
            "batch requests are not supported"
        } else {
            "a request must be a JSON object"
        };
        return respond_null(RpcError::invalid_request(what));
    };

    // Repeated keys, over the RAW bytes: parsing into a map keeps the last value
    // and forgets there were two — "last one wins" lets a sender show one value
    // to a logger and another to a checker (SPEC §8 ME-3c). The frame is known
    // to be valid JSON here, so the scan fails only on a duplicate.
    let duplicate = find_duplicate_key(frame);
    let has_id = obj.contains_key("id");
    if let Some(path) = duplicate.as_ref().filter(|p| p.len() == 1) {
        if !has_id {
            return Dispatch::Ignore; // a notification: never answered
        }
        return respond_null(RpcError::invalid_request(format!(
            "duplicate key `{}` in the request",
            clip(&path[0])
        )));
    }
    let duplicate_in_params = duplicate
        .as_ref()
        .filter(|p| p.len() > 1 && p[0] == "params")
        .map(|p| p.join("."));

    let id = match obj.get("id") {
        None => None,
        Some(Value::String(s)) if s.len() <= MAX_ID_BYTES => Some(s.clone()),
        Some(Value::String(_)) => {
            return respond_null(RpcError::invalid_request(format!(
                "the id is longer than {MAX_ID_BYTES} bytes"
            )));
        }
        Some(_) => return respond_null(RpcError::invalid_request("the id must be a string")),
    };
    if let Some(id) = &id
        && in_flight(id)
    {
        return respond_null(
            RpcError::invalid_request("this id is already in flight on this connection")
                .with_data("duplicate_id", Value::String(id.clone())),
        );
    }
    let fail = |e: RpcError| match &id {
        Some(id) => Dispatch::Respond(Response::error(Some(id.clone()), e)),
        None => Dispatch::Ignore,
    };

    if let Some(extra) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "jsonrpc" | "id" | "method" | "params"))
    {
        return fail(RpcError::invalid_request(format!(
            "unknown member `{}`",
            clip(extra)
        )));
    }
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return fail(RpcError::invalid_request("`jsonrpc` must be \"2.0\""));
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return fail(RpcError::invalid_request("`method` must be a string"));
    };
    let params = match obj.get("params") {
        None => Value::Object(Map::new()),
        Some(p @ Value::Object(_)) => p.clone(),
        Some(_) => return fail(RpcError::invalid_params("params must be an object")),
    };

    let Some(id) = id.clone() else {
        // A notification.
        if method == CANCEL_METHOD && duplicate_in_params.is_none() {
            return match params.get("id").and_then(Value::as_str) {
                Some(target) if params_only(&params, &["id", "_meta"]) => Dispatch::Cancel {
                    id: target.to_owned(),
                },
                _ => Dispatch::Ignore,
            };
        }
        return Dispatch::Ignore;
    };

    if method == INITIALIZE_METHOD {
        return fail(RpcError::invalid_request(
            "`initialize` may be sent only once, as the first message",
        ));
    }
    if method == CANCEL_METHOD {
        return fail(RpcError::invalid_request(
            "`$/cancelRequest` is a notification; send it without an id",
        ));
    }
    let Some(handler) = methods.get(method) else {
        return fail(RpcError::method_not_found(method));
    };
    if let Some(at) = duplicate_in_params {
        return fail(RpcError::invalid_params(format!(
            "duplicate key `{}` in params",
            clip(&at)
        )));
    }
    // A validator that panics is a kernel bug; it must fail that one request,
    // not unwind through the connection and take every other call with it.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handler.check_params(&params)
    })) {
        Ok(Ok(())) => {}
        Ok(Err(why)) => return fail(RpcError::invalid_params(why)),
        Err(_) => return fail(RpcError::internal("the method's params check failed")),
    }
    Dispatch::Call {
        id,
        params,
        handler,
    }
}

/// Longest error message [`serve`] writes, whoever wrote the message — the
/// `…` that marks a cut included.
pub(crate) const MAX_MESSAGE_BYTES: usize = 1024;

/// Longest string echoed back from a request into an error message. A message
/// that repeats an attacker-chosen string in full could itself exceed the frame
/// limit (a 1 MiB method name, echoed, is a response over 1 MiB).
const ECHO_LIMIT: usize = 128;

fn clip(s: &str) -> String {
    clip_to(s, ECHO_LIMIT)
}

pub(crate) fn clip_to(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_owned();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn params_only(params: &Value, allowed: &[&str]) -> bool {
    params
        .as_object()
        .is_some_and(|m| m.keys().all(|k| allowed.contains(&k.as_str())))
}

/// A repeated key, as a path of object keys from the top (array positions are
/// not recorded). **Every** repeat is found, and one in the envelope (a
/// one-segment path) is returned in preference to one inside `params` — so
/// `{"params":{"x":1,"x":2},"id":"a","id":"b"}` is an envelope problem, not a
/// params problem answered with whichever id parsed last. Call only on bytes
/// already known to be JSON.
pub(crate) fn find_duplicate_key(bytes: &[u8]) -> Option<Vec<String>> {
    scan_duplicates(bytes).0
}

fn scan_duplicates(bytes: &[u8]) -> (Option<Vec<String>>, usize) {
    let mut found = Found::default();
    let mut path = Vec::new();
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let _ = NoDup {
        path: &mut path,
        found: &mut found,
    }
    .deserialize(&mut de);
    let copies = found.copies;
    (found.envelope.or(found.first), copies)
}

/// The two repeats [`find_duplicate_key`] can answer with. Only these are kept:
/// cloning the path of EVERY repeat lets one frame of long nested keys and many
/// repeated leaves allocate far more than the frame itself (review @ 41d2094).
#[derive(Default)]
struct Found {
    envelope: Option<Vec<String>>,
    first: Option<Vec<String>>,
    /// How many paths were copied — at most two, whatever the frame. Counted so
    /// a test can pin the bound directly: timing it cannot tell 2 copies from
    /// 20 000 on a fast machine.
    copies: usize,
}

struct NoDup<'a> {
    path: &'a mut Vec<String>,
    found: &'a mut Found,
}

impl<'de> DeserializeSeed<'de> for NoDup<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for NoDup<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let (path, found) = (self.path, self.found);
        while seq
            .next_element_seed(NoDup {
                path: &mut *path,
                found: &mut *found,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let (path, found) = (self.path, self.found);
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                // Record and keep scanning: a later, more serious repeat (in the
                // envelope) must not be hidden by an earlier one (in params).
                if path.is_empty() && found.envelope.is_none() {
                    found.envelope = Some(vec![key.clone()]);
                    found.copies += 1;
                } else if found.first.is_none() {
                    let mut at = path.clone();
                    at.push(key.clone());
                    found.first = Some(at);
                    found.copies += 1;
                }
            }
            path.push(key);
            map.next_value_seed(NoDup {
                path: &mut *path,
                found: &mut *found,
            })?;
            path.pop();
        }
        Ok(())
    }
}

// ── the connection ──────────────────────────────────────────────────────

/// Per-connection limits. Production uses [`Limits::default`]; tests shrink them
/// so the `busy`, `timeout` and stuck-writer branches are reachable in
/// milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Calls accepted whose handler has not yet been reaped. An id leaves the
    /// table when its handler is reaped, before its response is queued, so the
    /// count never exceeds what the peer sees as unanswered: a peer with at
    /// most `max_in_flight - 1` unanswered calls always has the next one
    /// accepted. The converse does not hold — responses queued but not yet
    /// delivered are not counted (their memory is bounded by the queue's
    /// high-water mark instead). For a peer that sends more than the limit
    /// without waiting, how many are `busy` depends on how fast handlers finish
    /// and on scheduling.
    pub max_in_flight: usize,
    pub call_timeout: Duration,
    /// How long one response may take to be written before the connection is
    /// given up on.
    pub write_timeout: Duration,
    /// Bytes waiting to be written above which no new frame is read.
    pub queue_high_water: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_in_flight: MAX_IN_FLIGHT_PER_CONNECTION,
            call_timeout: CALL_TIMEOUT,
            write_timeout: WRITE_TIMEOUT,
            queue_high_water: QUEUE_HIGH_WATER,
        }
    }
}

/// Why [`serve`] returned. By then every in-flight handler future **has been
/// dropped** (not merely asked to stop), and none of them was answered — the
/// connection they would be answered on is gone.
#[derive(Debug)]
pub enum Ended {
    /// The module closed the connection.
    PeerClosed,
    /// A line exceeded [`MAX_FRAME_BYTES`]: the one post-handshake failure that
    /// disconnects (the stream is mid-line and cannot be resynchronised).
    TooLong,
    /// Reading failed.
    ReadFailed(std::io::Error),
    /// Writing a response failed, or took longer than `write_timeout` — which
    /// is also how a module that stopped reading shows up.
    WriteFailed(std::io::Error),
    /// The `stop` given to [`serve_until`] fired — the generation this
    /// connection belongs to was revoked, say. Handlers are aborted and waited
    /// for exactly as on any other end.
    Stopped,
}

/// Read one frame from an async reader — [`crate::frame::read_frame`]'s rules,
/// byte for byte: at most `MAX_FRAME_BYTES + 1` bytes are read before refusing,
/// the rest of an over-long line is NOT drained, and a final line without a
/// newline is `Eof`, not a frame. (The two are kept separate rather than one
/// generic over sync/async: the sync one is 3b-1's, with its own reviewed
/// history; the equivalence is pinned by running both on the same inputs.)
///
/// **Not cancel-safe**: dropping it mid-frame loses the bytes consumed so far.
/// [`serve`] therefore reads on its own task and never races this future.
///
/// # Errors
///
/// [`FrameError::TooLong`], [`FrameError::Eof`], [`FrameError::Io`].
pub async fn read_frame_async<R: AsyncBufRead + Unpin>(src: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::new();
    loop {
        let Some(room) = (MAX_FRAME_BYTES + 1)
            .checked_sub(out.len())
            .filter(|r| *r > 0)
        else {
            return Err(FrameError::TooLong {
                limit: MAX_FRAME_BYTES,
            });
        };
        let available = match src.fill_buf().await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(FrameError::Io(e)),
        };
        if available.is_empty() {
            return Err(FrameError::Eof);
        }
        let window = &available[..available.len().min(room)];
        match window.iter().position(|&b| b == b'\n') {
            Some(i) => {
                out.extend_from_slice(&window[..i]);
                src.consume(i + 1);
                return Ok(out);
            }
            None => {
                let taken = window.len();
                assert!(taken > 0, "no progress: the loop would spin");
                out.extend_from_slice(window);
                src.consume(taken);
            }
        }
    }
}

/// Aborts a task when dropped.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The line to write for `response`. A response that would exceed the frame
/// limit is replaced by an error for the same id — the peer applies the same
/// framing rule and would disconnect on it (SPEC §5: limits in both
/// directions).
///
/// Before that, an error message longer than [`MAX_MESSAGE_BYTES`] is cut. The
/// echoes this module writes are clipped where they are made, but a message can
/// also come from a handler — a params check that quotes the bad value, a
/// call's own error — and without this cut a long quote would turn the peer's
/// `-32602` into the frame-limit `-32603` (review of f2b4e2e, M1).
fn response_line(mut response: Response) -> Vec<u8> {
    if let Err(e) = &mut response.outcome
        && e.message.len() > MAX_MESSAGE_BYTES
    {
        e.message = clip_to(&e.message, MAX_MESSAGE_BYTES - '…'.len_utf8());
    }
    let line = response.to_line();
    if line.len() <= MAX_FRAME_BYTES + 1 {
        return line;
    }
    Response::error(
        response.id,
        RpcError::internal(format!(
            "the response would exceed the {MAX_FRAME_BYTES}-byte frame limit"
        )),
    )
    .to_line()
}

/// Run a connection after its handshake: read frames, run calls concurrently,
/// write each response when its call finishes (so responses may be out of
/// order), until the peer closes, a line is too long, the peer stops reading,
/// or I/O fails.
///
/// Structure, and why each part is where it is:
///
/// - **Frames come from a dedicated reader task.** `read_frame_async` is not
///   cancel-safe; racing it in the `select!` below would drop a half-read frame
///   and desynchronise the stream.
/// - **Responses go to a dedicated writer task** through an unbounded channel
///   whose bytes are counted, and each write has a deadline. A module that
///   stops reading must not freeze this loop — with the writer inline, a full
///   socket buffer blocks the loop, which then stops reading frames, so a
///   cancel or a close is never seen. Over the high-water mark the loop stops
///   taking reader events (below); a write past its deadline ends the
///   connection.
/// - **Handlers run directly in a `JoinSet` owned here** — one task each, no
///   nesting. Cancelling aborts the task and the `cancelled` response is sent
///   when the set reports the task finished, i.e. after the handler future has
///   been dropped. On return, `shutdown().await` waits for every task to finish,
///   so no handler future outlives the connection.
/// - **`biased`, frames first**: when an end-of-stream and a finished call are
///   ready together, the end wins, and nothing more is written. **Not over the
///   high-water mark**: there no reader event is taken at all — a frame, an
///   end-of-stream, a read error and an over-long line all wait for the queue
///   to drain, and if the writer's deadline expires first the connection ends
///   as `WriteFailed`, not as what the reader saw.
pub async fn serve<R, W>(reader: R, writer: W, methods: Methods, limits: Limits) -> Ended
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    serve_until(reader, writer, methods, limits, std::future::pending()).await
}

/// [`serve`], and also end — with [`Ended::Stopped`] — when `stop` resolves.
/// `stop` wins over everything else that is ready at the same moment, frames
/// included: a loop biased to frames would otherwise never see it while the
/// peer keeps sending. On return, as with [`serve`], every handler future has
/// been dropped and the writer has stopped.
pub async fn serve_until<R, W, S>(
    reader: R,
    writer: W,
    methods: Methods,
    limits: Limits,
    stop: S,
) -> Ended
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    S: std::future::Future<Output = ()>,
{
    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::channel::<Frame>(1);
    let _reader = AbortOnDrop(tokio::spawn(async move {
        let mut reader = reader;
        loop {
            let frame = read_frame_async(&mut reader).await;
            let last = frame.is_err();
            if frames_tx.send(frame).await.is_err() || last {
                return;
            }
        }
    }));
    run_until(&mut frames_rx, writer, methods, limits, stop).await
}

/// One reader event: a frame, or how the stream ended.
type Frame = Result<Vec<u8>, FrameError>;

/// The loop of [`serve`], fed by a channel of reader events rather than by the
/// reader itself. Tests hand it a channel with frames already waiting, which
/// makes paths that in production depend on the reader task running on
/// another thread (a frame ready at the reaping step; frames ready every time
/// the loop looks) happen on every run instead of on some — and, the channel
/// being borrowed, can count afterwards how many frames were never read.
#[cfg(test)]
async fn run<W>(
    frames_rx: &mut tokio::sync::mpsc::Receiver<Frame>,
    writer: W,
    methods: Methods,
    limits: Limits,
) -> Ended
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    run_until(frames_rx, writer, methods, limits, std::future::pending()).await
}

/// [`run`] with a stop signal (see [`serve_until`]).
async fn run_until<W, S>(
    frames_rx: &mut tokio::sync::mpsc::Receiver<Frame>,
    writer: W,
    methods: Methods,
    limits: Limits,
    stop: S,
) -> Ended
where
    W: AsyncWrite + Unpin + Send + 'static,
    S: std::future::Future<Output = ()>,
{
    let mut stop = std::pin::pin!(stop);
    // The response queue is bounded by BYTES, and the bound is enforced by not
    // READING, never by waiting: while more than `QUEUE_HIGH_WATER` bytes are
    // waiting to be written, no new frame is read — but finished calls are still
    // reaped and the writer still watched, so this loop never blocks on the
    // peer. (A wait for queue space inside the loop froze cancellation and EOF
    // for up to the write deadline; review @ 41d2094.) A peer that stops reading
    // shows up as the writer's own deadline expiring.
    let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let drained = Arc::new(tokio::sync::Notify::new());
    let (lines_tx, mut lines_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let write_timeout = limits.write_timeout;
    let mut writer_task = AbortOnDrop(tokio::spawn({
        let (queued, drained) = (queued.clone(), drained.clone());
        async move {
            let mut writer = writer;
            while let Some(line) = lines_rx.recv().await {
                let write = async {
                    writer.write_all(&line).await?;
                    writer.flush().await
                };
                match tokio::time::timeout(write_timeout, write).await {
                    Ok(Ok(())) => {
                        queued.fetch_sub(line.len(), std::sync::atomic::Ordering::SeqCst);
                        drained.notify_one();
                    }
                    Ok(Err(e)) => return e,
                    Err(_) => {
                        return std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!(
                                "a response took longer than {}ms to write",
                                write_timeout.as_millis()
                            ),
                        );
                    }
                }
            }
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the response queue closed")
        }
    }));
    let enqueue = |response: Response| -> Result<(), Ended> {
        let line = response_line(response);
        queued.fetch_add(line.len(), std::sync::atomic::Ordering::SeqCst);
        lines_tx.send(line).map_err(|_| {
            Ended::WriteFailed(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the writer stopped",
            ))
        })
    };

    let mut writer_done = false;
    let mut conn = Conn {
        methods: &methods,
        limits,
        handlers: tokio::task::JoinSet::new(),
        by_task: HashMap::new(),
        in_flight: HashMap::new(),
    };
    // Frames are taken first (`biased`) so that an end-of-stream beats a finished
    // call. Unbounded, that lets a peer that never stops sending starve every
    // finished call of its response (and its slot) — so after this many frames
    // in a row, one finished call is reaped before the next frame. The next
    // reader event is looked at FIRST: an end-of-stream still wins (under the
    // high-water mark; over it reader events wait — see `serve`).
    let mut frames_in_a_row = 0usize;

    let ended = 'conn: loop {
        // The stop, looked at first and without waiting, on EVERY turn. The
        // reaping step below does not go through the select, and it dispatches
        // the frame it takes before it `continue`s back there — so a stop that
        // became ready while frame 16 was handled would otherwise let frame 17
        // be dispatched, and its handler run up to its first await, after the
        // generation was revoked. (This check was removed once as redundant with
        // the select's stop branch; review of ME3-SUP slice 2, round 1, showed
        // why it is not.)
        if std::future::poll_fn(|cx| std::task::Poll::Ready(stop.as_mut().poll(cx).is_ready()))
            .await
        {
            break Ended::Stopped;
        }
        if frames_in_a_row >= FRAMES_BEFORE_REAPING {
            frames_in_a_row = 0;
            // Under the high-water mark only, as in the select below: taking a
            // frame here regardless let every 16th frame's response push the
            // queue further past the mark (review F2 @ #176).
            let stashed =
                if queued.load(std::sync::atomic::Ordering::SeqCst) < limits.queue_high_water {
                    frames_rx.try_recv().ok()
                } else {
                    None
                };
            if let Some(Err(end)) = stashed {
                break conn_end(end);
            }
            // The select below is biased to frames, so it sees a dead writer only
            // when no frame is ready. A peer that sends nothing but
            // notifications never enqueues a response whose send would fail —
            // without this look, it kept the connection reading after the writer
            // had died (review F3 @ #176).
            if writer_task.0.is_finished() {
                writer_done = true;
                break writer_failed((&mut writer_task.0).await);
            }
            if let Some(joined) = conn.handlers.try_join_next_with_id()
                && let Some(response) = conn.finished(joined)
                && let Err(e) = enqueue(response)
            {
                break e;
            }
            if let Some(Ok(bytes)) = stashed {
                frames_in_a_row += 1;
                if let Some(response) = conn.on_frame(&bytes)
                    && let Err(e) = enqueue(response)
                {
                    break e;
                }
            }
            continue;
        }
        let backpressured =
            queued.load(std::sync::atomic::Ordering::SeqCst) >= limits.queue_high_water;
        let response = tokio::select! {
            biased;
            // First here too: biased to frames, a stop placed after them would
            // never wake this select while the peer keeps sending.
            () = &mut stop => break 'conn Ended::Stopped,
            frame = frames_rx.recv(), if !backpressured => match frame {
                Some(Ok(bytes)) => {
                    frames_in_a_row += 1;
                    conn.on_frame(&bytes)
                }
                Some(Err(end)) => break 'conn conn_end(end),
                None => break 'conn Ended::PeerClosed,
            },
            e = &mut writer_task.0 => {
                writer_done = true;
                break 'conn writer_failed(e);
            }
            Some(joined) = conn.handlers.join_next_with_id(), if !conn.handlers.is_empty() => {
                frames_in_a_row = 0;
                conn.finished(joined)
            }
            () = drained.notified(), if backpressured => None,
        };
        if let Some(response) = response
            && let Err(e) = enqueue(response)
        {
            break e;
        }
    };
    // Abort every handler and the writer, and wait until each has actually
    // finished — so on return no handler future is alive and nothing more is
    // written. Unwritten responses are discarded.
    conn.handlers.shutdown().await;
    drop(lines_tx);
    if !writer_done {
        writer_task.0.abort();
        let _ = (&mut writer_task.0).await;
    }
    ended
}

/// After this many frames in a row, [`serve`] reaps one finished call before
/// reading the next frame (see the comment where it is used).
const FRAMES_BEFORE_REAPING: usize = 16;

/// Bytes waiting to be written above which [`serve`] stops reading frames
/// (production value of [`Limits::queue_high_water`]). Two
/// frames' worth: enough that one maximal response never pauses reading on its
/// own. It is checked before each frame is read, so the queue can exceed it by
/// the immediate response to the one frame read just under it, plus the
/// responses of calls already in flight — at most `max_in_flight` of them —
/// each within the frame limit.
pub const QUEUE_HIGH_WATER: usize = 2 * MAX_FRAME_BYTES;

fn writer_failed(joined: Result<std::io::Error, tokio::task::JoinError>) -> Ended {
    Ended::WriteFailed(joined.unwrap_or_else(|_| std::io::Error::other("the writer task failed")))
}

fn conn_end(end: FrameError) -> Ended {
    match end {
        FrameError::TooLong { .. } => Ended::TooLong,
        FrameError::Eof => Ended::PeerClosed,
        FrameError::Io(e) => Ended::ReadFailed(e),
    }
}

type HandlerOutcome = Result<Result<Value, RpcError>, tokio::time::error::Elapsed>;

/// The per-connection call state [`serve`] owns.
struct Conn<'a> {
    methods: &'a Methods,
    limits: Limits,
    handlers: tokio::task::JoinSet<HandlerOutcome>,
    by_task: HashMap<tokio::task::Id, String>,
    in_flight: HashMap<String, tokio::task::AbortHandle>,
}

impl Conn<'_> {
    /// Act on one frame; the response to send now, if any.
    fn on_frame(&mut self, bytes: &[u8]) -> Option<Response> {
        let in_flight = &self.in_flight;
        match dispatch(bytes, self.methods, &|id| in_flight.contains_key(id)) {
            Dispatch::Respond(r) => Some(r),
            Dispatch::Ignore => None,
            Dispatch::Cancel { id } => {
                // The `cancelled` response is sent when the set reports the task
                // finished — after the handler future is dropped.
                if let Some(handle) = self.in_flight.get(&id) {
                    handle.abort();
                }
                None
            }
            Dispatch::Call {
                id,
                params,
                handler,
            } => {
                if self.in_flight.len() >= self.limits.max_in_flight {
                    return Some(Response::error(
                        Some(id),
                        RpcError::application(
                            ErrorKind::Busy,
                            format!(
                                "{} calls are already in flight on this connection",
                                self.limits.max_in_flight
                            ),
                        ),
                    ));
                }
                let timeout = self.limits.call_timeout;
                // `call` runs inside the task, so a handler that panics while
                // building its future is caught too.
                let handle = self.handlers.spawn(async move {
                    tokio::time::timeout(timeout, handler.call(params)).await
                });
                self.by_task.insert(handle.id(), id.clone());
                self.in_flight.insert(id, handle);
                None
            }
        }
    }

    /// Turn a finished handler task into its response, and forget its id.
    fn finished(
        &mut self,
        joined: Result<(tokio::task::Id, HandlerOutcome), tokio::task::JoinError>,
    ) -> Option<Response> {
        let (task, outcome) = match joined {
            Ok((task, Ok(outcome))) => (task, outcome),
            Ok((task, Err(_elapsed))) => (
                task,
                Err(RpcError::application(
                    ErrorKind::Timeout,
                    format!(
                        "the kernel gave up after {}ms; the call is not retried",
                        self.limits.call_timeout.as_millis()
                    ),
                )),
            ),
            Err(e) if e.is_cancelled() => (
                e.id(),
                Err(RpcError::application(
                    ErrorKind::Cancelled,
                    "cancelled by $/cancelRequest; any side effect already committed stays",
                )),
            ),
            Err(e) => (
                e.id(),
                Err(RpcError::internal("the handler failed without answering")),
            ),
        };
        self.by_task.remove(&task).map(|id| {
            self.in_flight.remove(&id);
            Response {
                id: Some(id),
                outcome,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // ── test-only handlers (production registers none) ──────────────────

    /// Answers with its params, after `delay_ms`.
    struct Echo;
    impl Handler for Echo {
        fn check_params(&self, _: &Value) -> Result<(), String> {
            Ok(())
        }
        fn call(&self, params: Value) -> CallFuture {
            Box::pin(async move {
                let ms = params.get("delay_ms").and_then(Value::as_u64).unwrap_or(0);
                tokio::time::sleep(Duration::from_millis(ms)).await;
                Ok(params)
            })
        }
    }

    /// Never answers. Counts calls, and records when its future is dropped —
    /// so a test can tell "it was stopped" from "it is still running unanswered".
    #[derive(Clone, Default)]
    struct Hang {
        calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }
    struct SetOnDrop(Arc<AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    impl Handler for Hang {
        fn check_params(&self, _: &Value) -> Result<(), String> {
            Ok(())
        }
        fn call(&self, _: Value) -> CallFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let guard = SetOnDrop(self.dropped.clone());
            Box::pin(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
                Ok(Value::Null)
            })
        }
    }

    /// Requires `{"n": <number>}`; counts calls.
    #[derive(Clone, Default)]
    struct Strict {
        calls: Arc<AtomicUsize>,
    }
    impl Handler for Strict {
        fn check_params(&self, p: &Value) -> Result<(), String> {
            p.get("n")
                .and_then(Value::as_i64)
                .map(|_| ())
                .ok_or_else(|| "`n` must be a number".to_owned())
        }
        fn call(&self, p: Value) -> CallFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(p) })
        }
    }

    struct Panics;
    impl Handler for Panics {
        fn check_params(&self, _: &Value) -> Result<(), String> {
            Ok(())
        }
        fn call(&self, _: Value) -> CallFuture {
            Box::pin(async move { panic!("handler bug") })
        }
    }

    struct Fixture {
        hang: Hang,
        strict: Strict,
        methods: Methods,
    }

    fn fixture() -> Fixture {
        let hang = Hang::default();
        let strict = Strict::default();
        let methods = Methods::none()
            .with("t/echo", Arc::new(Echo))
            .with("t/hang", Arc::new(hang.clone()))
            .with("t/strict", Arc::new(strict.clone()))
            .with("t/panic", Arc::new(Panics));
        Fixture {
            hang,
            strict,
            methods,
        }
    }

    struct Conn {
        tx: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        rx: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        task: tokio::task::JoinHandle<Ended>,
    }

    fn connect(methods: Methods, limits: Limits) -> Conn {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (sr, sw) = tokio::io::split(server);
        let task = tokio::spawn(serve(BufReader::new(sr), sw, methods, limits));
        let (cr, cw) = tokio::io::split(client);
        Conn {
            tx: cw,
            rx: BufReader::new(cr),
            task,
        }
    }

    impl Conn {
        async fn send(&mut self, v: Value) {
            let mut line = serde_json::to_vec(&v).unwrap();
            line.push(b'\n');
            self.send_raw(&line).await;
        }
        async fn send_raw(&mut self, bytes: &[u8]) {
            self.tx.write_all(bytes).await.unwrap();
            self.tx.flush().await.unwrap();
        }
        async fn recv(&mut self) -> Value {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), self.rx.read_line(&mut line))
                .await
                .expect("no response within 5s")
                .unwrap();
            let v: Value =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line:?}"));
            assert_eq!(v["jsonrpc"], "2.0", "not a JSON-RPC 2.0 response: {line}");
            v
        }
        /// Nothing arrives within `ms` (the connection may still be open).
        async fn silent_for(&mut self, ms: u64) {
            let mut line = String::new();
            if let Ok(r) =
                tokio::time::timeout(Duration::from_millis(ms), self.rx.read_line(&mut line)).await
            {
                assert!(r.unwrap() == 0, "unexpected response: {line}");
            }
        }
    }

    fn req(id: &str, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn kind(v: &Value) -> Option<&str> {
        v["error"]["data"]["kind"].as_str()
    }

    fn code_of(v: &Value) -> i64 {
        v["error"]["code"].as_i64().unwrap_or(0)
    }

    const TEST_LIMITS: Limits = Limits {
        max_in_flight: 8,
        call_timeout: Duration::from_secs(10),
        write_timeout: Duration::from_secs(10),
        queue_high_water: QUEUE_HIGH_WATER,
    };

    // ── SPEC §8 ME-3c, one test per clause ──────────────────────────────

    /// "并发在途请求按 id 正确配对、响应可乱序". The slow call is sent FIRST
    /// and answered LAST — a server that answered in arrival order would fail.
    #[tokio::test]
    async fn concurrent_calls_are_paired_by_id_and_may_answer_out_of_order() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("slow", "t/echo", json!({"delay_ms": 300, "tag": "s"})))
            .await;
        c.send(req("fast", "t/echo", json!({"tag": "f"}))).await;
        let first = c.recv().await;
        let second = c.recv().await;
        assert_eq!(first["id"], "fast");
        assert_eq!(first["result"]["tag"], "f");
        assert_eq!(second["id"], "slow");
        assert_eq!(second["result"]["tag"], "s");
    }

    /// "仍在途的 id 被复用 → 该请求失败" — answered with `id: null` so it
    /// cannot be mistaken for the first call's response, which still arrives.
    /// Control: once the first has answered, the id is free again.
    #[tokio::test]
    async fn a_reused_in_flight_id_fails_that_request_without_touching_the_first() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("a", "t/echo", json!({"delay_ms": 300, "tag": "first"})))
            .await;
        c.send(req("a", "t/echo", json!({"tag": "second"}))).await;
        let dup = c.recv().await;
        assert_eq!(dup["id"], Value::Null);
        assert_eq!(code_of(&dup), i64::from(code::INVALID_REQUEST));
        assert_eq!(dup["error"]["data"]["duplicate_id"], "a");
        let original = c.recv().await;
        assert_eq!(original["id"], "a");
        assert_eq!(original["result"]["tag"], "first");
        // Control: completed ids may be reused (SPEC §3).
        c.send(req("a", "t/echo", json!({"tag": "third"}))).await;
        assert_eq!(c.recv().await["result"]["tag"], "third");
    }

    /// "`$/cancelRequest` 使目标请求回 cancelled 响应" — and the handler is
    /// actually stopped, not left running with its answer thrown away.
    #[tokio::test]
    async fn cancel_answers_cancelled_and_stops_the_handler() {
        let f = fixture();
        let hang = f.hang.clone();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("h", "t/hang", json!({}))).await;
        c.send(json!({"jsonrpc": "2.0", "method": CANCEL_METHOD, "params": {"id": "h"}}))
            .await;
        let r = c.recv().await;
        assert_eq!(r["id"], "h");
        assert_eq!(code_of(&r), i64::from(code::APPLICATION));
        assert_eq!(kind(&r), Some("cancelled"));
        assert_eq!(code_of(&r), i64::from(code::APPLICATION));
        // Asserted at the moment `cancelled` is read — not 50ms later: the
        // response is sent only after the handler future has been dropped.
        assert!(
            hang.dropped.load(Ordering::SeqCst),
            "the handler kept running"
        );
        // Cancelling an id that is not in flight is a no-op, and unanswered
        // (it is a notification).
        c.send(json!({"jsonrpc": "2.0", "method": CANCEL_METHOD, "params": {"id": "nope"}}))
            .await;
        c.silent_for(150).await;
    }

    /// "连接断开则在途请求就地中止、不产生响应". The handler is dropped, and
    /// `serve` returns — nothing is written to a connection that is gone.
    #[tokio::test]
    async fn a_closed_connection_stops_in_flight_calls_without_answering_them() {
        let f = fixture();
        let hang = f.hang.clone();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("h", "t/hang", json!({}))).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            hang.calls.load(Ordering::SeqCst),
            1,
            "precondition: it is running"
        );
        c.tx.shutdown().await.unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(5), c.task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ended, Ended::PeerClosed), "{ended:?}");
        // Asserted at the moment `serve` returns — not 50ms later.
        assert!(
            hang.dropped.load(Ordering::SeqCst),
            "the call outlived its connection"
        );
        let mut rest = String::new();
        let n = c.rx.read_line(&mut rest).await.unwrap();
        assert_eq!(
            n, 0,
            "something was answered after the connection closed: {rest}"
        );
    }

    /// "超时不重试": the handler runs once, is stopped, and the answer is
    /// `timeout`.
    #[tokio::test]
    async fn a_timeout_answers_timeout_and_is_not_retried() {
        let f = fixture();
        let hang = f.hang.clone();
        let mut c = connect(
            f.methods,
            Limits {
                call_timeout: Duration::from_millis(100),
                ..TEST_LIMITS
            },
        );
        let started = std::time::Instant::now();
        c.send(req("h", "t/hang", json!({}))).await;
        let r = c.recv().await;
        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "took {:?} for a 100ms timeout",
            started.elapsed()
        );
        assert_eq!(r["id"], "h");
        assert_eq!(kind(&r), Some("timeout"));
        assert_eq!(code_of(&r), i64::from(code::APPLICATION));
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(hang.calls.load(Ordering::SeqCst), 1, "retried");
        assert!(hang.dropped.load(Ordering::SeqCst));
    }

    /// "params 解析失败固定返回 -32602 且不 dispatch handler" and "一条坏
    /// params 只失败该行 RPC，连接继续处理下一行".
    #[tokio::test]
    async fn bad_params_fail_that_line_without_running_the_handler() {
        let f = fixture();
        let strict = f.strict.clone();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("bad", "t/strict", json!({"n": "not a number"})))
            .await;
        let r = c.recv().await;
        assert_eq!(r["id"], "bad");
        assert_eq!(code_of(&r), i64::from(code::INVALID_PARAMS));
        assert_eq!(strict.calls.load(Ordering::SeqCst), 0, "the handler ran");
        // The connection carries on; control that the same method works.
        c.send(req("good", "t/strict", json!({"n": 1}))).await;
        let r = c.recv().await;
        assert_eq!(r["id"], "good");
        assert_eq!(r["result"]["n"], 1);
        assert_eq!(strict.calls.load(Ordering::SeqCst), 1);
    }

    /// "重复的 JSON object key 被拒" — in params (-32602, the id is sound) and in
    /// the envelope (-32600, answered with `id: null` since which id is meant
    /// cannot be told). Nested duplicates count too.
    #[tokio::test]
    async fn duplicate_keys_are_refused_wherever_they_are() {
        let f = fixture();
        let strict = f.strict.clone();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send_raw(
            br#"{"jsonrpc":"2.0","id":"p","method":"t/strict","params":{"n":1,"n":2}}
"#,
        )
        .await;
        let r = c.recv().await;
        assert_eq!(r["id"], "p");
        assert_eq!(code_of(&r), i64::from(code::INVALID_PARAMS));
        c.send_raw(
            br#"{"jsonrpc":"2.0","id":"q","method":"t/strict","params":{"n":1,"x":{"y":1,"y":2}}}
"#,
        )
        .await;
        assert_eq!(code_of(&c.recv().await), i64::from(code::INVALID_PARAMS));
        c.send_raw(
            br#"{"jsonrpc":"2.0","id":"e1","id":"e2","method":"t/strict","params":{"n":1}}
"#,
        )
        .await;
        let r = c.recv().await;
        assert_eq!(r["id"], Value::Null);
        assert_eq!(code_of(&r), i64::from(code::INVALID_REQUEST));
        assert_eq!(strict.calls.load(Ordering::SeqCst), 0);
        // Control: the same request without the duplicate runs.
        c.send(req("ok", "t/strict", json!({"n": 1}))).await;
        assert_eq!(c.recv().await["result"]["n"], 1);
    }

    /// "握手之后的畸形 JSON → -32700 但只失败该行、不断连".
    #[tokio::test]
    async fn malformed_json_fails_the_line_and_the_connection_continues() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send_raw(b"{not json\n").await;
        let r = c.recv().await;
        assert_eq!(r["id"], Value::Null);
        assert_eq!(code_of(&r), i64::from(code::PARSE_ERROR));
        c.send(req("next", "t/echo", json!({"tag": "x"}))).await;
        assert_eq!(c.recv().await["id"], "next");
    }

    /// "握手成功后重复发 initialize → -32600，同样只失败该行、连接继续".
    #[tokio::test]
    async fn a_second_initialize_fails_that_line_only() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("i", INITIALIZE_METHOD, json!({}))).await;
        let r = c.recv().await;
        assert_eq!(r["id"], "i");
        assert_eq!(code_of(&r), i64::from(code::INVALID_REQUEST));
        c.send(req("next", "t/echo", json!({}))).await;
        assert_eq!(c.recv().await["id"], "next");
    }

    /// "此阶段 offer set 为空，所以调用任何业务方法都应是 -32601，不是
    /// forbidden". With `Methods::none()` — what production serves here — the
    /// memory, events and approval methods of SPEC §3 are all not-found. The
    /// control is the same call succeeding once a (test) handler exists.
    #[tokio::test]
    async fn with_the_empty_offer_set_every_business_method_is_not_found() {
        let mut c = connect(Methods::none(), TEST_LIMITS);
        for m in [
            "_a24/memory/private/remember",
            "_a24/memory/scoped/recall",
            "_a24/events/emit",
            "_a24/approval/request",
        ] {
            c.send(req("x", m, json!({}))).await;
            let r = c.recv().await;
            assert_eq!(code_of(&r), i64::from(code::METHOD_NOT_FOUND), "{m}");
            assert_eq!(
                kind(&r),
                None,
                "{m}: not-found must not be dressed as forbidden"
            );
        }
        let mut c = connect(
            Methods::none().with("_a24/events/emit", Arc::new(Echo)),
            TEST_LIMITS,
        );
        c.send(req("x", "_a24/events/emit", json!({}))).await;
        assert!(c.recv().await.get("result").is_some());
    }

    /// Over the limit → `busy` at once, not queued. Control: under it, calls run.
    #[tokio::test]
    async fn beyond_the_ceiling_a_call_is_busy_not_queued() {
        let f = fixture();
        let mut c = connect(
            f.methods,
            Limits {
                max_in_flight: 1,
                ..TEST_LIMITS
            },
        );
        c.send(req("h", "t/hang", json!({}))).await;
        c.send(req("b", "t/echo", json!({}))).await;
        let r = c.recv().await;
        assert_eq!(r["id"], "b");
        assert_eq!(kind(&r), Some("busy"));
        assert_eq!(code_of(&r), i64::from(code::APPLICATION));
        c.send(json!({"jsonrpc": "2.0", "method": CANCEL_METHOD, "params": {"id": "h"}}))
            .await;
        assert_eq!(kind(&c.recv().await), Some("cancelled"));
        c.send(req("c", "t/echo", json!({}))).await;
        assert!(
            c.recv().await.get("result").is_some(),
            "the slot was not freed"
        );
    }

    /// A handler that panics is answered, not left in flight forever.
    #[tokio::test]
    async fn a_panicking_handler_is_answered_with_an_internal_error() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("p", "t/panic", json!({}))).await;
        let r = c.recv().await;
        assert_eq!(r["id"], "p");
        assert_eq!(code_of(&r), i64::from(code::INTERNAL_ERROR));
        c.send(req("p", "t/echo", json!({}))).await;
        assert!(
            c.recv().await.get("result").is_some(),
            "the id stayed in flight"
        );
    }

    /// Notifications are never answered — an unknown one, and a malformed cancel.
    #[tokio::test]
    async fn notifications_are_never_answered() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(json!({"jsonrpc": "2.0", "method": "t/echo", "params": {}}))
            .await;
        c.send(json!({"jsonrpc": "2.0", "method": CANCEL_METHOD, "params": {"id": 7}}))
            .await;
        c.send(json!({"jsonrpc": "2.0", "method": "t/nope"})).await;
        c.silent_for(200).await;
        c.send(req("after", "t/echo", json!({}))).await;
        assert_eq!(c.recv().await["id"], "after");
    }

    /// Shapes JSON-RPC allows but SPEC §3 does not: non-string ids, batches,
    /// non-object params, unknown members.
    #[tokio::test]
    async fn request_shapes_outside_spec_are_invalid_requests() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "t/echo"}))
            .await;
        let r = c.recv().await;
        assert_eq!(
            (r["id"].clone(), code_of(&r)),
            (Value::Null, i64::from(code::INVALID_REQUEST))
        );
        c.send(json!([req("a", "t/echo", json!({}))])).await;
        assert_eq!(code_of(&c.recv().await), i64::from(code::INVALID_REQUEST));
        c.send(json!({"jsonrpc": "2.0", "id": "a", "method": "t/echo", "params": [1]}))
            .await;
        assert_eq!(code_of(&c.recv().await), i64::from(code::INVALID_PARAMS));
        c.send(json!({"jsonrpc": "2.0", "id": "a", "method": "t/echo", "extra": 1}))
            .await;
        assert_eq!(code_of(&c.recv().await), i64::from(code::INVALID_REQUEST));
        c.send(json!({"jsonrpc": "1.0", "id": "a", "method": "t/echo"}))
            .await;
        assert_eq!(code_of(&c.recv().await), i64::from(code::INVALID_REQUEST));
        c.send(req("a", CANCEL_METHOD, json!({"id": "x"}))).await;
        assert_eq!(code_of(&c.recv().await), i64::from(code::INVALID_REQUEST));
    }

    /// "握手后的超长行被拒并断连" — the one post-handshake failure that
    /// disconnects. Control: a line of EXACTLY the limit is served.
    #[tokio::test]
    async fn an_over_long_line_disconnects_and_one_at_the_limit_is_served() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        let skeleton = r#"{"jsonrpc":"2.0","id":"big","method":"t/nope","params":{"pad":""}}"#;
        let pad = "x".repeat(MAX_FRAME_BYTES - skeleton.len());
        let exact = skeleton.replace(r#""pad":"""#, &format!(r#""pad":"{pad}""#));
        assert_eq!(exact.len(), MAX_FRAME_BYTES);
        let tx = &mut c.tx;
        let (w, r) = tokio::join!(
            async {
                tx.write_all(exact.as_bytes()).await.unwrap();
                tx.write_all(b"\n").await.unwrap();
                tx.flush().await.unwrap();
            },
            async {
                let mut line = String::new();
                c.rx.read_line(&mut line).await.unwrap();
                serde_json::from_str::<Value>(&line).unwrap()
            }
        );
        let () = w;
        assert_eq!(r["id"], "big");
        assert_eq!(code_of(&r), i64::from(code::METHOD_NOT_FOUND));

        let over = format!("{exact}x\n");
        let writer = tokio::spawn({
            let mut tx = c.tx;
            async move {
                let _ = tx.write_all(over.as_bytes()).await;
            }
        });
        let ended = tokio::time::timeout(Duration::from_secs(5), c.task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ended, Ended::TooLong), "{ended:?}");
        writer.abort();
        // SPEC's ME-3c table: disconnect without answering first.
        let mut rest = String::new();
        assert_eq!(
            c.rx.read_line(&mut rest).await.unwrap(),
            0,
            "answered before disconnecting: {rest}"
        );
    }

    // ── the pieces ──────────────────────────────────────────────────────

    /// The async reader and 3b-1's sync reader agree on every edge: exact limit,
    /// one past it, a final line without a newline, empty lines.
    #[tokio::test]
    async fn the_async_frame_reader_agrees_with_the_sync_one() {
        fn sync_read(bytes: &[u8]) -> Vec<Result<Vec<u8>, String>> {
            let mut r = std::io::BufReader::new(bytes);
            let mut out = Vec::new();
            loop {
                match crate::frame::read_frame(&mut r) {
                    Ok(f) => out.push(Ok(f)),
                    Err(e) => {
                        out.push(Err(format!("{e:?}")
                            .split_whitespace()
                            .next()
                            .unwrap()
                            .to_owned()));
                        return out;
                    }
                }
            }
        }
        async fn async_read(bytes: &[u8]) -> Vec<Result<Vec<u8>, String>> {
            let mut r = tokio::io::BufReader::new(bytes);
            let mut out = Vec::new();
            loop {
                match read_frame_async(&mut r).await {
                    Ok(f) => out.push(Ok(f)),
                    Err(e) => {
                        out.push(Err(format!("{e:?}")
                            .split_whitespace()
                            .next()
                            .unwrap()
                            .to_owned()));
                        return out;
                    }
                }
            }
        }
        let exact = [vec![b'a'; MAX_FRAME_BYTES], b"\n".to_vec()].concat();
        let over = [vec![b'a'; MAX_FRAME_BYTES + 1], b"\n".to_vec()].concat();
        for input in [
            b"one\ntwo\n".to_vec(),
            b"\n\n".to_vec(),
            b"no newline".to_vec(),
            b"".to_vec(),
            exact,
            over,
        ] {
            let s = sync_read(&input);
            let a = async_read(&input).await;
            assert_eq!(s, a, "disagree on an input of {} bytes", input.len());
        }
    }

    /// SPEC §3's closed set, quoted: the kinds here are exactly the backticked
    /// words of the sentence — not one more, not one fewer — and the handshake's
    /// kinds (ME-3b-2b) are members of it. (SPEC §8: a wire constant must appear
    /// as a whole word in the quoted SPEC text.)
    #[test]
    fn the_error_kinds_are_exactly_specs_closed_set() {
        const SPEC: &str = "kind 是闭集：`forbidden` / `busy` / `cancelled` / `timeout` / `quota_exceeded` / `invalid_lease` / `unknown_capability` / `version_mismatch` / **`auth_failed`** / **`manifest_mismatch`**";
        let quoted: HashSet<&str> = SPEC.split('`').skip(1).step_by(2).collect();
        let ours: HashSet<&str> = ErrorKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(ours, quoted);
        assert_eq!(
            ours.len(),
            ErrorKind::ALL.len(),
            "two kinds share a wire string"
        );
        for k in ["auth_failed", "manifest_mismatch", "version_mismatch"] {
            assert!(
                ours.contains(k),
                "handshake kind {k} is not in the closed set"
            );
        }
    }

    // ── review round 1 (Codex + an independent reviewer) ────────────────

    /// Answers with a string larger than the frame limit.
    struct Big;
    impl Handler for Big {
        fn check_params(&self, _: &Value) -> Result<(), String> {
            Ok(())
        }
        fn call(&self, _: Value) -> CallFuture {
            Box::pin(async move { Ok(Value::String("x".repeat(MAX_FRAME_BYTES + 10))) })
        }
    }

    /// Its params check panics.
    struct PanicCheck;
    impl Handler for PanicCheck {
        fn check_params(&self, _: &Value) -> Result<(), String> {
            panic!("validator bug")
        }
        fn call(&self, _: Value) -> CallFuture {
            Box::pin(async move { Ok(Value::Null) })
        }
    }

    /// The in-flight check comes BEFORE every other validation: a malformed
    /// request that reuses a running id is answered `id: null` too — otherwise
    /// its error is paired with the call still running.
    #[tokio::test]
    async fn a_reused_id_is_answered_null_even_when_the_request_is_also_malformed() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("a", "t/hang", json!({}))).await;
        let shapes = [
            json!({"jsonrpc": "2.0", "id": "a", "method": "t/echo", "params": [1]}),
            json!({"jsonrpc": "2.0", "id": "a", "method": INITIALIZE_METHOD}),
            json!({"jsonrpc": "2.0", "id": "a", "method": CANCEL_METHOD, "params": {"id": "a"}}),
            json!({"jsonrpc": "2.0", "id": "a", "method": "t/echo", "extra": 1}),
            json!({"jsonrpc": "1.0", "id": "a", "method": "t/echo"}),
            json!({"jsonrpc": "2.0", "id": "a", "method": 7}),
        ];
        for shape in shapes {
            c.send(shape.clone()).await;
            let r = c.recv().await;
            assert_eq!(r["id"], Value::Null, "{shape}");
            assert_eq!(r["error"]["data"]["duplicate_id"], "a", "{shape}");
        }
        c.send_raw(
            br#"{"jsonrpc":"2.0","id":"a","method":"t/strict","params":{"n":1,"n":2}}
"#,
        )
        .await;
        let r = c.recv().await;
        assert_eq!(r["id"], Value::Null, "params duplicate on a running id");
        assert_eq!(r["error"]["data"]["duplicate_id"], "a");
    }

    /// A cancel stops its target and nothing else.
    #[tokio::test]
    async fn a_cancel_stops_only_its_target() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("x", "t/echo", json!({"delay_ms": 300, "tag": "x"})))
            .await;
        c.send(req("y", "t/echo", json!({"delay_ms": 300, "tag": "y"})))
            .await;
        c.send(json!({"jsonrpc": "2.0", "method": CANCEL_METHOD, "params": {"id": "x"}}))
            .await;
        let first = c.recv().await;
        assert_eq!(
            (first["id"].clone(), kind(&first)),
            (json!("x"), Some("cancelled"))
        );
        let second = c.recv().await;
        assert_eq!(second["id"], "y");
        assert_eq!(second["result"]["tag"], "y");
    }

    /// Where a repeated key sits decides the answer: the envelope (including
    /// `params` itself twice) → -32600 null; inside params, at any depth
    /// including inside arrays → -32602 with the id; no id member → a
    /// notification, unanswered; truncated JSON → -32700, whatever else.
    #[tokio::test]
    async fn repeated_keys_are_classified_by_where_they_are() {
        let f = fixture();
        let strict = f.strict.clone();
        let mut c = connect(f.methods, TEST_LIMITS);
        for (raw, code, id) in [
            (&br#"{"jsonrpc":"2.0","id":"p","method":"t/strict","params":{"n":1},"params":{"n":2}}"#[..], code::INVALID_REQUEST, Value::Null),
            (&br#"{"jsonrpc":"2.0","id":"m","method":"t/strict","method":"t/echo","params":{"n":1}}"#[..], code::INVALID_REQUEST, Value::Null),
            (&br#"{"jsonrpc":"2.0","id":"q","method":"t/strict","params":{"n":1,"l":[{"k":1,"k":2}]}}"#[..], code::INVALID_PARAMS, json!("q")),
            (&br#"{"jsonrpc":"2.0","id":7,"method":"t/strict","params":{"n":1,"n":2}}"#[..], code::INVALID_REQUEST, Value::Null),
            (&br#"{"jsonrpc":"2.0","id":"t","method":"t/strict","params":{"n":1,"n":2"#[..], code::PARSE_ERROR, Value::Null),
        ] {
            c.send_raw(&[raw, b"\n"].concat()).await;
            let r = c.recv().await;
            assert_eq!(code_of(&r), i64::from(code), "{}", String::from_utf8_lossy(raw));
            assert_eq!(r["id"], id, "{}", String::from_utf8_lossy(raw));
        }
        // A notification with a repeated key: never answered.
        c.send_raw(
            br#"{"jsonrpc":"2.0","method":"t/echo","method":"t/strict"}
"#,
        )
        .await;
        c.silent_for(150).await;
        assert_eq!(strict.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_id_longer_than_the_limit_is_refused() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req(&"i".repeat(MAX_ID_BYTES + 1), "t/echo", json!({})))
            .await;
        let r = c.recv().await;
        assert_eq!(
            (r["id"].clone(), code_of(&r)),
            (Value::Null, i64::from(code::INVALID_REQUEST))
        );
        // Control: exactly the limit is fine.
        let id = "i".repeat(MAX_ID_BYTES);
        c.send(req(&id, "t/echo", json!({}))).await;
        assert_eq!(c.recv().await["id"], json!(id));
    }

    /// A blank line is not JSON: -32700, and the connection carries on.
    #[tokio::test]
    async fn a_blank_line_is_a_parse_error() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send_raw(b"\n").await;
        assert_eq!(code_of(&c.recv().await), i64::from(code::PARSE_ERROR));
        c.send(req("n", "t/echo", json!({}))).await;
        assert_eq!(c.recv().await["id"], "n");
    }

    /// The envelope is closed: `method` must be a string, `jsonrpc` must be
    /// present, `_meta` belongs in params not beside it. Absent params are `{}`.
    #[tokio::test]
    async fn the_envelope_is_strict_and_absent_params_are_an_empty_object() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        for shape in [
            json!({"jsonrpc": "2.0", "id": "a", "method": 7}),
            json!({"id": "a", "method": "t/echo"}),
            json!({"jsonrpc": "2.0", "id": "a", "method": "t/echo", "_meta": {}}),
        ] {
            c.send(shape.clone()).await;
            let r = c.recv().await;
            assert_eq!(
                (r["id"].clone(), code_of(&r)),
                (json!("a"), i64::from(code::INVALID_REQUEST)),
                "{shape}"
            );
        }
        c.send(json!({"jsonrpc": "2.0", "id": "b", "method": "t/echo"}))
            .await;
        assert_eq!(c.recv().await["result"], json!({}));
    }

    /// `$/cancelRequest` params are `{id, _meta?}` — an extra member makes it a
    /// malformed notification, which is dropped (the target keeps running).
    #[tokio::test]
    async fn a_cancel_with_unknown_params_is_ignored_and_meta_is_allowed() {
        let f = fixture();
        let mut c = connect(f.methods, TEST_LIMITS);
        c.send(req("h", "t/hang", json!({}))).await;
        c.send(
            json!({"jsonrpc": "2.0", "method": CANCEL_METHOD, "params": {"id": "h", "why": "x"}}),
        )
        .await;
        c.silent_for(150).await;
        c.send(json!({"jsonrpc": "2.0", "method": CANCEL_METHOD, "params": {"id": "h", "_meta": {"k": 1}}})).await;
        let r = c.recv().await;
        assert_eq!((r["id"].clone(), kind(&r)), (json!("h"), Some("cancelled")));
    }

    /// A params check that panics fails that one request (-32603) and the
    /// connection — with its other calls — carries on.
    #[tokio::test]
    async fn a_panicking_params_check_fails_only_that_request() {
        let mut c = connect(
            fixture()
                .methods
                .with("t/panic-check", Arc::new(PanicCheck)),
            TEST_LIMITS,
        );
        c.send(req("slow", "t/echo", json!({"delay_ms": 200})))
            .await;
        c.send(req("p", "t/panic-check", json!({}))).await;
        let r = c.recv().await;
        assert_eq!(
            (r["id"].clone(), code_of(&r)),
            (json!("p"), i64::from(code::INTERNAL_ERROR))
        );
        assert_eq!(c.recv().await["id"], "slow", "the other call was lost");
    }

    /// A response larger than the frame limit is replaced by an error for the
    /// same id — the peer would disconnect on it.
    #[tokio::test]
    async fn a_response_over_the_frame_limit_becomes_an_error() {
        let mut c = connect(fixture().methods.with("t/big", Arc::new(Big)), TEST_LIMITS);
        c.send(req("b", "t/big", json!({}))).await;
        let r = c.recv().await;
        assert_eq!(
            (r["id"].clone(), code_of(&r)),
            (json!("b"), i64::from(code::INTERNAL_ERROR))
        );
        c.send(req("n", "t/echo", json!({}))).await;
        assert_eq!(c.recv().await["id"], "n");
    }

    /// A module that stops reading must not freeze the kernel's side: the
    /// connection ends within the write deadline, and its handlers are dropped.
    #[tokio::test]
    async fn a_peer_that_stops_reading_ends_the_connection_instead_of_freezing_it() {
        let f = fixture();
        let hang = f.hang.clone();
        let (client, server) = tokio::io::duplex(256);
        let (sr, sw) = tokio::io::split(server);
        let task = tokio::spawn(serve(
            BufReader::new(sr),
            sw,
            f.methods,
            Limits {
                write_timeout: Duration::from_millis(200),
                ..TEST_LIMITS
            },
        ));
        let (_client_rx, mut client_tx) = tokio::io::split(client); // never read
        let line = |id: &str, m: &str, p: Value| {
            let mut l = serde_json::to_vec(&req(id, m, p)).unwrap();
            l.push(b'\n');
            l
        };
        client_tx
            .write_all(&line("h", "t/hang", json!({})))
            .await
            .unwrap();
        for i in 0..4 {
            let pad = "p".repeat(4096);
            client_tx
                .write_all(&line(&format!("e{i}"), "t/echo", json!({"pad": pad})))
                .await
                .unwrap();
        }
        let started = std::time::Instant::now();
        let ended = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the connection froze instead of ending")
            .unwrap();
        assert!(
            matches!(&ended, Ended::WriteFailed(e) if e.kind() == std::io::ErrorKind::TimedOut),
            "{ended:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            hang.dropped.load(Ordering::SeqCst),
            "a handler outlived the connection"
        );
    }

    struct FailingWriter;
    impl tokio::io::AsyncWrite for FailingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::other("write boom")))
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    struct FailingReader;
    impl tokio::io::AsyncRead for FailingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::Error::other("read boom")))
        }
    }

    /// Each way a connection ends is reported as itself — a write failure is not
    /// a close, a read failure is not a close.
    #[tokio::test]
    async fn write_and_read_failures_are_reported_as_themselves() {
        let (client, server) = tokio::io::duplex(1024);
        let (sr, _sw) = tokio::io::split(server);
        let task = tokio::spawn(serve(
            BufReader::new(sr),
            FailingWriter,
            fixture().methods,
            TEST_LIMITS,
        ));
        let (_rx, mut tx) = tokio::io::split(client);
        tx.write_all(
            &[
                serde_json::to_vec(&req("a", "t/echo", json!({}))).unwrap(),
                b"\n".to_vec(),
            ]
            .concat(),
        )
        .await
        .unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(&ended, Ended::WriteFailed(e) if e.to_string() == "write boom"),
            "{ended:?}"
        );

        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            serve(
                BufReader::new(FailingReader),
                tokio::io::sink(),
                fixture().methods,
                TEST_LIMITS,
            ),
        )
        .await
        .unwrap();
        assert!(
            matches!(&ended, Ended::ReadFailed(e) if e.to_string() == "read boom"),
            "{ended:?}"
        );
    }

    /// The production limits are the documented constants.
    #[test]
    fn the_default_limits_are_the_documented_constants() {
        assert_eq!(
            Limits::default(),
            Limits {
                max_in_flight: MAX_IN_FLIGHT_PER_CONNECTION,
                call_timeout: CALL_TIMEOUT,
                write_timeout: WRITE_TIMEOUT,
                queue_high_water: QUEUE_HIGH_WATER,
            }
        );
        assert_eq!(MAX_IN_FLIGHT_PER_CONNECTION, 64);
        assert_eq!(CALL_TIMEOUT, Duration::from_secs(30));
    }

    /// The handshake's kinds come from `initialize` itself, not from a list
    /// retyped here — so renaming one there turns this red.
    #[test]
    fn the_handshakes_error_kinds_are_members_of_the_closed_set() {
        use crate::initialize::HandshakeError;
        let ours: HashSet<&str> = ErrorKind::ALL.iter().map(|k| k.as_str()).collect();
        let theirs = [
            HandshakeError::AuthFailed.kind(),
            HandshakeError::ManifestMismatch {
                expected: String::new(),
                got: String::new(),
            }
            .kind(),
            Some(crate::version::VersionMismatch::KIND),
        ];
        for k in theirs {
            let k = k.expect("a handshake kind is missing");
            assert!(
                ours.contains(k),
                "handshake kind `{k}` is not in the closed set"
            );
        }
    }

    /// The empty offer set, on both sides: the handshake offers none of SPEC
    /// §3's business methods, and the connection serves none of them.
    #[test]
    fn the_offer_and_the_served_methods_are_both_empty() {
        let offer = crate::initialize::Offer::none();
        for m in [
            "_a24/memory/private/remember",
            "_a24/memory/scoped/remember",
            "_a24/events/emit",
            "_a24/approval/request",
        ] {
            assert!(!offer.provides(m), "{m} is offered");
        }
        assert!(Methods::none().map.is_empty());
    }

    /// The boundary exactly: `serve` is awaited HERE, in the test's own task, so
    /// nothing else runs between its return and the assertion. (Awaiting a
    /// spawned `serve` yields to the runtime first, which gets to finish an
    /// abort that `serve` itself never waited for — measured: with
    /// `abort_all()` in place of `shutdown().await`, the spawned-task version of
    /// this test stays green.)
    #[tokio::test]
    async fn when_serve_returns_every_handler_future_is_already_dropped() {
        let f = fixture();
        let hang = f.hang.clone();
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (sr, sw) = tokio::io::split(server);
        let calls = hang.calls.clone();
        let module = tokio::spawn(async move {
            let (_rx, mut tx) = tokio::io::split(client);
            let mut l = serde_json::to_vec(&req("h", "t/hang", json!({}))).unwrap();
            l.push(b'\n');
            tx.write_all(&l).await.unwrap();
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            tx.shutdown().await.unwrap();
            // keep `_rx` alive until the end so the close is a clean EOF
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        let ended = serve(BufReader::new(sr), sw, f.methods, TEST_LIMITS).await;
        let dropped_at_return = hang.dropped.load(Ordering::SeqCst);
        assert!(matches!(ended, Ended::PeerClosed), "{ended:?}");
        assert!(
            dropped_at_return,
            "serve returned while a handler future was still alive"
        );
        module.await.unwrap();
    }

    // ── review round 2 ─────────────────────────────────────────────────

    /// A burst that fills the response queue while the peer IS reading (just
    /// slower than the kernel writes) is not "the peer stopped reading": every
    /// response arrives and the connection stays up.
    #[tokio::test]
    async fn a_burst_that_fills_the_queue_is_not_mistaken_for_a_peer_that_stopped_reading() {
        let limits = Limits {
            max_in_flight: 2, // queue = 2 + 16
            write_timeout: Duration::from_secs(3),
            ..TEST_LIMITS
        };
        let (client, server) = tokio::io::duplex(256); // the writer blocks almost at once
        let (sr, sw) = tokio::io::split(server);
        let task = tokio::spawn(serve(BufReader::new(sr), sw, fixture().methods, limits));
        let (cr, mut cw) = tokio::io::split(client);
        let burst: Vec<u8> = (0..40)
            .flat_map(|_| b"{not json\n".iter().copied())
            .collect();
        cw.write_all(&burst).await.unwrap();
        let mut rx = BufReader::new(cr);
        for i in 0..40 {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), rx.read_line(&mut line))
                .await
                .unwrap_or_else(|_| panic!("response {i} never came"))
                .unwrap();
            assert!(line.contains("-32700"), "{line}");
            tokio::time::sleep(Duration::from_millis(5)).await; // a slow reader
        }
        assert!(
            !task.is_finished(),
            "the connection was ended while the peer was reading"
        );
        cw.shutdown().await.unwrap(); // dropping one half does not close a duplex
        let ended = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ended, Ended::PeerClosed), "{ended:?}");
    }

    /// A writer that enters a write and never returns, and records being dropped.
    struct StuckWriter {
        entered: Arc<AtomicBool>,
        _dropped: SetOnDrop,
    }
    impl tokio::io::AsyncWrite for StuckWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.entered.store(true, Ordering::SeqCst);
            std::task::Poll::Pending
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
        fn poll_shutdown(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// When `serve` returns, the writer has stopped too — not merely been asked
    /// to. `serve` is awaited in the test's own task, so nothing runs between its
    /// return and the assertion.
    #[tokio::test]
    async fn when_serve_returns_the_writer_has_stopped() {
        let entered = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let writer = StuckWriter {
            entered: entered.clone(),
            _dropped: SetOnDrop(dropped.clone()),
        };
        let (client, server) = tokio::io::duplex(1024);
        let module = tokio::spawn({
            let entered = entered.clone();
            async move {
                let (_rx, mut tx) = tokio::io::split(client);
                tx.write_all(b"{not json\n").await.unwrap();
                while !entered.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                tx.shutdown().await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        let (sr, _sw) = tokio::io::split(server);
        let ended = serve(BufReader::new(sr), writer, fixture().methods, TEST_LIMITS).await;
        let dropped_at_return = dropped.load(Ordering::SeqCst);
        assert!(
            entered.load(Ordering::SeqCst),
            "precondition: the writer was mid-write"
        );
        assert!(matches!(ended, Ended::PeerClosed), "{ended:?}");
        assert!(
            dropped_at_return,
            "serve returned while the writer was still alive"
        );
        module.await.unwrap();
    }

    /// Every repeated key is found; an envelope repeat outranks an earlier one
    /// inside params (which would otherwise be answered with whichever id parsed
    /// last).
    #[tokio::test]
    async fn an_envelope_repeat_outranks_an_earlier_repeat_in_params() {
        let mut c = connect(fixture().methods, TEST_LIMITS);
        c.send_raw(
            br#"{"jsonrpc":"2.0","id":"a","params":{"x":1,"x":2},"id":"b","method":"t/echo"}
"#,
        )
        .await;
        let r = c.recv().await;
        assert_eq!(
            (r["id"].clone(), code_of(&r)),
            (Value::Null, i64::from(code::INVALID_REQUEST))
        );
    }

    // ── review round 3 ─────────────────────────────────────────────────

    /// Below the high-water mark the loop never waits on the peer: with the
    /// writer stuck and small responses queued behind it, a cancel that arrives
    /// is still acted on at once. (The version before waited for queue space inside the loop, and
    /// saw nothing — cancel, EOF, finished calls — for up to the write deadline.)
    #[tokio::test]
    async fn a_stuck_writer_does_not_stop_the_loop_from_acting_on_a_cancel() {
        let f = fixture();
        let hang = f.hang.clone();
        let entered = Arc::new(AtomicBool::new(false));
        let writer = StuckWriter {
            entered: entered.clone(),
            _dropped: SetOnDrop(Arc::new(AtomicBool::new(false))),
        };
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (sr, _sw) = tokio::io::split(server);
        let task = tokio::spawn(serve(
            BufReader::new(sr),
            writer,
            f.methods,
            Limits {
                max_in_flight: 2, // the old queue held 2 + 16
                write_timeout: Duration::from_secs(10),
                ..TEST_LIMITS
            },
        ));
        let (_rx, mut tx) = tokio::io::split(client);
        let mut lines = serde_json::to_vec(&req("h", "t/hang", json!({}))).unwrap();
        lines.push(b'\n');
        for _ in 0..40 {
            lines.extend_from_slice(b"{not json\n");
        }
        lines.extend_from_slice(
            format!(
                "{}\n",
                json!({"jsonrpc": "2.0", "method": CANCEL_METHOD, "params": {"id": "h"}})
            )
            .as_bytes(),
        );
        tx.write_all(&lines).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !hang.dropped.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "the cancel was not acted on while the writer was stuck"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            entered.load(Ordering::SeqCst),
            "precondition: the writer was stuck"
        );
        task.abort();
    }

    /// One frame of long nested keys and many repeated leaves must not cost far
    /// more than the frame: only the two candidate repeats are kept, not a copy
    /// of the path for every repeat.
    #[test]
    fn many_repeated_keys_under_a_long_path_are_classified_cheaply() {
        let depth = 20;
        let key = "k".repeat(1000); // a ~20 KB path
        let mut frame = String::from(r#"{"jsonrpc":"2.0","id":"a","method":"t/echo","params":"#);
        for _ in 0..depth {
            frame.push_str(&format!(r#"{{"{key}":"#));
        }
        frame.push('{');
        let leaves: Vec<String> = (0..20_000).map(|i| format!(r#""d":{i}"#)).collect();
        frame.push_str(&leaves.join(","));
        frame.push('}');
        for _ in 0..depth {
            frame.push('}');
        }
        frame.push('}');
        assert!(frame.len() < MAX_FRAME_BYTES);
        let (found, copies) = scan_duplicates(frame.as_bytes());
        assert!(found.is_some());
        assert!(copies <= 2, "{copies} paths were copied for one frame");
        let started = std::time::Instant::now();
        let d = dispatch(frame.as_bytes(), &fixture().methods, &|_| false);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "took {:?}",
            started.elapsed()
        );
        let Dispatch::Respond(r) = d else {
            panic!("a repeated key must be refused")
        };
        assert_eq!(r.outcome.unwrap_err().code, code::INVALID_PARAMS);
    }

    /// Above the high-water mark reading stops — even a cancel waits — and it
    /// resumes when the peer drains the queue. Both halves: a watermark that
    /// never paused, or a pause that never woke up, each fail one assertion.
    #[tokio::test]
    async fn over_the_high_water_mark_reading_pauses_and_resumes_when_drained() {
        let f = fixture();
        let hang = f.hang.clone();
        let (client, server) = tokio::io::duplex(16); // a write of any response blocks
        let (sr, sw) = tokio::io::split(server);
        let task = tokio::spawn(serve(
            BufReader::new(sr),
            sw,
            f.methods,
            Limits {
                queue_high_water: 1,
                write_timeout: Duration::from_secs(10),
                ..TEST_LIMITS
            },
        ));
        let (cr, mut cw) = tokio::io::split(client);
        let line = |v: Value| format!("{v}\n");
        cw.write_all(line(req("h", "t/hang", json!({}))).as_bytes())
            .await
            .unwrap();
        while hang.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        // One response queued and stuck behind the 16-byte pipe → over the mark.
        cw.write_all(b"{not json\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        cw.write_all(
            line(json!({"jsonrpc": "2.0", "method": CANCEL_METHOD, "params": {"id": "h"}}))
                .as_bytes(),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !hang.dropped.load(Ordering::SeqCst),
            "a frame was read while over the high-water mark"
        );
        // Drain: read the parse-error response; reading must resume and the
        // cancel take effect.
        let mut rx = BufReader::new(cr);
        let mut first = String::new();
        tokio::time::timeout(Duration::from_secs(5), rx.read_line(&mut first))
            .await
            .unwrap()
            .unwrap();
        assert!(first.contains("-32700"), "{first}");
        let mut second = String::new();
        tokio::time::timeout(Duration::from_secs(5), rx.read_line(&mut second))
            .await
            .expect("reading never resumed after the queue drained")
            .unwrap();
        assert!(second.contains("cancelled"), "{second}");
        assert!(hang.dropped.load(Ordering::SeqCst));
        task.abort();
    }

    // ── review of #176 (approved; non-blocking F2, F3, F5) ────────────────

    /// F5: a method name or an envelope key is the peer's string, and the error
    /// that names it is capped like every other echo — a 200 KiB method name is
    /// still `-32601`, not a 200 KiB message (or, near the frame limit, a
    /// `-32603` for a response that grew too big).
    #[test]
    fn a_huge_method_name_or_repeated_key_is_echoed_clipped() {
        let long = "m".repeat(200 * 1024);
        let frame = serde_json::to_vec(&req("a", &long, json!({}))).unwrap();
        let Dispatch::Respond(r) = dispatch(&frame, &fixture().methods, &|_| false) else {
            panic!("an unknown method must be answered")
        };
        let e = r.outcome.unwrap_err();
        assert_eq!(e.code, code::METHOD_NOT_FOUND);
        assert!(e.message.len() < 256, "{} bytes", e.message.len());

        let key = "k".repeat(300 * 1024);
        let frame =
            format!(r#"{{"jsonrpc":"2.0","id":"a","method":"t/echo","{key}":1,"{key}":2}}"#);
        let Dispatch::Respond(r) = dispatch(frame.as_bytes(), &fixture().methods, &|_| false)
        else {
            panic!("a repeated key must be answered")
        };
        let e = r.outcome.unwrap_err();
        assert_eq!(e.code, code::INVALID_REQUEST);
        assert!(e.message.contains("duplicate key"), "{}", e.message);
        assert!(e.message.len() < 256, "{} bytes", e.message.len());
    }

    /// Records that its params check ran — which happens inside `dispatch`, in
    /// the loop itself, the moment its frame is read.
    struct Probe {
        seen: Arc<AtomicBool>,
    }
    impl Handler for Probe {
        fn check_params(&self, _: &Value) -> Result<(), String> {
            self.seen.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn call(&self, _: Value) -> CallFuture {
            Box::pin(async move { Ok(Value::Null) })
        }
    }

    fn frame_of(v: &Value) -> Frame {
        Ok(serde_json::to_vec(v).unwrap())
    }

    /// F2: over the high-water mark no frame is read — including by the reaping
    /// step every 16 frames. Sixteen requests, answered at once, fill the queue
    /// to the mark behind a writer that never finishes a write; the seventeenth
    /// frame is already waiting when the reaping step runs. It must stay unread.
    ///
    /// Fed through `run` with every frame queued up front: through `serve`, the
    /// seventeenth frame is ready at that moment only if the reader task ran on
    /// another thread in time — the first version of this test depended on that
    /// and was not reliably red on its mutant (review of f2b4e2e, L2).
    #[tokio::test]
    async fn the_reaping_step_does_not_read_past_the_high_water_mark() {
        let seen = Arc::new(AtomicBool::new(false));
        let methods = fixture()
            .methods
            .with("t/probe", Arc::new(Probe { seen: seen.clone() }));
        let one = response_line(Response::error(
            Some("00".to_owned()),
            RpcError::method_not_found("t/none"),
        ))
        .len();
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        for i in 0..16 {
            tx.send(frame_of(&req(&format!("{i:02}"), "t/none", json!({}))))
                .await
                .unwrap();
        }
        tx.send(frame_of(&req("16", "t/probe", json!({}))))
            .await
            .unwrap();
        let writer = StuckWriter {
            entered: Arc::new(AtomicBool::new(false)),
            _dropped: SetOnDrop(Arc::new(AtomicBool::new(false))),
        };
        let task = tokio::spawn(async move {
            let mut rx = rx;
            let limits = Limits {
                queue_high_water: 16 * one,
                ..TEST_LIMITS
            };
            run(&mut rx, writer, methods, limits).await
        });
        // The loop takes the sixteen frames and reaches the reaping step in one
        // poll, so once they are gone the reaping step has run.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while tx.capacity() < 31 {
            assert!(
                std::time::Instant::now() < deadline,
                "the loop never took the first sixteen frames"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !seen.load(Ordering::SeqCst),
            "a frame was read with the queue at the high-water mark"
        );
        assert_eq!(
            tx.capacity(),
            31,
            "precondition: exactly the seventeenth frame is still waiting"
        );
        task.abort();
    }

    /// F3: a writer that has died is noticed even while frames keep coming —
    /// here notifications, which never enqueue a response whose send would
    /// fail. While a frame is waiting, the biased select takes it and never
    /// polls the writer branch; only the reaping step's look catches the dead
    /// writer. With 100 000 notifications queued up front, the loop must stop
    /// long before it has read them all — without the look it reads every one
    /// and notices only when the channel runs dry. (Through `serve`, frames are
    /// waiting only while the reader stays ahead on another thread; the first
    /// version of this test, and a second with a refilling producer, both let
    /// the channel run dry and survived their mutant — review of f2b4e2e, L3.)
    #[tokio::test]
    async fn a_dead_writer_is_noticed_while_frames_keep_coming() {
        // The writer runs, and dies, the first time the loop yields — on a
        // current-thread runtime, when tokio's coop budget (128 in 1.53) runs
        // out. Before that the loop takes up to budget × 16/15 frames (the
        // reaping step's `try_recv` spends no budget), then at most 16 more to
        // its next reaping step; "more than half unread" therefore holds for any
        // budget below about 46 000.
        const NOTES: usize = 100_000;
        let (tx, mut rx) = tokio::sync::mpsc::channel(NOTES + 1);
        // One request, so the writer has a line to fail on.
        tx.send(frame_of(&req("x", "t/none", json!({}))))
            .await
            .unwrap();
        let note = serde_json::to_vec(&json!({"jsonrpc": "2.0", "method": "t/none"})).unwrap();
        for _ in 0..NOTES {
            tx.send(Ok(note.clone())).await.unwrap();
        }
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            run(&mut rx, FailingWriter, fixture().methods, TEST_LIMITS),
        )
        .await
        .expect("the connection never ended");
        assert!(
            matches!(&ended, Ended::WriteFailed(e) if e.to_string() == "write boom"),
            "{ended:?}"
        );
        let unread = rx.len();
        assert!(
            unread > NOTES / 2,
            "the loop read {} notifications after its writer died",
            NOTES - unread
        );
    }

    /// M1 (review of f2b4e2e): a message this module did not clip — a params
    /// check that quotes the bad value — is cut where responses are written, so
    /// a long quote still reaches the peer as `-32602`, not as the frame-limit
    /// `-32603`.
    #[test]
    fn a_long_message_from_a_handler_is_cut_not_turned_into_an_internal_error() {
        let quote = "v".repeat(2 * MAX_FRAME_BYTES);
        let line = response_line(Response::error(
            Some("a".to_owned()),
            RpcError::invalid_params(format!("bad value: {quote}")),
        ));
        let v: Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(code_of(&v), i64::from(code::INVALID_PARAMS), "{v}");
        let message = v["error"]["message"].as_str().unwrap();
        assert!(message.starts_with("bad value: v"), "{message}");
        assert!(
            message.len() <= MAX_MESSAGE_BYTES,
            "{} bytes",
            message.len()
        );
    }

    /// `serve_until` ends with `Stopped` when its stop fires — and, as on every
    /// other end, the handler in flight has been dropped by then.
    #[tokio::test]
    async fn serve_until_stops_when_told_and_drops_what_was_running() {
        let f = fixture();
        let hang = f.hang.clone();
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (sr, sw) = tokio::io::split(server);
        let (_cr, mut cw) = tokio::io::split(client);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(serve_until(
            BufReader::new(sr),
            sw,
            f.methods,
            TEST_LIMITS,
            async {
                let _ = rx.await;
            },
        ));
        cw.write_all(format!("{}\n", req("h", "t/hang", json!({}))).as_bytes())
            .await
            .unwrap();
        while hang.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        // Control: not stopped yet, still serving.
        assert!(!task.is_finished());
        tx.send(()).unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("did not stop")
            .unwrap();
        assert!(matches!(ended, Ended::Stopped), "{ended:?}");
        assert!(
            hang.dropped.load(Ordering::SeqCst),
            "the handler outlived the stop"
        );
    }

    /// A stop wins over frames that are ready at the same time: with thousands
    /// queued and the stop already fired, the loop reads almost none of them.
    /// (Biased to frames, it would otherwise never look.)
    #[tokio::test]
    async fn a_stop_is_seen_even_while_frames_keep_coming() {
        const NOTES: usize = 10_000;
        let (tx, mut rx) = tokio::sync::mpsc::channel(NOTES);
        let note = serde_json::to_vec(&json!({"jsonrpc": "2.0", "method": "t/none"})).unwrap();
        for _ in 0..NOTES {
            tx.send(Ok(note.clone())).await.unwrap();
        }
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            run_until(
                &mut rx,
                tokio::io::sink(),
                fixture().methods,
                TEST_LIMITS,
                std::future::ready(()),
            ),
        )
        .await
        .expect("never stopped");
        assert!(matches!(ended, Ended::Stopped), "{ended:?}");
        assert!(
            rx.len() > NOTES - 16,
            "read {} frames after the stop",
            NOTES - rx.len()
        );
    }

    /// Fires a stop from inside its params check — i.e. while the loop is
    /// handling this frame, synchronously.
    struct TriggerStop {
        stop: Arc<tokio::sync::Notify>,
    }
    impl Handler for TriggerStop {
        fn check_params(&self, _: &Value) -> Result<(), String> {
            self.stop.notify_one();
            Ok(())
        }
        fn call(&self, _: Value) -> CallFuture {
            Box::pin(async move { Ok(Value::Null) })
        }
    }

    /// A stop that becomes ready while frame 16 is handled is honoured before
    /// frame 17 is dispatched — even though frame 17 is taken by the reaping
    /// step, which does not go through the select. The probe records whether
    /// frame 17's params check ever ran (review of ME3-SUP slice 2, round 1).
    #[tokio::test]
    async fn a_stop_during_the_sixteenth_frame_is_seen_before_the_seventeenth() {
        let stop = Arc::new(tokio::sync::Notify::new());
        let seen = Arc::new(AtomicBool::new(false));
        let methods = fixture()
            .methods
            .with("t/trigger", Arc::new(TriggerStop { stop: stop.clone() }))
            .with("t/probe", Arc::new(Probe { seen: seen.clone() }));
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        for i in 0..15 {
            tx.send(frame_of(&req(&format!("{i:02}"), "t/none", json!({}))))
                .await
                .unwrap();
        }
        tx.send(frame_of(&req("15", "t/trigger", json!({}))))
            .await
            .unwrap();
        tx.send(frame_of(&req("16", "t/probe", json!({}))))
            .await
            .unwrap();
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            run_until(
                &mut rx,
                tokio::io::sink(),
                methods,
                TEST_LIMITS,
                async move {
                    stop.notified().await;
                },
            ),
        )
        .await
        .expect("never stopped");
        assert!(matches!(ended, Ended::Stopped), "{ended:?}");
        assert!(
            !seen.load(Ordering::SeqCst),
            "frame 17 was dispatched after the stop"
        );
        assert_eq!(rx.len(), 1, "precondition: frame 17 was there to be taken");
    }
}
