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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::{AsyncBufRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};

use crate::frame::FrameError;
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

/// State shared between every clone of a [`KernelCalls`] handle and the mux
/// loop that resolves or drops incoming frames.
struct Shared {
    pending: Mutex<HashMap<String, oneshot::Sender<Map<String, Value>>>>,
    next_id: AtomicU64,
    stray_responses: AtomicU64,
    /// Set once the connection has ended. An advisory fast path only — see
    /// [`KernelCalls::call`] — not a second source of truth: the pending map
    /// being cleared on connection end is what actually guarantees an
    /// in-flight call is resolved.
    closed: AtomicBool,
}

/// Handle for kernel-originated requests on an attached connection. Cheap to
/// clone (an `Arc` and an `mpsc::UnboundedSender` inside).
#[derive(Clone)]
pub struct KernelCalls {
    shared: Arc<Shared>,
    /// Raw NDJSON lines (newline included), merged with `serve_until`'s own
    /// output onto the real connection by the single writer task `serve_attached`
    /// spawns.
    out_tx: mpsc::UnboundedSender<Vec<u8>>,
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
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(KernelCallFailed::NotSent);
        }
        let id = format!("k{}", self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self
                .shared
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            pending.insert(id.clone(), tx);
        }
        let mut line = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .unwrap_or_default();
        line.push(b'\n');
        if self.out_tx.send(line).is_err() {
            self.forget(&id);
            return Err(KernelCallFailed::NotSent);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(obj)) => interpret_kernel_response(obj),
            // The sender was dropped without ever sending — only happens when
            // the connection ends (`serve_attached`'s finalizer drains and
            // drops every pending sender; see there).
            Ok(Err(_recv_error)) => Err(KernelCallFailed::ConnectionLost),
            Err(_elapsed) => {
                self.forget(&id);
                Err(KernelCallFailed::Timeout)
            }
        }
    }

    fn forget(&self, id: &str) {
        let mut pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pending.remove(id);
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
        pending.remove(&id)
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
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(0),
        stray_responses: AtomicU64::new(0),
        closed: AtomicBool::new(false),
    });
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let calls = KernelCalls {
        shared: Arc::clone(&shared),
        out_tx: out_tx.clone(),
    };

    // One shared stop signal, fanned out to the classifier loop below AND to
    // the nested `serve_until` — `stop` itself is a one-shot, `!Clone`
    // future, so it is awaited exactly once, here, and its firing is
    // rebroadcast through a `watch`.
    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(async move {
        stop.await;
        let _ = stop_tx.send(true);
    });

    // Pipe #1: classified frames destined for `serve_until`, as if it were
    // reading the real connection directly.
    const PIPE_CAPACITY: usize = crate::frame::MAX_FRAME_BYTES + 4096;
    let (to_su_write, to_su_read) = tokio::io::duplex(PIPE_CAPACITY);
    // Pipe #2: `serve_until`'s own output, relayed onward below.
    let (su_writer, mut su_read) = tokio::io::duplex(PIPE_CAPACITY);

    let su_stop = watch_true(stop_rx.clone());
    let su_handle = tokio::spawn(serve_until(
        tokio::io::BufReader::new(to_su_read),
        su_writer,
        methods,
        limits,
        su_stop,
    ));

    // Relay `serve_until`'s raw bytes onto the shared output channel. Ends
    // when `serve_until`'s writer half is dropped (task finished) and its
    // buffered bytes are drained — an ordinary EOF, not an error.
    let relay_out_tx = out_tx.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match su_read.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if relay_out_tx.send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });

    // The single writer task: everything destined for the real module —
    // `serve_until`'s answers and this side's kernel-originated requests —
    // is serialised through this one channel and written in the order it
    // arrives, so the two producers above can never interleave a write.
    tokio::spawn(async move {
        let mut writer = writer;
        while let Some(chunk) = out_rx.recv().await {
            if writer.write_all(&chunk).await.is_err() {
                return;
            }
        }
        let _ = writer.flush().await;
    });

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
                frame = frames_rx.recv() => match frame {
                    None => break Ended::PeerClosed,
                    Some(Err(e)) => break map_frame_error(e),
                    Some(Ok(bytes)) => {
                        if let Some(forwarded) = route_frame(&bytes, &shared)
                            && let Err(e) = write_frame(&mut to_su_write, forwarded).await
                        {
                            break e;
                        }
                    }
                },
            }
        };

        // The connection is over: no more kernel calls may be issued or
        // answered. Order matters — `closed` first (a `call()` racing this
        // exact instant sees either the old state and enqueues into a still-
        // open pipeline, or the new one and fails fast; either is fine), then
        // drop every pending sender so each in-flight `call()` observes
        // `ConnectionLost` rather than hanging until its own timeout.
        shared.closed.store(true, Ordering::Release);
        {
            let mut pending = shared
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            pending.clear();
        }
        drop(to_su_write);
        let _ = su_handle.await;
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
