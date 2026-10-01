//! A3 (`docs/design/A3-ATTACHED-MODULE.md` §6.2, §6.3) — `serve_attached`: run
//! [`crate::rpc::serve_until`] **unmodified** on an attached connection, while
//! also letting the KERNEL originate calls on the same connection (`speak`,
//! `stop_playback` — §6).
//!
//! # Why a wrapper, and not a change to `rpc::serve_until`
//!
//! `serve_until` has been through several rounds of review and is frozen
//! (design doc §6.2: "已被多轮评审固化，不改"). It already does everything a
//! MODULE-originated call needs: read a frame, dispatch it, write the
//! response. What it does not do — because A1 never needed it — is originate
//! a request of its OWN and wait for a reply on the same wire. So this module
//! sits **outside** `serve_until`, splitting the connection into two virtual
//! pipes:
//!
//! - Frames that are ordinary JSON-RPC requests/notifications (anything with
//!   a `method`, or anything that is not even a JSON object — so `serve_until`
//!   still produces its usual `-32700`/`-32600`) are forwarded, byte for
//!   byte, into an in-memory duplex pipe that `serve_until` reads as if it
//!   were the real connection.
//! - Frames that look like a RESPONSE (a JSON object, no `method`) are
//!   intercepted here: if their `id` matches a call this side is waiting on,
//!   that call is resolved; otherwise the frame is dropped and counted
//!   ([`KernelCalls::stray_responses`]) — **never** hand it to `serve_until`,
//!   which would answer it with its own `-32600` under the KERNEL's id space,
//!   not the module's (§6.2, review H3: this is exactly the bug v1 had).
//!
//! `serve_until`'s own output, and this side's kernel-originated request
//! lines, are merged (in the order each becomes ready) onto the one real
//! writer — the module sees a single ordinary NDJSON stream either way.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};

use crate::frame::{FrameError, MAX_FRAME_BYTES};
use crate::rpc::{Ended, ErrorKind, Limits, Methods, RpcError, read_frame_async, serve_until};

/// The one method a kernel-originated call uses today (§6.1).
pub const COMMAND_METHOD: &str = "_a24/command/invoke";
/// How long the kernel waits for an answer to `_a24/command/invoke` (§6.1)
/// before giving up (the REST caller then sees `504`; the module's late
/// answer, if any, is dropped per [`KernelCalls::stray_responses`]).
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// §6.1: past this many kernel-originated calls in flight on one connection,
/// a new one is refused `429 busy` WITHOUT a frame ever being sent — enforced
/// by the caller (the REST handler, A3-3), not by this module; declared here
/// because it is a property of this wire, same as [`COMMAND_METHOD`].
pub const MAX_KERNEL_CALLS_IN_FLIGHT: usize = 8;

/// Why a kernel→module call produced no usable result.
#[derive(Debug)]
pub enum KernelCallFailed {
    /// The connection was already gone (or this generation already revoked)
    /// at call time — nothing was ever written.
    NotSent,
    /// The request WAS written, but the connection ended before an answer
    /// came back. The outcome is genuinely unknown — the module may or may
    /// not have acted on it (§6.1: REST maps this to `502 connection_lost`).
    ConnectionLost,
    /// No answer within the caller's `timeout`. A late answer arriving after
    /// this is dropped and counted, never delivered (§4.2, §6.1).
    Timeout,
    /// The module answered with a well-formed JSON-RPC error.
    Rpc(RpcError),
    /// The module answered, but not usably: `result` and `error` both
    /// present, both absent, `result` not an object, or `error` missing a
    /// valid `code`/`message` (§6.1 M5's REST mapping table).
    Malformed(String),
}

impl std::fmt::Display for KernelCallFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSent => f.write_str("the request was never sent"),
            Self::ConnectionLost => f.write_str("the connection ended before an answer arrived"),
            Self::Timeout => f.write_str("no answer within the deadline"),
            Self::Rpc(e) => write!(f, "the module answered with an error: {e:?}"),
            Self::Malformed(why) => write!(f, "malformed response: {why}"),
        }
    }
}

impl std::error::Error for KernelCallFailed {}

/// `pending` plus the "has this connection ended" flag, behind ONE lock
/// (review M1). Before this both lived separately (`closed` an `AtomicBool`,
/// `pending` its own `Mutex`), which let [`KernelCalls::call`]'s "check
/// closed, then insert" and the finaliser's "set closed, then clear" race:
/// `call()` could observe `closed == false`, then the finaliser could run
/// its whole close-and-clear between that load and the insert, and `call()`
/// would then insert a pending entry into a map nobody will ever clear again
/// — the request already written (or about to be), its answer never
/// resolvable, hanging until its own `timeout`. Sharing one lock makes the
/// two operations atomic with respect to each other: whichever runs first
/// under the lock is fully visible to the other.
struct PendingState {
    map: HashMap<String, oneshot::Sender<Map<String, Value>>>,
    /// Set once the connection has ended. From here on [`KernelCalls::call`]
    /// fails fast with `NotSent` rather than inserting into `map` — and
    /// nothing inserts into `map` again, since only `call()`, under this
    /// same lock, ever does.
    closed: bool,
}

/// State shared between every clone of a [`KernelCalls`] handle and the mux
/// loop that resolves or drops incoming frames.
struct Shared {
    pending: Mutex<PendingState>,
    next_id: AtomicU64,
    stray_responses: AtomicU64,
}

impl Shared {
    /// Ends the connection from `pending`'s point of view: no further call
    /// may be issued, and every call already waiting is dropped (waking it
    /// with `ConnectionLost` rather than leaving it to its own timeout).
    /// Idempotent — safe to call from both the ordinary end-of-connection
    /// path and a guard's `Drop` (review M2), whichever runs first does the
    /// work and the other finds nothing left to do.
    fn close(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending.closed = true;
        pending.map.clear();
    }
}

/// Handle for kernel-originated requests on an attached connection. Cheap to
/// clone (an `Arc` and an `mpsc::Sender` inside).
#[derive(Clone)]
pub struct KernelCalls {
    shared: Arc<Shared>,
    /// Raw NDJSON lines (newline included), merged with `serve_until`'s own
    /// output onto the real connection by the single writer task `serve_attached`
    /// spawns. Bounded (review H1): a kernel call that finds it full does not
    /// wait — it fails fast with [`KernelCallFailed::NotSent`] via
    /// [`mpsc::Sender::try_send`], the same way a closed channel already did.
    /// [`serve_until`]'s own relayed output, in contrast, is pushed with
    /// `.send().await` (see `serve_attached`) so that when the real module
    /// stops reading, the resulting backpressure travels all the way back
    /// into the pipe `serve_until` writes to — reviving ITS OWN
    /// `queue_high_water`/`write_timeout` handling rather than buffering
    /// unboundedly here instead.
    out_tx: mpsc::Sender<Vec<u8>>,
}

/// Removes this call's pending entry when dropped — the one piece of cleanup
/// code for every way [`KernelCalls::call`] can stop waiting for an answer:
/// an ordinary return (success, timeout, send failure), OR the `call()`
/// future itself being dropped/cancelled by its caller mid-await (review M2,
/// "顺手做"). Removing an already-absent id (the common case: a matching
/// response already removed it in [`route_response`]) is a harmless no-op,
/// so this never needs to know which case it is.
struct PendingGuard {
    shared: Arc<Shared>,
    id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let mut pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pending.map.remove(&self.id);
    }
}

impl KernelCalls {
    /// Issue one kernel-originated request and wait for its answer, `params`
    /// verbatim (§6.1: the kernel does not interpret `body`'s business shape).
    ///
    /// # Errors
    ///
    /// See [`KernelCallFailed`].
    pub async fn call(
        &self,
        method: &'static str,
        params: Value,
        timeout: Duration,
    ) -> Result<Map<String, Value>, KernelCallFailed> {
        let id = format!("k{}", self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        let mut line = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .unwrap_or_default();
        line.push(b'\n');
        {
            // review M1 (original) / A3-3 review M2 (widened): the "is this
            // connection still open" check, the (non-blocking) enqueue onto
            // the real writer, AND the pending-map insert now ALL happen
            // under this ONE lock acquisition — the same lock `close()`
            // takes to flip `closed` and clear `map`. That makes "decide
            // this connection is closed" and "enqueue a new frame for it"
            // mutually exclusive: whichever gets the lock first is fully
            // visible to the other.
            //
            // M2's own gap: `close()` can be called from OUTSIDE this
            // connection's wire teardown — `agent24d::attach_registry` calls
            // it under ITS OWN lock the instant it decides a generation is
            // no longer live (rotation, disable, `release`, `revoke_all`),
            // which can run well before `serve_attached`'s finalizer would
            // ever notice. Previously `try_send` ran OUTSIDE any lock, so a
            // `call()` that had already passed the (then separate)
            // closed-check could still successfully queue a frame onto
            // `out_tx` AFTER the registry had already decided this
            // connection was gone and told it so via `close()` — the queued
            // frame would reach a module the registry no longer considers
            // reachable. Folding `try_send` into the SAME critical section
            // `close()` uses closes that window: if `close()` already ran,
            // `pending.closed` is observed `true` and `try_send` is never
            // even attempted; if this call's critical section runs first,
            // it completes atomically (check, send, insert) before `close()`
            // can start.
            let mut pending = self
                .shared
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if pending.closed {
                return Err(KernelCallFailed::NotSent);
            }
            // `try_send`, not `.send().await` (review H1): non-blocking, so
            // safe to call while holding this synchronous `Mutex` — a
            // kernel-originated call must not block waiting for queue space
            // either way. A full queue means the real module is not keeping
            // up, and the caller is entitled to `NotSent` promptly rather
            // than discovering that only after its own `timeout` elapses.
            if self.out_tx.try_send(line).is_err() {
                return Err(KernelCallFailed::NotSent);
            }
            pending.map.insert(id.clone(), tx);
        }
        let _guard = PendingGuard {
            shared: Arc::clone(&self.shared),
            id: id.clone(),
        };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(obj)) => interpret_kernel_response(obj),
            // The sender was dropped without ever sending — happens when the
            // connection ends on its own (`serve_attached`'s finalizer
            // drains and drops every pending sender) OR when `close()` is
            // called externally while this call's answer was still pending
            // (`agent24d::attach_registry`, review M2) — either way the
            // frame WAS sent and the module's fate for it is unknown.
            Ok(Err(_recv_error)) => Err(KernelCallFailed::ConnectionLost),
            Err(_elapsed) => Err(KernelCallFailed::Timeout),
        }
    }

    /// A3-3 review M2: externally mark this connection "closed" for the
    /// purpose of `call()` — sets the SAME `pending.closed` flag (and clears
    /// `pending.map`, dropping every already-pending call's sender so its
    /// `call()` wakes with [`KernelCallFailed::ConnectionLost`]) that the
    /// wire's own EOF/write-failure path already used internally
    /// ([`Shared::close`]). Exposed so `agent24d::attach_registry` can call
    /// it the INSTANT it decides (under its own lock) that a generation is
    /// no longer live — rotation, disable, `release`, `revoke_all` — rather
    /// than waiting for the wire itself to notice, which can lag behind that
    /// decision by one or more async ticks (see `call()`'s own doc for the
    /// race this closes). Idempotent, and safe to call even if the
    /// connection has already ended on its own.
    pub fn close(&self) {
        self.shared.close();
    }

    /// C1 (pre-release): enqueue an already-framed, newline-terminated line
    /// (NOT a `call()` — no `id` is registered in `pending`, so no response
    /// is ever awaited for it) onto the SAME outbound queue `call()` and
    /// `serve_until`'s relayed answers share, via the same non-blocking
    /// `try_send` `call()` uses (review H1's reasoning applies identically:
    /// a kernel-originated write must not block on queue space).
    ///
    /// This exists for exactly one caller,
    /// `agent24d::attach_listener::handle_connection`, and one line: the
    /// handshake success frame. The bug it closes: writing that frame
    /// directly to the raw connection BEFORE calling
    /// `agent24d::attach_registry::AttachRegistry::attach_kernel_calls`
    /// leaves a gap — the module has its handshake result and may act on it
    /// (or a client observing attach state elsewhere may race ahead) while
    /// the registry still has no `KernelCalls` installed for this
    /// generation, so `POST /api/v1/os/{name}/commands/*` in that gap gets a
    /// spurious `503 module_not_ready`. Calling this BEFORE
    /// `attach_kernel_calls` closes it the other way around: this handle is
    /// not reachable from anywhere else until `attach_kernel_calls` installs
    /// it, so nothing could have enqueued a command frame ahead of `line`
    /// here — `line` is guaranteed to be message #1 on this queue — and by
    /// the time `attach_kernel_calls` makes this handle reachable, the
    /// success frame is already ahead of it in program order, not behind.
    ///
    /// # Errors
    ///
    /// `Err(())` if the outbound queue is already gone or full — treat
    /// exactly like the old direct write timing out: the caller must not
    /// admit this generation. A dedicated error type would carry no more
    /// information than that (the queue's own `mpsc::error::TrySendError`
    /// is `Full`-or-`Closed`, and this caller's one response to either is
    /// identical), so `()` is kept rather than introduced for its own sake.
    #[allow(clippy::result_unit_err)]
    pub fn enqueue_raw(&self, line: Vec<u8>) -> Result<(), ()> {
        self.out_tx.try_send(line).map_err(|_| ())
    }

    /// Frames without `method` that matched no pending call: dropped, never
    /// answered, never handed to `serve_until` (§6.2, review H3).
    #[must_use]
    pub fn stray_responses(&self) -> u64 {
        self.shared.stray_responses.load(Ordering::Relaxed)
    }
}

/// `error`/`result` shape rules from §6.1's REST mapping table, applied to
/// the raw response object (minus `jsonrpc`/`id`, which the classifier never
/// strips — reading straight past them here is simpler than stripping twice).
fn interpret_kernel_response(
    mut obj: Map<String, Value>,
) -> Result<Map<String, Value>, KernelCallFailed> {
    let result = obj.remove("result");
    let error = obj.remove("error");
    match (result, error) {
        (Some(Value::Object(result)), None) => Ok(result),
        (Some(_not_object), None) => Err(KernelCallFailed::Malformed(
            "result is not an object".to_owned(),
        )),
        (None, Some(Value::Object(mut err))) => {
            let code = err.remove("code").and_then(|c| c.as_i64());
            let message = err
                .remove("message")
                .and_then(|m| m.as_str().map(str::to_owned));
            match (code, message) {
                (Some(code), Some(message)) => {
                    let mut data = match err.remove("data") {
                        Some(Value::Object(d)) => Some(d),
                        _ => None,
                    };
                    let kind = data
                        .as_mut()
                        .and_then(|d| d.remove("kind"))
                        .and_then(|k| k.as_str().and_then(parse_error_kind));
                    Err(KernelCallFailed::Rpc(RpcError {
                        code: i32::try_from(code).unwrap_or(i32::MAX),
                        message,
                        kind,
                        data,
                    }))
                }
                _ => Err(KernelCallFailed::Malformed("malformed response".to_owned())),
            }
        }
        (None, None) | (None, Some(_)) | (Some(_), Some(_)) => {
            Err(KernelCallFailed::Malformed("malformed response".to_owned()))
        }
    }
}

fn parse_error_kind(s: &str) -> Option<ErrorKind> {
    ErrorKind::ALL.into_iter().find(|k| k.as_str() == s)
}

/// Aborts a task when dropped — a private copy of `rpc::AbortOnDrop` (that
/// one is not `pub(crate)`, and it is four lines; duplicating it here keeps
/// this module from depending on `rpc`'s internals beyond what it already
/// exports).
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn map_frame_error(e: FrameError) -> Ended {
    match e {
        FrameError::TooLong { .. } => Ended::TooLong,
        FrameError::Eof => Ended::PeerClosed,
        FrameError::Io(e) => Ended::ReadFailed(e),
    }
}

/// One incoming frame, classified: forward it to `serve_until` verbatim, or
/// consume it as a (possibly stray) response.
fn route_frame<'a>(bytes: &'a [u8], shared: &Shared) -> Option<&'a [u8]> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(obj)) if !obj.contains_key("method") => {
            route_response(obj, shared);
            None
        }
        // Has `method`, or is not even a JSON object at all: `serve_until`
        // must see it, so it can answer with its own protocol error for the
        // malformed cases — dropping those here would silently swallow a
        // `-32700`/`-32600` a module is entitled to see.
        _ => Some(bytes),
    }
}

fn route_response(mut obj: Map<String, Value>, shared: &Shared) {
    let id = obj.remove("id").and_then(|v| match v {
        Value::String(s) => Some(s),
        _ => None,
    });
    let Some(id) = id else {
        shared.stray_responses.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let sender = {
        let mut pending = shared
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pending.map.remove(&id)
    };
    match sender {
        // `obj` still has `id` removed above but keeps `jsonrpc`/`result`/
        // `error` — `interpret_kernel_response` only reads the latter two.
        Some(tx) => {
            let _ = tx.send(obj);
        }
        None => {
            shared.stray_responses.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Bound on [`KernelCalls::out_tx`] (review H1). Once this many NDJSON lines
/// are queued for the real writer and it has not caught up, two things
/// happen: a kernel-originated [`KernelCalls::call`] finds the queue full and
/// fails fast with `NotSent` (`try_send`, never waiting for space), and the
/// relay task copying `serve_until`'s own output (which DOES wait for space —
/// see where it is used) stops draining `serve_until`'s output pipe, so the
/// backpressure travels all the way back into `serve_until`, reviving ITS OWN
/// `queue_high_water`/`write_timeout` handling instead of this side buffering
/// without bound.
const OUT_QUEUE_CAPACITY: usize = 32;

/// [`read_relay_line`] found a line longer than [`MAX_FRAME_BYTES`] without a
/// `\n` — see there for why this ends the relay rather than growing its
/// buffer further.
struct RelayOverflow;

/// Reads one `\n`-terminated line, delimiter included, from `serve_until`'s
/// own output pipe — never fewer than a whole line, and never more than one
/// (review C1). Relaying whole lines, instead of raw 8 KiB chunks as before,
/// is what keeps a `serve_until` response from being split mid-write and a
/// kernel-originated request line from landing in the MIDDLE of one: both
/// are queued as complete entries on [`KernelCalls::out_tx`], and the real
/// writer task writes each entry to completion before taking the next, so
/// the two producers can never interleave a write on the wire (previously, a
/// response longer than 8 KiB read in several chunks left a window, between
/// two of those chunks, where a kernel call's own line could be queued and
/// written in between — corrupting the module's NDJSON stream).
///
/// Capped at `MAX_FRAME_BYTES + 1` bytes — [`read_frame_async`]'s own limit.
/// `serve_until` never writes a longer line itself (`response_line` enforces
/// the same limit before writing), so exceeding the cap here means something
/// unexpected is coming out of the nested connection; treated as a protocol
/// error ([`RelayOverflow`]) rather than buffered without bound.
///
/// Returns `Ok(None)` at a clean EOF with no partial line left over.
async fn read_relay_line<R: AsyncBufRead + Unpin>(
    src: &mut R,
) -> Result<Option<Vec<u8>>, RelayOverflow> {
    let mut out = Vec::new();
    loop {
        let Some(room) = (MAX_FRAME_BYTES + 1)
            .checked_sub(out.len())
            .filter(|r| *r > 0)
        else {
            return Err(RelayOverflow);
        };
        // A duplex pipe's read half cannot itself fail; an `Err` here would
        // only ever be an artefact of a future `AsyncBufRead` impl swapped
        // in — treated the same as EOF, since there is nothing more usable
        // to read either way.
        let Ok(available) = src.fill_buf().await else {
            return Ok(if out.is_empty() { None } else { Some(out) });
        };
        if available.is_empty() {
            return Ok(if out.is_empty() { None } else { Some(out) });
        }
        let window = &available[..available.len().min(room)];
        match window.iter().position(|&b| b == b'\n') {
            Some(i) => {
                out.extend_from_slice(&window[..=i]);
                src.consume(i + 1);
                return Ok(Some(out));
            }
            None => {
                let taken = window.len();
                out.extend_from_slice(window);
                src.consume(taken);
            }
        }
    }
}

/// Everything torn down when this connection ends — whether [`serve_attached`]'s
/// returned future runs to completion or is dropped/cancelled before it does
/// (review M2). Built once near the top of that future and held for its
/// entire lifetime, so this is the ONE place the teardown happens rather than
/// duplicating it at the future's normal exit AND hoping nothing ever drops
/// it early instead.
struct Teardown {
    shared: Arc<Shared>,
    su_handle: AbortOnDrop<Ended>,
    _writer_handle: AbortOnDrop<()>,
    _stop_task: AbortOnDrop<()>,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        // No more kernel calls may be issued or answered from here on, and
        // every one already waiting is dropped so it resolves as
        // `ConnectionLost` rather than hanging until its own timeout —
        // `Shared::close` is the single lock-protected operation that does
        // both (review M1), and is idempotent, so it is harmless if the
        // normal end-of-connection path already called it.
        self.shared.close();
        // `su_handle`, `writer_handle` and `_stop_task` each abort in their
        // own `AbortOnDrop::drop` right after this method returns (review
        // H1, H2a, M2) — including when this whole `Teardown` is dropped
        // because `serve_attached`'s future was cancelled before reaching
        // its own normal end, which is exactly the gap this type closes.
        // Aborting `writer_handle` closes the real write half regardless of
        // whether any `KernelCalls` clone — so, `out_tx` clone — is still
        // alive elsewhere: teardown must not depend on every clone having
        // been dropped first (review H1). Discarding whatever `writer_handle`
        // had not yet written mirrors `serve_until`'s own documented choice
        // to discard unwritten responses when ITS connection ends.
    }
}

/// Wraps the UNCHANGED [`serve_until`]. A frame that is not a JSON object, or
/// that has `method`, goes to `serve_until`; an object without `method`
/// resolves the pending kernel call with that string id, or is DROPPED and
/// counted (never handed to `serve_until` — §6.2, review H3).
///
/// `stop` ends BOTH halves: the classifier loop returned here, and the
/// `serve_until` instance running underneath it (`agent24d` passes
/// `generation.revoked()` — an attached generation's revocation is this
/// connection's only stop signal, there being no drain, §5.3).
pub fn serve_attached<R, W, S>(
    reader: R,
    writer: W,
    methods: Methods,
    limits: Limits,
    stop: S,
) -> (KernelCalls, impl Future<Output = Ended> + Send)
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    S: Future<Output = ()> + Send + 'static,
{
    let shared = Arc::new(Shared {
        pending: Mutex::new(PendingState {
            map: HashMap::new(),
            closed: false,
        }),
        next_id: AtomicU64::new(0),
        stray_responses: AtomicU64::new(0),
    });
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(OUT_QUEUE_CAPACITY);
    let calls = KernelCalls {
        shared: Arc::clone(&shared),
        out_tx: out_tx.clone(),
    };
    // Carries the ONE reason either background task (the relay below, or the
    // real writer further down) ended the connection for — a hard failure,
    // as opposed to the ordinary EOF/shutdown paths, which report nothing
    // here at all (review H1, C1: see where each sender is used).
    let (out_fail_tx, mut out_fail_rx) = mpsc::channel::<Ended>(1);

    // One shared stop signal, fanned out to the classifier loop below AND to
    // the nested `serve_until` — `stop` itself is a one-shot, `!Clone`
    // future, so it is awaited exactly once, here, and its firing is
    // rebroadcast through a `watch`.
    let (stop_tx, stop_rx) = watch::channel(false);
    let stop_task = tokio::spawn(async move {
        stop.await;
        let _ = stop_tx.send(true);
    });

    // Pipe #1: classified frames destined for `serve_until`, as if it were
    // reading the real connection directly.
    const PIPE_CAPACITY: usize = MAX_FRAME_BYTES + 4096;
    let (to_su_write, to_su_read) = tokio::io::duplex(PIPE_CAPACITY);
    // Pipe #2: `serve_until`'s own output, relayed onward below.
    let (su_writer, su_read) = tokio::io::duplex(PIPE_CAPACITY);

    let su_stop = watch_true(stop_rx.clone());
    let su_handle = tokio::spawn(serve_until(
        tokio::io::BufReader::new(to_su_read),
        su_writer,
        methods,
        limits,
        su_stop,
    ));

    // Relay `serve_until`'s own output onto the shared output channel, one
    // whole NDJSON line at a time (review C1 — see `read_relay_line`).
    // `.send(..).await`, not `try_send` (review H1): when the real writer
    // task falls behind — the module stopped reading — this wait is exactly
    // what stops draining `su_read`, so the backpressure reaches all the way
    // back to `serve_until`'s own writer. Ends when `serve_until`'s writer
    // half is dropped (task finished) and its buffered bytes are drained —
    // an ordinary EOF, not an error — or when a line comes out longer than
    // this side ever expects (`RelayOverflow`), which is reported upward as
    // `Ended::TooLong` rather than silently dropped.
    let relay_out_tx = out_tx.clone();
    let relay_fail_tx = out_fail_tx.clone();
    let mut su_read = tokio::io::BufReader::new(su_read);
    tokio::spawn(async move {
        loop {
            match read_relay_line(&mut su_read).await {
                Ok(None) => return,
                Ok(Some(line)) => {
                    if relay_out_tx.send(line).await.is_err() {
                        return;
                    }
                }
                Err(RelayOverflow) => {
                    let _ = relay_fail_tx.try_send(Ended::TooLong);
                    return;
                }
            }
        }
    });

    // The single writer task: everything destined for the real module —
    // `serve_until`'s answers and this side's kernel-originated requests —
    // is serialised through this one channel and written in the order it
    // arrives, so the two producers above can never interleave a write.
    // Each write is capped at `limits.write_timeout` (review H1): without
    // this, a module that stops reading never surfaces here at all — the
    // write simply hangs — and this loop's caller (`serve_attached`'s
    // returned future) has no way to learn the connection is dead. On
    // failure or timeout the reason is reported through `out_fail_tx` so the
    // main loop can end the connection instead of finding out only when
    // `Teardown` eventually aborts this task anyway.
    let write_timeout = limits.write_timeout;
    let writer_fail_tx = out_fail_tx;
    let writer_handle = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(chunk) = out_rx.recv().await {
            let write = async {
                writer.write_all(&chunk).await?;
                writer.flush().await
            };
            match tokio::time::timeout(write_timeout, write).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = writer_fail_tx.try_send(Ended::WriteFailed(e));
                    return;
                }
                Err(_elapsed) => {
                    let _ = writer_fail_tx.try_send(Ended::WriteFailed(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "a write to the attached module took longer than {}ms",
                            write_timeout.as_millis()
                        ),
                    )));
                    return;
                }
            }
        }
    });

    // Review (Codex A3 follow-up): built HERE, synchronously, right after
    // the three spawns above and BEFORE the `async move` block below is even
    // constructed — not as the first statement INSIDE that block, which is
    // where it used to live. An `async move { ... }` block's captures are
    // moved in the instant the block VALUE is created (this line, still
    // inside `serve_attached`'s own synchronous body), regardless of whether
    // the resulting future is ever polled — but a STATEMENT inside that
    // block only runs once the future is actually polled for the first
    // time. The old code built `Teardown` (and so wrapped `su_handle`/
    // `writer_handle`/`stop_task` in `AbortOnDrop`) as such a statement, so a
    // caller that dropped the returned future before ever polling it (e.g.
    // a `select!` branch that lost a race before this arm was reached) never
    // ran that statement at all — the THREE JoinHandles were still captured
    // into the future's environment, but as plain, un-wrapped `JoinHandle`s,
    // and dropping a `JoinHandle` WITHOUT calling `.abort()` first just
    // detaches it — the task keeps running, leaked, forever. Building
    // `Teardown` out here closes the gap: it is moved into the future's
    // environment at construction time either way, so dropping an un-polled
    // future now runs `Teardown`'s `Drop` (and so every `AbortOnDrop`'s)
    // exactly as if the future HAD run to its normal end.
    let mut teardown = Teardown {
        shared: Arc::clone(&shared),
        su_handle: AbortOnDrop(su_handle),
        _writer_handle: AbortOnDrop(writer_handle),
        _stop_task: AbortOnDrop(stop_task),
    };

    let fut = async move {
        // A dedicated reader task, exactly like `serve_until`'s own —
        // `read_frame_async` is documented not cancel-safe, so it must never
        // be raced inside a `select!` that could drop it mid-frame while the
        // same reader keeps being used afterward. Here it is raced only in
        // the sense that `stop` can win — and when it does, this connection
        // is discarded entirely, so losing a half-read frame is harmless.
        let (frames_tx, mut frames_rx) = mpsc::channel::<Result<Vec<u8>, FrameError>>(1);
        let _reader_task = AbortOnDrop(tokio::spawn(async move {
            let mut reader = reader;
            loop {
                let frame = read_frame_async(&mut reader).await;
                let last = frame.is_err();
                if frames_tx.send(frame).await.is_err() || last {
                    return;
                }
            }
        }));

        let mut to_su_write = to_su_write;
        // `watch_true` (not `stop_watch.wait_for(..)` inline) deliberately:
        // `wait_for`'s `Ok` value is a `watch::Ref`, a non-`Send` read guard,
        // and naming it directly as a `select!` branch's output makes the
        // WHOLE select — and so this function's returned future — not `Send`,
        // even though nothing here holds the guard past the match arm.
        // Wrapping it in a plain `async fn` that resolves to `()` keeps the
        // guard entirely inside that function's own stack frame.
        let mut stop_watch = std::pin::pin!(watch_true(stop_rx.clone()));
        let ended = loop {
            tokio::select! {
                biased;
                () = &mut stop_watch => break Ended::Stopped,
                // The real writer (or the relay ahead of it) hit a fatal
                // condition (review H1, C1): reported here rather than only
                // discovered when `Teardown` reaps the task at the very end,
                // which would have left this loop running — and so the
                // connection nominally alive — for no reason. `Some(reason)`
                // deliberately: once every `out_fail_tx` clone drops without
                // ever sending (the ordinary shutdown case), this future
                // resolves to `None` and tokio's `select!` disables the
                // branch for this call rather than treating it as ready —
                // see the tokio docs for the `Some(x) = fut` pattern.
                Some(reason) = out_fail_rx.recv() => break reason,
                frame = frames_rx.recv() => match frame {
                    None => break Ended::PeerClosed,
                    Some(Err(e)) => break map_frame_error(e),
                    Some(Ok(bytes)) => {
                        if let Some(forwarded) = route_frame(&bytes, &shared) {
                            // Raced against `stop` (review H1): once the
                            // real module stops reading, `serve_until`'s own
                            // writer can block writing into `to_su_write`'s
                            // sibling pipe indefinitely (the bounded
                            // `out_tx` and its `.send().await` relay above
                            // are what let that backpressure reach here).
                            // Without this race, a `stop` firing while that
                            // write is stuck would never be seen, hanging
                            // this loop instead of ending the connection. If
                            // `stop` wins, `to_su_write` may hold a
                            // half-written frame — harmless, since the whole
                            // connection, `to_su_write` and the nested
                            // `serve_until` alike, is torn down right after
                            // (`Teardown`).
                            tokio::select! {
                                biased;
                                () = &mut stop_watch => break Ended::Stopped,
                                result = write_frame(&mut to_su_write, forwarded) => {
                                    if let Err(e) = result {
                                        break e;
                                    }
                                }
                            }
                        }
                    }
                },
            }
        };

        // Graceful path: let the nested `serve_until` see EOF and wind down
        // on its own, rather than aborting it outright. `teardown`'s `Drop`
        // (run when `fut` finally drops, right after this) then finds the
        // task already finished, and its abort is a harmless no-op — but
        // still runs unconditionally, so a `serve_until` that somehow never
        // finishes does not keep this future from ever completing its own
        // caller's expectations (review H1, M2).
        drop(to_su_write);
        let _ = (&mut teardown.su_handle.0).await;
        ended
    };
    (calls, fut)
}

async fn write_frame<W2: AsyncWrite + Unpin>(w: &mut W2, bytes: &[u8]) -> Result<(), Ended> {
    w.write_all(bytes).await.map_err(Ended::WriteFailed)?;
    w.write_all(b"\n").await.map_err(Ended::WriteFailed)?;
    Ok(())
}

/// A one-shot `Future` that resolves once `rx`'s value becomes `true` — how
/// `stop`'s single firing is shared between the classifier loop and the
/// nested `serve_until` (which each need their OWN `Future`, `stop` itself
/// being consumed once).
async fn watch_true(mut rx: watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    let _ = rx.wait_for(|v| *v).await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::rpc::{CallFuture, Handler};
    use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream};

    /// An echo handler: answers `{"echo": params}` for method `"echo"`.
    struct Echo;
    impl Handler for Echo {
        fn check_params(&self, _params: &Value) -> Result<(), String> {
            Ok(())
        }
        fn call(&self, params: Value) -> CallFuture {
            Box::pin(async move { Ok(serde_json::json!({ "echo": params })) })
        }
    }

    /// A handler that never returns, so a kernel-originated `call()` (which
    /// races the connection independently) has a live in-flight module call
    /// to exercise alongside it without the two interfering.
    struct Hang;
    impl Handler for Hang {
        fn check_params(&self, _p: &Value) -> Result<(), String> {
            Ok(())
        }
        fn call(&self, _params: Value) -> CallFuture {
            Box::pin(std::future::pending())
        }
    }

    fn methods() -> Methods {
        Methods::none()
            .with("echo", Arc::new(Echo))
            .with("hang", Arc::new(Hang))
    }

    /// A connected pair of in-memory duplex streams, standing in for the
    /// module's end and the kernel's end of the attach socket.
    fn harness(
        methods: Methods,
    ) -> (
        KernelCalls,
        impl Future<Output = Ended> + Send,
        BufReader<tokio::io::ReadHalf<DuplexStream>>,
        tokio::io::WriteHalf<DuplexStream>,
    ) {
        let (kernel_side, module_side) = tokio::io::duplex(64 * 1024);
        let (kernel_read, kernel_write) = tokio::io::split(kernel_side);
        let (calls, fut) = serve_attached(
            BufReader::new(kernel_read),
            kernel_write,
            methods,
            Limits::default(),
            std::future::pending(),
        );
        let (module_read, module_write) = tokio::io::split(module_side);
        // A buffered reader (to read what the kernel wrote) and a plain
        // writer (to send frames as the module) — the two halves a test
        // needs; nothing here needs them recombined into one handle.
        (calls, fut, BufReader::new(module_read), module_write)
    }

    async fn send_line(w: &mut (impl AsyncWrite + Unpin), line: &str) {
        w.write_all(line.as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
    }

    async fn recv_line(r: &mut (impl AsyncBufRead + Unpin)) -> Value {
        let mut line = String::new();
        let n = r.read_line(&mut line).await.unwrap();
        assert!(n > 0, "connection closed with nothing to read");
        serde_json::from_str(line.trim_end()).unwrap()
    }

    /// C6, row 1/2: a frame with `method` — and a frame that is not even a
    /// JSON object — both reach `serve_until` unchanged. This is the "A1
    /// behaviour is untouched" half of C6.
    #[tokio::test]
    async fn frames_with_method_and_non_object_frames_reach_serve_until() {
        let (_calls, fut, mut kernel_read, mut module_write) = harness(methods());
        tokio::spawn(fut);

        send_line(
            &mut module_write,
            r#"{"jsonrpc":"2.0","id":"1","method":"echo","params":{"x":1}}"#,
        )
        .await;
        let reply = recv_line(&mut kernel_read).await;
        assert_eq!(reply["id"], "1");
        assert_eq!(reply["result"]["echo"]["x"], 1);

        // Not a JSON object at all (a bare array): `serve_until` answers its
        // usual `-32600`, proving this frame reached it rather than being
        // silently dropped by the classifier.
        send_line(&mut module_write, r#"[1,2,3]"#).await;
        let reply = recv_line(&mut kernel_read).await;
        assert_eq!(reply["error"]["code"], -32600);
    }

    /// C6, row 3: an object with no `method` and an `id` that matches a
    /// pending kernel-originated call is routed to that call, not to
    /// `serve_until`.
    #[tokio::test]
    async fn a_matching_response_resolves_the_pending_kernel_call() {
        let (calls, fut, mut kernel_read, mut module_write) = harness(methods());
        tokio::spawn(fut);

        let call = tokio::spawn(async move {
            calls
                .call(
                    "speak",
                    serde_json::json!({"text": "hi"}),
                    Duration::from_secs(5),
                )
                .await
        });

        // The kernel's own request must appear on the wire, addressed to the
        // module — same channel the module already reads `echo` on.
        let request = recv_line(&mut kernel_read).await;
        assert_eq!(request["method"], "speak");
        assert_eq!(request["params"]["text"], "hi");
        let id = request["id"].as_str().unwrap().to_owned();

        send_line(
            &mut module_write,
            &format!(r#"{{"jsonrpc":"2.0","id":"{id}","result":{{"accepted":true}}}}"#),
        )
        .await;

        let result = call.await.unwrap().expect("the module's answer");
        assert_eq!(result["accepted"], true);
    }

    /// C6, row 4 — the important negative property (review H3): an unknown
    /// id, a non-string id, and a LATE id (one that already timed out) are
    /// all dropped, counted, and produce **zero** new frames back to the
    /// module — never `serve_until`'s own `-32600` under the kernel's id
    /// space, which is exactly the bug v1 had.
    #[tokio::test]
    async fn unmatched_responses_are_dropped_and_counted_not_forwarded() {
        let (calls, fut, mut kernel_read, mut module_write) = harness(methods());
        let conn = tokio::spawn(fut);

        // 1. Unknown id: nothing was ever waiting on "ghost".
        send_line(
            &mut module_write,
            r#"{"jsonrpc":"2.0","id":"ghost","result":{}}"#,
        )
        .await;

        // 2. Non-string id.
        send_line(&mut module_write, r#"{"jsonrpc":"2.0","id":7,"result":{}}"#).await;

        // 3. A late response: issue a real call with a tiny timeout, let it
        // time out, THEN answer it — the id existed once but is gone from
        // the pending map by the time this arrives.
        let late_call = calls
            .call("speak", serde_json::json!({}), Duration::from_millis(20))
            .await;
        assert!(matches!(late_call, Err(KernelCallFailed::Timeout)));
        let request = recv_line(&mut kernel_read).await;
        let late_id = request["id"].as_str().unwrap().to_owned();
        send_line(
            &mut module_write,
            &format!(r#"{{"jsonrpc":"2.0","id":"{late_id}","result":{{"late":true}}}}"#),
        )
        .await;

        // Positive control, issued last on the SAME connection: a normal call
        // still works, proving the connection was not wedged by any of the
        // three frames above, AND the next line read from the kernel is this
        // call's OWN request — nothing was written back for any of the three
        // unmatched frames.
        let good_call = tokio::spawn({
            let calls = calls.clone();
            async move {
                calls
                    .call(
                        "speak",
                        serde_json::json!({"good": true}),
                        Duration::from_secs(5),
                    )
                    .await
            }
        });
        let next = recv_line(&mut kernel_read).await;
        assert_eq!(
            next["method"], "speak",
            "the kernel wrote something for an unmatched response instead of nothing: {next}"
        );
        let good_id = next["id"].as_str().unwrap().to_owned();
        send_line(
            &mut module_write,
            &format!(r#"{{"jsonrpc":"2.0","id":"{good_id}","result":{{"accepted":true}}}}"#),
        )
        .await;
        good_call
            .await
            .unwrap()
            .expect("the positive control call must still succeed");

        assert_eq!(
            calls.stray_responses(),
            3,
            "all three unmatched frames must be counted"
        );

        // `.shutdown()`, not `drop(..)` — see the note in
        // `a_call_after_the_connection_ends_is_not_sent` for why dropping
        // only one `tokio::io::split` half does not signal EOF while the
        // other (`kernel_read`) is still alive.
        module_write.shutdown().await.unwrap();
        let _ = conn.await;
    }

    /// The `Malformed` classification (§6.1 M5): `result`+`error` both
    /// present, both absent, `result` not an object, and a well-formed
    /// `error` mapping to [`KernelCallFailed::Rpc`].
    #[tokio::test]
    async fn kernel_call_classifies_the_modules_answer_shape() {
        let (calls, fut, mut kernel_read, mut module_write) = harness(methods());
        tokio::spawn(fut);

        async fn one_round(
            calls: &KernelCalls,
            kernel_read: &mut (impl AsyncBufRead + Unpin),
            module_write: &mut (impl AsyncWrite + Unpin),
            answer: &str,
        ) -> Result<Map<String, Value>, KernelCallFailed> {
            // `calls.call(..)` must be DRIVEN (spawned, here) concurrently
            // with reading its request line back — an async fn's body does
            // not run at all until first polled, so racing it unpolled
            // against `recv_line` in a single-armed `select!` (an earlier
            // version of this helper did exactly that) never sends the
            // request in the first place, and `recv_line` then waits
            // forever for a line nothing produced.
            let handle = tokio::spawn({
                let calls = calls.clone();
                async move {
                    calls
                        .call("speak", serde_json::json!({}), Duration::from_secs(5))
                        .await
                }
            });
            let request = recv_line(kernel_read).await;
            let id = request["id"].as_str().unwrap().to_owned();
            send_line(module_write, &answer.replace("{id}", &id)).await;
            handle.await.unwrap()
        }

        let malformed_neither = one_round(
            &calls,
            &mut kernel_read,
            &mut module_write,
            r#"{"jsonrpc":"2.0","id":"{id}"}"#,
        )
        .await
        .unwrap_err();
        assert!(matches!(malformed_neither, KernelCallFailed::Malformed(_)));

        let malformed_both = one_round(
            &calls,
            &mut kernel_read,
            &mut module_write,
            r#"{"jsonrpc":"2.0","id":"{id}","result":{},"error":{"code":1,"message":"x"}}"#,
        )
        .await
        .unwrap_err();
        assert!(matches!(malformed_both, KernelCallFailed::Malformed(_)));

        let malformed_result = one_round(
            &calls,
            &mut kernel_read,
            &mut module_write,
            r#"{"jsonrpc":"2.0","id":"{id}","result":1}"#,
        )
        .await
        .unwrap_err();
        assert!(matches!(malformed_result, KernelCallFailed::Malformed(_)));

        let rpc_error = one_round(
            &calls,
            &mut kernel_read,
            &mut module_write,
            r#"{"jsonrpc":"2.0","id":"{id}","error":{"code":-32602,"message":"bad name"}}"#,
        )
        .await
        .unwrap_err();
        match rpc_error {
            KernelCallFailed::Rpc(e) => {
                assert_eq!(e.code, -32602);
                assert_eq!(e.message, "bad name");
            }
            other => panic!("expected Rpc, got {other}"),
        }

        // Control: a well-formed, well-typed result is accepted.
        let ok = one_round(
            &calls,
            &mut kernel_read,
            &mut module_write,
            r#"{"jsonrpc":"2.0","id":"{id}","result":{"accepted":true}}"#,
        )
        .await
        .expect("a well-formed result must be accepted");
        assert_eq!(ok["accepted"], true);
    }

    /// §6.1: a call issued after the connection has already ended returns
    /// `NotSent` promptly, without waiting for its timeout.
    #[tokio::test]
    async fn a_call_after_the_connection_ends_is_not_sent() {
        let (calls, fut, _kernel_read, mut module_write) = harness(methods());
        // `.shutdown()`, not `drop(..)`: a `tokio::io::split` half shares the
        // underlying `DuplexStream` with its sibling via an internal `Arc`, so
        // dropping only the write half does NOT close the stream (and so does
        // NOT signal EOF to the peer) while `_kernel_read` — the other half of
        // the SAME split — is still alive. `shutdown()` is the documented way
        // to signal EOF from one split half regardless of its sibling.
        module_write.shutdown().await.unwrap(); // the "module" hangs up immediately
        let ended = fut.await;
        assert!(matches!(ended, Ended::PeerClosed));

        let result = tokio::time::timeout(
            Duration::from_millis(200),
            calls.call("speak", serde_json::json!({}), Duration::from_secs(30)),
        )
        .await
        .expect("must not wait for the 30s call timeout")
        .unwrap_err();
        assert!(matches!(result, KernelCallFailed::NotSent));
    }

    /// §6.1: a call already in flight when the connection ends resolves as
    /// `ConnectionLost` — not left hanging until ITS OWN timeout either.
    #[tokio::test]
    async fn an_in_flight_call_is_connection_lost_when_the_connection_ends() {
        let (calls, fut, mut kernel_read, mut module_write) = harness(methods());
        let conn = tokio::spawn(fut);

        let call = tokio::spawn(async move {
            calls
                .call("speak", serde_json::json!({}), Duration::from_secs(30))
                .await
        });
        let _request = recv_line(&mut kernel_read).await;

        // See the sibling test above for why `.shutdown()` and not `drop(..)`.
        module_write.shutdown().await.unwrap(); // the module hangs up mid-call
        let ended = conn.await.unwrap();
        assert!(matches!(ended, Ended::PeerClosed));

        let result = tokio::time::timeout(Duration::from_millis(200), call)
            .await
            .expect("must not wait for the 30s call timeout")
            .unwrap()
            .unwrap_err();
        assert!(matches!(result, KernelCallFailed::ConnectionLost));
    }

    /// A regression guard on the counter itself: it starts at zero and only
    /// moves for genuinely unmatched frames, not for every frame received.
    #[tokio::test]
    async fn stray_responses_starts_at_zero_and_only_counts_unmatched_frames() {
        let (calls, fut, mut kernel_read, mut module_write) = harness(methods());
        tokio::spawn(fut);
        assert_eq!(calls.stray_responses(), 0);

        send_line(
            &mut module_write,
            r#"{"jsonrpc":"2.0","id":"1","method":"echo","params":{}}"#,
        )
        .await;
        let _ = recv_line(&mut kernel_read).await;
        assert_eq!(
            calls.stray_responses(),
            0,
            "a request with `method` is not a stray response"
        );

        send_line(
            &mut module_write,
            r#"{"jsonrpc":"2.0","id":"nobody-waiting"}"#,
        )
        .await;
        // Give the mux loop a moment to process the frame above before
        // asserting — there is nothing to read back for it (that IS the
        // property), so a synchronization point is needed instead of a read.
        tokio::task::yield_now().await;
        for _ in 0..50 {
            if calls.stray_responses() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(calls.stray_responses(), 1);
    }

    // Regression guard for the id-counter format not colliding across many
    // calls in flight at once.
    #[tokio::test]
    async fn many_concurrent_kernel_calls_each_get_their_own_id() {
        let (calls, fut, mut kernel_read, mut module_write) = harness(methods());
        tokio::spawn(fut);

        let n = 16;
        let mut handles = Vec::new();
        for i in 0..n {
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                calls
                    .call(
                        "speak",
                        serde_json::json!({ "i": i }),
                        Duration::from_secs(5),
                    )
                    .await
            }));
        }

        let mut seen_ids = std::collections::HashSet::new();
        for _ in 0..n {
            let request = recv_line(&mut kernel_read).await;
            let id = request["id"].as_str().unwrap().to_owned();
            assert!(seen_ids.insert(id.clone()), "duplicate kernel call id {id}");
            send_line(
                &mut module_write,
                &format!(r#"{{"jsonrpc":"2.0","id":"{id}","result":{{"ok":true}}}}"#),
            )
            .await;
        }
        for h in handles {
            h.await.unwrap().expect("every call must succeed");
        }
    }
}

/// Regression tests for review findings C1, H1, H2/H2a, M1, M2. These are the
/// formal, in-tree versions of the review's own repro probes (previously a
/// scratch `review_probe` module, ported and turned into real assertions
/// here) plus additional coverage for H2, M1 and M2, which had no probe of
/// their own.
#[cfg(test)]
mod review_fixes {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::rpc::{CallFuture, Handler};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt as _, BufReader, DuplexStream};

    /// Answers `{"blob": <200 KiB of "x">}` — much bigger than a single 8 KiB
    /// relay chunk, so a response takes several reads off `serve_until`'s
    /// output pipe to fully arrive.
    struct Big;
    impl Handler for Big {
        fn check_params(&self, _p: &Value) -> Result<(), String> {
            Ok(())
        }
        fn call(&self, _params: Value) -> CallFuture {
            Box::pin(async move { Ok(serde_json::json!({ "blob": "x".repeat(200_000) })) })
        }
    }

    fn big_methods() -> Methods {
        Methods::none().with("big", Arc::new(Big))
    }

    async fn send_line(w: &mut (impl AsyncWrite + Unpin), line: &str) {
        w.write_all(line.as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
    }

    async fn recv_line(r: &mut (impl AsyncBufRead + Unpin)) -> Value {
        let mut line = String::new();
        let n = r.read_line(&mut line).await.unwrap();
        assert!(n > 0, "connection closed with nothing to read");
        serde_json::from_str(line.trim_end()).unwrap()
    }

    fn methods() -> Methods {
        Methods::none()
    }

    fn harness(
        methods: Methods,
    ) -> (
        KernelCalls,
        impl Future<Output = Ended> + Send,
        BufReader<tokio::io::ReadHalf<DuplexStream>>,
        tokio::io::WriteHalf<DuplexStream>,
    ) {
        let (kernel_side, module_side) = tokio::io::duplex(64 * 1024);
        let (kernel_read, kernel_write) = tokio::io::split(kernel_side);
        let (calls, fut) = serve_attached(
            BufReader::new(kernel_read),
            kernel_write,
            methods,
            Limits::default(),
            std::future::pending(),
        );
        let (module_read, module_write) = tokio::io::split(module_side);
        (calls, fut, BufReader::new(module_read), module_write)
    }

    /// C1 — the review's probe 1, ported: a >8 KiB `serve_until` response
    /// (well over the old raw-chunk relay's 8 KiB read buffer) interleaved
    /// with a storm of concurrent kernel-originated calls must never produce
    /// a line the module can't parse as JSON, and every line the module DOES
    /// see must be either a complete `big` response or a complete kernel
    /// request — never a byte-level splice of the two. On the pre-fix 8
    /// KiB-chunk relay this reliably went red: a `speak` request queued while
    /// a `big` response was mid-relay landed inside it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn big_response_and_kernel_calls_never_interleave_mid_line() {
        let (kernel_side, module_side) = tokio::io::duplex(4 * 1024);
        let (kr, kw) = tokio::io::split(kernel_side);
        let (calls, fut) = serve_attached(
            BufReader::new(kr),
            kw,
            big_methods(),
            Limits::default(),
            std::future::pending(),
        );
        tokio::spawn(fut);
        let (mr, mut mw) = tokio::io::split(module_side);
        let mut mr = BufReader::new(mr);

        const BIG_CALLS: usize = 20;
        const KERNEL_CALLS: usize = 2000;
        for i in 0..BIG_CALLS {
            send_line(
                &mut mw,
                &format!(r#"{{"jsonrpc":"2.0","id":"{i}","method":"big","params":{{}}}}"#),
            )
            .await;
        }
        let spawn_calls = calls.clone();
        tokio::spawn(async move {
            for _ in 0..KERNEL_CALLS {
                let c = spawn_calls.clone();
                tokio::spawn(async move {
                    let _ = c
                        .call("speak", serde_json::json!({}), Duration::from_secs(5))
                        .await;
                });
                tokio::time::sleep(Duration::from_micros(50)).await;
            }
        });

        let mut total = 0;
        let mut unparseable = 0;
        for _ in 0..(BIG_CALLS + KERNEL_CALLS) {
            let mut line = String::new();
            let read = tokio::time::timeout(Duration::from_secs(5), mr.read_line(&mut line)).await;
            let Ok(Ok(n)) = read else { break };
            if n == 0 {
                break;
            }
            total += 1;
            if serde_json::from_str::<Value>(line.trim_end()).is_err() {
                unparseable += 1;
            }
        }
        assert!(
            total >= BIG_CALLS,
            "expected at least the {BIG_CALLS} big responses, got {total} lines total"
        );
        assert_eq!(
            unparseable, 0,
            "every line the module receives must be complete, parseable JSON — a \
             non-zero count means a response and a kernel-originated request \
             interleaved mid-line (review C1)"
        );
    }

    /// H1 — the review's probe 2, ported and sharpened: the module never
    /// reads at all, so the real socket's write buffer fills. With a short
    /// `write_timeout`, the connection must end — as `Ended::WriteFailed` —
    /// well within it, rather than hanging until the module (never) reads.
    #[tokio::test]
    async fn module_never_reading_ends_the_connection_within_its_write_timeout() {
        let (kernel_side, module_side) = tokio::io::duplex(1024);
        let (kr, kw) = tokio::io::split(kernel_side);
        let limits = Limits {
            write_timeout: Duration::from_millis(100),
            ..Limits::default()
        };
        let (_calls, fut) = serve_attached(
            BufReader::new(kr),
            kw,
            big_methods(),
            limits,
            std::future::pending(),
        );
        let conn = tokio::spawn(fut);
        let (_mr, mut mw) = tokio::io::split(module_side);
        // Enough `big` requests to overflow the 1 KiB duplex buffer many
        // times over — the module below never reads any of the responses.
        for i in 0..5 {
            send_line(
                &mut mw,
                &format!(r#"{{"jsonrpc":"2.0","id":"{i}","method":"big","params":{{}}}}"#),
            )
            .await;
        }
        let ended = tokio::time::timeout(Duration::from_secs(2), conn)
            .await
            .expect(
                "the connection must end well within 2s given a 100ms write_timeout (review H1)",
            )
            .unwrap();
        assert!(
            matches!(ended, Ended::WriteFailed(_)),
            "expected WriteFailed, got {ended:?}"
        );
    }

    /// H1 — the real write half must close at connection end even if a
    /// `KernelCalls` clone (as `agent24d` would hold on the module's behalf)
    /// is kept alive well past that point: teardown must not depend on every
    /// `out_tx` clone having been dropped first.
    #[tokio::test]
    async fn the_real_write_half_closes_at_teardown_even_with_a_kernel_calls_clone_still_alive() {
        let (calls, fut, mut module_read, mut module_write) = harness(methods());
        let conn = tokio::spawn(fut);

        // End the connection the ordinary way (module hangs up) while
        // deliberately keeping `calls` (and so an `out_tx` clone) alive.
        module_write.shutdown().await.unwrap();
        let ended = conn.await.unwrap();
        assert!(matches!(ended, Ended::PeerClosed));

        // `calls` is still alive here — if teardown depended on every
        // `out_tx` clone dropping, the writer task (and the real write half
        // it owns) would still be sitting open waiting for one more chunk.
        let mut buf = [0u8; 8];
        let read = tokio::time::timeout(Duration::from_millis(500), module_read.read(&mut buf))
            .await
            .expect(
                "the module's read half must see EOF promptly once the connection ends, \
                     regardless of a live KernelCalls clone (review H1)",
            )
            .unwrap();
        assert_eq!(read, 0, "expected EOF (0 bytes), got {read}");

        drop(calls);
    }

    /// H2a — the internal task that forwards `stop` into the classifier loop
    /// and the nested `serve_until` must not outlive the connection. Modelled
    /// with a drop-flag rather than `Arc::strong_count` on `Generation`
    /// directly, since `attach_mux` does not depend on `drain`'s
    /// `Generation` type — the property under test ("this task's captured
    /// state is dropped when the connection ends") is identical either way.
    struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn the_stop_forwarding_task_does_not_outlive_the_connection() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = DropFlag(Arc::clone(&dropped));
        // A `stop` that never resolves on its own — the ONLY way its
        // captured state (`guard`) is ever dropped is if whatever awaits it
        // is torn down from the outside.
        let stop = async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        };
        let (kernel_side, module_side) = tokio::io::duplex(64 * 1024);
        let (kr, kw) = tokio::io::split(kernel_side);
        let (_calls, fut) =
            serve_attached(BufReader::new(kr), kw, methods(), Limits::default(), stop);
        let conn = tokio::spawn(fut);

        let (_mr, mut mw) = tokio::io::split(module_side);
        mw.shutdown().await.unwrap(); // end the connection some OTHER way than `stop`
        let ended = conn.await.unwrap();
        assert!(matches!(ended, Ended::PeerClosed));

        tokio::task::yield_now().await;
        assert!(
            dropped.load(Ordering::SeqCst),
            "the stop-forwarding task must be torn down with the connection, not left \
             waiting on a `stop` that will now never fire (review H2a)"
        );
    }

    /// M1 — many kernel-originated calls racing the connection's end (the
    /// module hangs up immediately) must all resolve promptly (well within
    /// their own generous timeout), never silently insert into a pending map
    /// the finaliser already cleared and hang until that timeout fires for
    /// real. `call()`'s "check closed, then insert" and the finaliser's "set
    /// closed, then clear" sharing one lock (`Shared::close`) is what
    /// guarantees this.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn calls_racing_connection_end_never_hang_past_their_own_timeout() {
        let (calls, fut, _kernel_read, mut module_write) = harness(methods());
        let conn = tokio::spawn(fut);

        const N: usize = 200;
        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                calls
                    .call("speak", serde_json::json!({}), Duration::from_secs(20))
                    .await
            }));
        }
        // Hang up right away, racing every one of the calls above against
        // the finaliser's close-and-clear.
        module_write.shutdown().await.unwrap();
        let _ = conn.await;

        for h in handles {
            let _ = tokio::time::timeout(Duration::from_millis(500), h)
                .await
                .expect(
                    "a call racing the connection's end must resolve promptly, not hang \
                 until its own 20s timeout (review M1)",
                );
        }
    }

    /// M2 — the connection task being cancelled (aborted, or dropped by a
    /// caller racing it in a `select!`) instead of running to its own normal
    /// completion must STILL tear everything down: an in-flight call
    /// resolves as `ConnectionLost` promptly, and a call issued afterward
    /// fails fast with `NotSent` — proving `closed` really got set, not just
    /// that the one call in flight happened to be dropped along with
    /// everything else.
    #[tokio::test]
    async fn cancelling_the_connection_task_still_tears_down_pending_calls() {
        let (calls, fut, mut kernel_read, mut module_write) = harness(methods());
        let conn = tokio::spawn(fut);

        let call = tokio::spawn({
            let calls = calls.clone();
            async move {
                calls
                    .call("speak", serde_json::json!({}), Duration::from_secs(20))
                    .await
            }
        });
        let _request = recv_line(&mut kernel_read).await; // it is genuinely in flight

        // Cancel the connection task instead of letting it end normally —
        // drops its future (and so its `Teardown`) mid-flight, same as an
        // external `select!` dropping `fut` (review M2).
        conn.abort();
        let _ = conn.await;

        let result = tokio::time::timeout(Duration::from_millis(500), call)
            .await
            .expect(
                "an in-flight call must not hang past its own timeout when the \
                     connection task is cancelled (review M2)",
            )
            .unwrap()
            .unwrap_err();
        assert!(matches!(result, KernelCallFailed::ConnectionLost));

        let after = tokio::time::timeout(
            Duration::from_millis(200),
            calls.call("speak", serde_json::json!({}), Duration::from_secs(20)),
        )
        .await
        .expect("a call issued after cancellation must not hang either")
        .unwrap_err();
        assert!(matches!(after, KernelCallFailed::NotSent));

        let _ = module_write.shutdown().await;
    }

    /// M2 ("顺手做") — a `call()` future that is cancelled (dropped) before
    /// it ever gets an answer must not leave its pending entry behind
    /// forever: a late response for the SAME id, arriving after the
    /// cancellation, must be counted as stray rather than silently handed to
    /// a receiver that no longer exists.
    #[tokio::test]
    async fn a_cancelled_call_leaves_no_pending_entry_behind() {
        let (calls, fut, mut kernel_read, mut module_write) = harness(methods());
        tokio::spawn(fut);

        let call = tokio::spawn({
            let calls = calls.clone();
            async move {
                calls
                    .call("speak", serde_json::json!({}), Duration::from_secs(30))
                    .await
            }
        });
        let request = recv_line(&mut kernel_read).await;
        let id = request["id"].as_str().unwrap().to_owned();

        call.abort();
        let _ = call.await;

        send_line(
            &mut module_write,
            &format!(r#"{{"jsonrpc":"2.0","id":"{id}","result":{{"late":true}}}}"#),
        )
        .await;
        tokio::task::yield_now().await;
        let mut stray = calls.stray_responses();
        for _ in 0..50 {
            if stray == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
            stray = calls.stray_responses();
        }
        assert_eq!(
            stray, 1,
            "the cancelled call's pending entry must have been removed by its guard, \
             so a late response for the same id is counted as stray, not silently \
             matched (review M2)"
        );
    }

    /// Review (Codex A3 follow-up): before the fix, `Teardown` (and so every
    /// `AbortOnDrop` wrapping `su_handle`/`writer_handle`/`stop_task`) was
    /// built as the FIRST STATEMENT inside the `async move { ... }` block
    /// `serve_attached` returns — which only ever runs once that future is
    /// actually polled. A caller that drops the returned future before its
    /// first poll (e.g. a `select!` arm that lost a race before ever
    /// reaching this one) never executes that statement, so the three
    /// `tokio::spawn`s from just above it — all of which happen
    /// unconditionally, inside `serve_attached`'s own synchronous body, well
    /// before the future is even constructed — are never wrapped in
    /// `AbortOnDrop` at all. Dropping a bare `JoinHandle` (without calling
    /// `.abort()` first) only DETACHES it; the task keeps running, leaked,
    /// for good.
    ///
    /// This proves it on the `writer_handle` task specifically: it owns the
    /// sole `Receiver` half of `KernelCalls::out_tx`'s channel, so as long as
    /// that task is alive, `enqueue_raw` (a plain `try_send`) keeps
    /// succeeding — a full round trip through a live, un-aborted task,
    /// exactly what "leaked" means here. Fixed, dropping the future before
    /// polling it must abort `writer_handle` (via `Teardown`'s `Drop`) and so
    /// drop its `Receiver`, which makes every subsequent `try_send` fail.
    #[tokio::test]
    async fn dropping_the_future_before_its_first_poll_still_tears_down_the_background_tasks() {
        let (calls, fut, _kernel_read, _module_write) = harness(methods());

        // Never spawned, never `.await`ed — dropped with zero polls, the
        // exact case `Teardown`'s in-block construction used to miss.
        drop(fut);

        // `JoinHandle::abort()` only REQUESTS cancellation; the task's own
        // drop glue (including dropping `writer_handle`'s `out_rx`) runs on
        // a later poll of the runtime, not synchronously here.
        let mut leaked = true;
        for _ in 0..200 {
            if calls.enqueue_raw(b"probe\n".to_vec()).is_err() {
                leaked = false;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            !leaked,
            "the writer task must have been aborted (dropping its out_rx) even though the \
             returned future was dropped before its first poll — otherwise it (and the other \
             two background tasks) leak forever"
        );
    }
}
