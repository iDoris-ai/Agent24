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
/// **SPEC gives no number** (§3: "并发上限见 §5"; §5 has none). 64 matches the
/// proxy's per-module ceiling (`proxy::MAX_INFLIGHT_PER_MODULE`) so that a module
/// cannot hold more callbacks open than requests it is serving at full load. It
/// is a choice, recorded as one in SPEC's ME-3c table, not a measurement.
pub const MAX_IN_FLIGHT_PER_CONNECTION: usize = 64;

/// How long the kernel works on one callback before answering `timeout`. Not
/// retried: a callback may have side effects (SPEC §3). Same caveat as above —
/// SPEC gives no number; 30s matches the proxy's total deadline, so a callback
/// made on behalf of a proxied request cannot outlive that request by design.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

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
            format!("method not found: this daemon does not provide `{method}`"),
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
/// `in_flight` is the set of ids currently running on this connection.
///
/// **A reused in-flight id is answered with `id: null`**, carrying the id in
/// `error.data.duplicate_id`. SPEC §3 says the second request fails; it does not
/// say how to answer it, and the obvious answer (echo the id) makes the error
/// indistinguishable from a response to the FIRST request — the caller would
/// pair it with the call that is still running. `null` is JSON-RPC's own answer
/// for "the id of this request cannot be used", and the original request still
/// gets its own response later.
#[must_use]
pub fn dispatch(frame: &[u8], methods: &Methods, in_flight: &HashSet<String>) -> Dispatch {
    // Duplicate keys first, over the RAW bytes: parsing into a map keeps the last
    // value and forgets that there were two — "last one wins" lets a sender show
    // one value to a logger and another to a checker (SPEC §8 ME-3c).
    match find_duplicate_key(frame) {
        Err(e) => return Dispatch::Respond(Response::error(None, RpcError::parse_error(e))),
        Ok(Some(path)) => {
            let in_params = path.first().is_some_and(|p| p == "params");
            let where_ = path.join(".");
            if in_params {
                // The envelope itself is sound, so the id can be trusted.
                let id = serde_json::from_slice::<Value>(frame)
                    .ok()
                    .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_owned));
                if id.is_none() {
                    return Dispatch::Ignore; // a notification: never answered
                }
                return Dispatch::Respond(Response::error(
                    id,
                    RpcError::invalid_params(format!("duplicate key `{where_}` in params")),
                ));
            }
            return Dispatch::Respond(Response::error(
                None,
                RpcError::invalid_request(format!("duplicate key `{where_}` in the request")),
            ));
        }
        Ok(None) => {}
    }
    let value: Value = match serde_json::from_slice(frame) {
        Ok(v) => v,
        Err(e) => {
            return Dispatch::Respond(Response::error(None, RpcError::parse_error(e.to_string())));
        }
    };
    let Value::Object(obj) = value else {
        let what = if value.is_array() {
            "batch requests are not supported"
        } else {
            "a request must be a JSON object"
        };
        return Dispatch::Respond(Response::error(None, RpcError::invalid_request(what)));
    };

    // The id, if it can be determined. SPEC §3: ids are strings.
    let id = match obj.get("id") {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Dispatch::Respond(Response::error(
                None,
                RpcError::invalid_request("the id must be a string"),
            ));
        }
    };
    let fail = |e: RpcError| match &id {
        Some(id) => Dispatch::Respond(Response::error(Some(id.clone()), e)),
        None => Dispatch::Ignore,
    };

    if let Some(extra) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "jsonrpc" | "id" | "method" | "params"))
    {
        return fail(RpcError::invalid_request(format!(
            "unknown member `{extra}`"
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
        if method == CANCEL_METHOD {
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
    if in_flight.contains(&id) {
        return Dispatch::Respond(Response::error(
            None,
            RpcError::invalid_request("this id is already in flight on this connection")
                .with_data("duplicate_id", Value::String(id)),
        ));
    }
    let Some(handler) = methods.get(method) else {
        return fail(RpcError::method_not_found(method));
    };
    if let Err(why) = handler.check_params(&params) {
        return fail(RpcError::invalid_params(why));
    }
    Dispatch::Call {
        id,
        params,
        handler,
    }
}

fn params_only(params: &Value, allowed: &[&str]) -> bool {
    params
        .as_object()
        .is_some_and(|m| m.keys().all(|k| allowed.contains(&k.as_str())))
}

/// `Err` = not JSON at all. `Ok(Some(path))` = the first repeated key, as a path
/// of object keys from the top (array positions are not recorded).
fn find_duplicate_key(bytes: &[u8]) -> Result<Option<Vec<String>>, String> {
    let mut found: Option<Vec<String>> = None;
    let mut path = Vec::new();
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let r = NoDup {
        path: &mut path,
        found: &mut found,
    }
    .deserialize(&mut de)
    .and_then(|()| de.end());
    match (r, found) {
        (_, Some(p)) => Ok(Some(p)),
        (Ok(()), None) => Ok(None),
        (Err(e), None) => Err(e.to_string()),
    }
}

struct NoDup<'a> {
    path: &'a mut Vec<String>,
    found: &'a mut Option<Vec<String>>,
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
                let mut at = path.clone();
                at.push(key);
                *found = Some(at);
                return Err(de::Error::custom("duplicate key"));
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
/// so the `busy` and `timeout` branches are reachable in milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_in_flight: usize,
    pub call_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_in_flight: MAX_IN_FLIGHT_PER_CONNECTION,
            call_timeout: CALL_TIMEOUT,
        }
    }
}

/// Why [`serve`] returned. Every in-flight call has been dropped by then, and
/// none of them was answered — the connection they would be answered on is gone.
#[derive(Debug)]
pub enum Ended {
    /// The module closed the connection.
    PeerClosed,
    /// A line exceeded [`MAX_FRAME_BYTES`]: the one post-handshake failure that
    /// disconnects (the stream is mid-line and cannot be resynchronised).
    TooLong,
    /// Reading failed.
    ReadFailed(std::io::Error),
    /// Writing a response failed.
    WriteFailed(std::io::Error),
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

/// Aborts a task when dropped — so a call whose connection ended, or whose
/// outer task was aborted, does not keep running detached.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn run_call(
    handler: Arc<dyn Handler>,
    params: Value,
    cancel: tokio::sync::oneshot::Receiver<()>,
    timeout: Duration,
) -> Result<Value, RpcError> {
    // On its own task so a panicking handler becomes an error response instead
    // of a call that never answers (and an id that stays "in flight" forever).
    let mut inner = AbortOnDrop(tokio::spawn(async move {
        tokio::time::timeout(timeout, handler.call(params)).await
    }));
    tokio::select! {
        r = &mut inner.0 => match r {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_elapsed)) => Err(RpcError::application(
                ErrorKind::Timeout,
                format!("the kernel gave up after {}ms; the call is not retried", timeout.as_millis()),
            )),
            Err(_) => Err(RpcError::internal("the handler failed without answering")),
        },
        Ok(()) = cancel => Err(RpcError::application(
            ErrorKind::Cancelled,
            "cancelled by $/cancelRequest; any side effect already committed stays",
        )),
    }
}

/// Run a connection after its handshake: read frames, run calls concurrently,
/// write each response when its call finishes (so responses may be out of
/// order), until the peer closes, a line is too long, or I/O fails.
///
/// When it returns, every in-flight call has been dropped **without a response**
/// (SPEC §3: the connection they would be answered on is gone).
pub async fn serve<R, W>(reader: R, mut writer: W, methods: Methods, limits: Limits) -> Ended
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    // Frames arrive from a dedicated reader task: `read_frame_async` is not
    // cancel-safe, and racing it against "a call finished" would drop a
    // half-read frame and desynchronise the stream.
    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, FrameError>>(1);
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

    let (done_tx, mut done_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Result<Value, RpcError>)>();
    // id → the cancel trigger (taken when fired). Removed when the call answers.
    let mut in_flight: HashMap<String, Option<tokio::sync::oneshot::Sender<()>>> = HashMap::new();
    let mut calls = tokio::task::JoinSet::new();

    let ended = loop {
        let line = tokio::select! {
            frame = frames_rx.recv() => match frame {
                Some(Ok(bytes)) => {
                    let ids: HashSet<String> = in_flight.keys().cloned().collect();
                    match dispatch(&bytes, &methods, &ids) {
                        Dispatch::Respond(r) => Some(r.to_line()),
                        Dispatch::Ignore => None,
                        Dispatch::Cancel { id } => {
                            if let Some(trigger) = in_flight.get_mut(&id).and_then(Option::take) {
                                let _ = trigger.send(());
                            }
                            None
                        }
                        Dispatch::Call { id, params, handler } => {
                            if in_flight.len() >= limits.max_in_flight {
                                Some(Response::error(Some(id), RpcError::application(
                                    ErrorKind::Busy,
                                    format!("{} calls are already in flight on this connection", limits.max_in_flight),
                                )).to_line())
                            } else {
                                let (trigger, cancel) = tokio::sync::oneshot::channel();
                                in_flight.insert(id.clone(), Some(trigger));
                                let done = done_tx.clone();
                                let timeout = limits.call_timeout;
                                calls.spawn(async move {
                                    let outcome = run_call(handler, params, cancel, timeout).await;
                                    let _ = done.send((id, outcome));
                                });
                                None
                            }
                        }
                    }
                }
                Some(Err(FrameError::TooLong { .. })) => break Ended::TooLong,
                Some(Err(FrameError::Eof)) | None => break Ended::PeerClosed,
                Some(Err(FrameError::Io(e))) => break Ended::ReadFailed(e),
            },
            Some((id, outcome)) = done_rx.recv() => {
                in_flight.remove(&id);
                Some(Response { id: Some(id), outcome }.to_line())
            }
            // Reap finished call tasks so the set does not grow without bound.
            Some(_) = calls.join_next(), if !calls.is_empty() => None,
        };
        if let Some(line) = line
            && let Err(e) = async {
                writer.write_all(&line).await?;
                writer.flush().await
            }
            .await
        {
            break Ended::WriteFailed(e);
        }
    };
    // Dropping the set aborts every call task, and each aborts its handler task
    // (AbortOnDrop) — no call outlives its connection, and none is answered.
    calls.abort_all();
    ended
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
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line:?}"))
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
        tokio::time::sleep(Duration::from_millis(50)).await;
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
        tokio::time::sleep(Duration::from_millis(50)).await;
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
                max_in_flight: 8,
                call_timeout: Duration::from_millis(100),
            },
        );
        c.send(req("h", "t/hang", json!({}))).await;
        let r = c.recv().await;
        assert_eq!(r["id"], "h");
        assert_eq!(kind(&r), Some("timeout"));
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
                call_timeout: Duration::from_secs(10),
            },
        );
        c.send(req("h", "t/hang", json!({}))).await;
        c.send(req("b", "t/echo", json!({}))).await;
        let r = c.recv().await;
        assert_eq!(r["id"], "b");
        assert_eq!(kind(&r), Some("busy"));
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
}
