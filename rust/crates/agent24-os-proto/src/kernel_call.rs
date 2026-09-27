//! ME4-1.3.1 — design `docs/design/ME4-S1-scheduler-callback.md` §5.2/§5.3: a
//! kernel-ORIGINATED request into one module generation — the "fired"
//! delivery the scheduler's delivery pump (`agent24-scheduler::deliveries`)
//! sends. Reference implementation: the frozen (v3.1) design's scratch check
//! crate `src/kernel_call.rs`, ported here to call the REAL crate-private
//! [`crate::proxy::exchange`] (`force_fresh = true`) instead of a stub.
//!
//! This does **not** go through [`crate::proxy::proxy`] (loopback HTTP +
//! bearer): it uses the proxy's own UDS upstream machinery directly, on the
//! module's live [`Generation`] — §5.2 L4: internal requests are not counted
//! against `MAX_INFLIGHT_PER_MODULE` and are not visible to a client; they are
//! bounded instead by the delivery pump's own per-owner/global ceilings
//! (§5.4).
//!
//! # The "bytes may have left" flag (design v2 H1, v3 H-A)
//!
//! [`InFlight::dispatch`] is called TWICE in the real send path (once by this
//! module before building the request, once by [`crate::proxy::exchange`]'s
//! own `send_guard` right before the physical send) — the same shape
//! `crate::proxy::forward`'s retry path already has. So
//! `Abandoned::dispatched` from [`InFlight::finish`] only says "`dispatch()`
//! once returned `true`", never "bytes may have left". [`send_kernel_request`]
//! keeps its OWN flag instead, set the moment the `send_guard` itself lets the
//! send through — the last point this function can observe before hyper takes
//! the request. An earlier design (v2) set the flag when `exchange` RETURNED,
//! but the real `exchange` returns only once a response head arrives
//! (`proxy.rs`'s `send_request(..).await`), so a revocation while the module
//! sat on a fully-read request left the flag unset and a module that crashes
//! on every fired delivery would be redelivered forever — v3 (H-A) fixed that
//! by moving the write into the guard itself. The remaining, DELIBERATE
//! imprecision — between the guard returning `true` and hyper's dispatcher
//! actually accepting the request a few lines later — is residual risk R11
//! (design §14): it can only make this module count one MORE attempt than
//! strictly happened, never fewer, and never turn an undelivered fire into
//! one recorded as delivered.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full, Limited};
use serde::{Deserialize, Serialize};

use crate::drain::{Abandoned, Generation, InFlight, RequestRefused, sha256};
use crate::proxy::{self, ExchangeError, IdleConnections};

/// design §5.3: the fired body's `key` header — `owner.module_key` (never the
/// internal `schedule_id`).
pub const SCHEDULE_KEY_HEADER: &str = "x-a24-schedule-key";
/// design §5.3: which fire this attempt is for — stable across every retry of
/// the same slot (design §4.2).
pub const FIRE_ID_HEADER: &str = "x-a24-fire-id";

/// design §5.2, FU-60: fixed and kernel-chosen, independent of the transport —
/// never derived from the module's socket path. Deliberately NOT reused from
/// `proxy::UPSTREAM_HOST` (private to that module, and not worth widening
/// its visibility for one more call site): the value itself is part of the
/// wire contract module authors read (SPEC-ME3 §2), so it is pinned here too
/// by `the_upstream_host_matches_the_proxys` below.
const UPSTREAM_HOST: &str = "agent24-module.invalid";

/// What the kernel sends. Built by the kernel only — there is no client input
/// in it, so nothing here is sanitised; every `extra_headers` name must be
/// `x-a24-*` (asserted in debug builds by [`exchange_once`]), which the
/// response-side prefix strip and the proxy's own request-side strip already
/// cover if this ever crossed the proxy (it does not: this is a direct UDS
/// exchange, and the module cannot tell it apart from any other request on
/// the wire).
pub struct KernelRequest {
    /// Origin-form, e.g. `/api/v1/sin90/_a24/scheduler/fired`.
    pub path: String,
    pub extra_headers: Vec<(HeaderName, HeaderValue)>,
    /// `application/json`.
    pub body: Bytes,
}

/// design §5.4 (v3, L-F): the pump injects production limits (10s / 64 KiB)
/// and a test shrinks `total` to reach the timeout branch in milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct KernelLimits {
    /// The admission budget AND the end-to-end deadline (head + body).
    pub total: Duration,
    pub max_response_bytes: usize,
}

/// Only the status matters: the body is drained under the limit and dropped
/// (design v2, L2 — a 2xx head is the acknowledgement, whatever the body then
/// does).
#[derive(Debug)]
pub struct KernelResponse {
    pub status: StatusCode,
}

/// ME4-S3 §2.5/§4.4 (M7): the body of `POST /api/v1/<ns>/_a24/scheduler/
/// fired` — moved here (owned) from `agent24d::scheduler_deliver`'s private
/// borrowed `FiredBody<'a>` so the kernel (this module, serializing) and the
/// SDK's `fired` extractor (deserializing) share one type instead of two
/// independently-maintained mirrors. `deny_unknown_fields`: unlike a call
/// RESPONSE (§2.4's "responses are lenient" rule doesn't apply here — this is
/// the kernel-originated REQUEST body on the reverse channel), the kernel
/// controls both ends of this shape and a drifting field should fail loudly
/// rather than be silently dropped by an older SDK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiredBody {
    pub key: String,
    /// `"tick"` | `"run_now"`.
    pub trigger: String,
    pub scheduled_for: String,
    pub fired_at: String,
}

/// Every way [`send_kernel_request`] did not produce a response. The split
/// that matters to the caller is "could the module have seen it" (design
/// §5.3's `classify`, `agent24d::scheduler_deliver`).
#[derive(Debug)]
pub enum KernelCallError {
    /// [`Generation::admit_request`] refused: nothing admitted, nothing sent.
    Refused(RequestRefused),
    /// No entropy for the approval token: nothing admitted.
    EntropyUnavailable,
    /// [`InFlight::dispatch`] said revoked, before the first attempt: admitted,
    /// never sent.
    NotDispatched,
    /// The generation was revoked while this was in flight
    /// ([`InFlight::finish`]'s `Err`). Takes precedence over whatever the
    /// transport said (see [`send_kernel_request`]'s doc comment).
    Abandoned(Abandoned),
    /// Could not connect, or the send was withdrawn before it reached the
    /// wire — the generation is still live, nothing reached the module.
    NotSent(String),
    /// The module may have received this attempt.
    MaybeSent(String),
    Timeout,
    ResponseTooLarge,
    /// Review round 1, L3: a NON-2xx response's body failed to read for a
    /// reason OTHER than exceeding `limits.max_response_bytes` (a length
    /// limit hit is [`Self::ResponseTooLarge`] instead) — a truncated or
    /// reset connection while the module was answering, most likely. Kept
    /// distinct from `ResponseTooLarge` so `last_error` reports what
    /// actually happened rather than claiming a size limit that was never
    /// reached.
    ResponseBodyError(String),
    /// A `Running` generation always has one; refused rather than trusted.
    NoUpstream,
}

/// `sch-<8 hex>-<n>`: shaped so it can never collide with the proxy's own
/// `<8 hex>-<n>` ids within the same generation (`admit_request` would refuse
/// a real collision as `DuplicateId` — this prefix just means it never has
/// reason to try).
pub struct KernelRequestIds {
    prefix: String,
    next: AtomicU64,
}

impl KernelRequestIds {
    #[must_use]
    pub fn new(prefix8hex: String) -> Self {
        Self {
            prefix: prefix8hex,
            next: AtomicU64::new(0),
        }
    }

    /// A fresh, process-random prefix (mirrors `proxy`'s own correlation-id
    /// minting, `RequestIds::new`/`short_prefix` — not reused directly since
    /// those are private to `proxy`, and the two prefixes only need to be
    /// shaped alike, not share a source).
    #[must_use]
    pub fn new_random() -> Self {
        Self::new(random_hex8())
    }

    fn mint(&self) -> String {
        format!(
            "sch-{}-{}",
            self.prefix,
            self.next.fetch_add(1, Ordering::Relaxed)
        )
    }
}

fn random_hex8() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 4];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok()
    {
        return bytes.iter().map(|b| format!("{b:02x}")).collect();
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{nanos:08x}")
}

/// Admit → build → `dispatch()` → send on a FRESH connection (never pooled: a
/// kernel request is not idempotent, design §5.2 step 5) → read under the
/// deadline → `finish()`. Holds the [`InFlight`] for the whole exchange, so
/// the request id it injects is a REAL in-flight request of `generation`:
/// callbacks carrying it pass `admit_callback_bound` even while Draining, and
/// stop passing the moment this function returns (design §5.2, judgement
/// C4.6).
///
/// `before_guard` is a test seam (production passes `None`, the same shape as
/// `crate::proxy`'s own `TakeSendGate`): it runs after the connection exists
/// and before the send_guard — the one window where a test **inside this
/// crate** can call the crate-private [`Generation::revoke`] deterministically
/// (judgement C4.7b). `limits.total` is both the admission budget (so every
/// callback bound to this request's id is bounded by it) and the end-to-end
/// deadline this call races against.
///
/// # Errors
/// See [`KernelCallError`].
pub async fn send_kernel_request(
    generation: &Arc<Generation>,
    ids: &KernelRequestIds,
    request: KernelRequest,
    limits: KernelLimits,
    before_guard: Option<&(dyn Fn() + Sync)>,
) -> Result<KernelResponse, KernelCallError> {
    let id = ids.mint();
    let token = proxy::mint_approval_token().ok_or(KernelCallError::EntropyUnavailable)?;
    let in_flight = generation
        .admit_request(
            id.clone(),
            sha256(token.as_bytes()),
            Instant::now(),
            limits.total,
        )
        .map_err(KernelCallError::Refused)?;
    let may_have_left = AtomicBool::new(false);
    let head = std::sync::OnceLock::<StatusCode>::new();
    // Race the exchange against revocation, exactly like `proxy::proxy`.
    let outcome = tokio::select! {
        r = tokio::time::timeout(
            limits.total,
            exchange_once(&in_flight, &id, &token, request, limits, &may_have_left, &head, before_guard),
        ) => Some(r.unwrap_or(Err(KernelCallError::Timeout))),
        () = in_flight.revoked() => None,
    };
    // The commit point: read BEFORE trusting `outcome` (design §5.2 step 8).
    let finished = in_flight.finish();
    // v2 (L2): a 2xx head is the acknowledgement — body overflow, timeout, or
    // a revocation racing `finish()` after this point change nothing.
    if let Some(status) = head.get().filter(|s| s.is_success()) {
        return Ok(KernelResponse { status: *status });
    }
    let left = may_have_left.load(Ordering::SeqCst);
    match (finished, outcome) {
        (Err(_), _) | (Ok(()), None) => {
            Err(KernelCallError::Abandoned(Abandoned { dispatched: left }))
        }
        (Ok(()), Some(r)) => r,
    }
}

#[allow(clippy::too_many_arguments)]
async fn exchange_once(
    in_flight: &InFlight,
    id: &str,
    token: &str,
    request: KernelRequest,
    limits: KernelLimits,
    may_have_left: &AtomicBool,
    head: &std::sync::OnceLock<StatusCode>,
    before_guard: Option<&(dyn Fn() + Sync)>,
) -> Result<KernelResponse, KernelCallError> {
    let upstream = in_flight.upstream().ok_or(KernelCallError::NoUpstream)?;
    let uri: Uri = request
        .path
        .parse()
        .map_err(|e| KernelCallError::NotSent(format!("bad kernel path: {e}")))?;
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .body(Full::new(request.body))
        .map_err(|e| KernelCallError::NotSent(e.to_string()))?;
    let h = req.headers_mut();
    for (name, value) in request.extra_headers {
        debug_assert!(
            name.as_str().starts_with(proxy::A24_HEADER_PREFIX),
            "kernel_call extra_headers must all be x-a24-*"
        );
        h.insert(name, value);
    }
    h.insert(
        HeaderName::from_static(proxy::REQUEST_ID_HEADER),
        HeaderValue::from_str(id).map_err(|e| KernelCallError::NotSent(e.to_string()))?,
    );
    // The secret twin of `request_id` — injected at the same admission point
    // and the same way as every proxied request (`proxy::forward`), never
    // minted anywhere else.
    h.insert(
        HeaderName::from_static(proxy::APPROVAL_TOKEN_HEADER),
        HeaderValue::from_str(token).map_err(|e| KernelCallError::NotSent(e.to_string()))?,
    );
    h.insert(
        axum::http::header::HOST,
        HeaderValue::from_static(UPSTREAM_HOST),
    );
    h.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );

    // design §5.2 step 4: constructed, THEN dispatch() — a `false` here means
    // revoked after admission, before the first attempt: not one byte goes
    // out (judgement C4.7a).
    if !in_flight.dispatch() {
        return Err(KernelCallError::NotDispatched);
    }
    // v3 (H-A): the "bytes may have left" flag is set by the guard ITSELF,
    // the moment it lets the send through — see the module doc.
    let guard = || {
        if let Some(hook) = before_guard {
            hook();
        }
        let ok = in_flight.dispatch();
        if ok {
            may_have_left.store(true, Ordering::SeqCst);
        }
        ok
    };
    // A throwaway pool: `force_fresh = true` means `exchange` never consults
    // it (design §5.2, v3.1 L4) — one is still required by `exchange`'s
    // signature.
    let idle = IdleConnections::default();
    let (response, _connection) = match proxy::exchange(
        &idle,
        in_flight.generation(),
        upstream,
        req,
        true,
        None,
        Some(&guard),
    )
    .await
    {
        Ok(r) => r,
        Err(ExchangeError::NotSent(e)) => return Err(KernelCallError::NotSent(e)),
        Err(ExchangeError::MaybeSent(e)) => return Err(KernelCallError::MaybeSent(e)),
    };
    let (parts, body) = response.into_parts();
    // Recorded the instant the head arrives — before the body is even
    // touched, so a revocation or an oversized body afterward cannot erase a
    // 2xx that already happened (design v2, L2).
    let _ = head.set(parts.status);
    // Review round 1, L3: only a genuine length-limit hit is `ResponseTooLarge`
    // — any OTHER body-read failure (the module hung up mid-answer, a reset
    // connection) is a real, different problem and must say so, not claim a
    // size limit that was never reached. A 2xx head is unaffected either way
    // (design v2, L2): `send_kernel_request`'s own head-check treats it as
    // `Ok` regardless of what this function returns.
    if let Err(err) = Limited::new(body, limits.max_response_bytes)
        .collect()
        .await
        && !parts.status.is_success()
    {
        return if proxy::is_length_limit(&*err) {
            Err(KernelCallError::ResponseTooLarge)
        } else {
            Err(KernelCallError::ResponseBodyError(err.to_string()))
        };
    }
    Ok(KernelResponse {
        status: parts.status,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::drain::CallbackRefused;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64 as StdAtomicU64;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn unique_sock(tag: &str) -> PathBuf {
        static SEQ: StdAtomicU64 = StdAtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        PathBuf::from(format!(
            "/tmp/a24-kernel-call-{tag}-{}-{nanos}-{n}.sock",
            std::process::id()
        ))
    }

    fn running_generation(upstream: PathBuf) -> Arc<Generation> {
        let g = Generation::serving_at(upstream);
        assert!(g.ready());
        g
    }

    fn ids() -> KernelRequestIds {
        KernelRequestIds::new("aaaaaaaa".to_owned())
    }

    fn limits() -> KernelLimits {
        KernelLimits {
            total: Duration::from_secs(5),
            max_response_bytes: 64 * 1024,
        }
    }

    fn short_limits(total: Duration) -> KernelLimits {
        KernelLimits {
            total,
            max_response_bytes: 64 * 1024,
        }
    }

    fn fired_request() -> KernelRequest {
        KernelRequest {
            path: "/api/v1/zz/_a24/scheduler/fired".to_owned(),
            extra_headers: vec![
                (
                    HeaderName::from_static(SCHEDULE_KEY_HEADER),
                    HeaderValue::from_static("k"),
                ),
                (
                    HeaderName::from_static(FIRE_ID_HEADER),
                    HeaderValue::from_static("fire_test"),
                ),
            ],
            body: Bytes::from_static(b"{\"key\":\"k\"}"),
        }
    }

    /// A raw upstream that reads exactly one HTTP/1.1 request (headers +
    /// `Content-Length` body) off the socket, hands it to the caller, then
    /// answers with `respond`.
    async fn raw_upstream_capturing(
        respond: &'static [u8],
    ) -> (PathBuf, tokio::sync::oneshot::Receiver<(String, Vec<u8>)>) {
        let path = unique_sock("capture");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head, body) = loop {
                let n = socket.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break (String::from_utf8_lossy(&buf).into_owned(), Vec::new());
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(idx) = text.find("\r\n\r\n") {
                    let head = text[..idx].to_owned();
                    let content_length: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().to_owned())
                        })
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let body_start = idx + 4;
                    while buf.len() < body_start + content_length {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body =
                        buf[body_start..(body_start + content_length).min(buf.len())].to_vec();
                    break (head, body);
                }
            };
            let _ = socket.write_all(respond).await;
            let _ = tx.send((head, body));
        });
        (path, rx)
    }

    /// C4.1 (shape): the request the module actually receives carries every
    /// header §5.3 promises, POSTs the right path, and the body is the JSON
    /// the caller handed in.
    #[tokio::test]
    async fn the_module_receives_the_fired_headers_path_and_body() {
        let (path, rx) =
            raw_upstream_capturing(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        let generation = running_generation(path);
        let result =
            send_kernel_request(&generation, &ids(), fired_request(), limits(), None).await;
        assert!(matches!(result, Ok(KernelResponse { status }) if status.is_success()));
        let (head, body) = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("the module never received a request")
            .unwrap();
        assert!(
            head.starts_with("POST /api/v1/zz/_a24/scheduler/fired HTTP/1.1"),
            "{head}"
        );
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("x-a24-schedule-key: k"), "{head}");
        assert!(lower.contains("x-a24-fire-id: fire_test"), "{head}");
        assert!(lower.contains("x-a24-request-id: sch-aaaaaaaa-0"), "{head}");
        assert!(lower.contains("x-a24-approval-token:"), "{head}");
        assert!(lower.contains(&format!("host: {UPSTREAM_HOST}")), "{head}");
        assert_eq!(body, b"{\"key\":\"k\"}");
    }

    /// Review round 1, M2: this used to be named as if it exercised C4.7a's
    /// own window — it does not. `send_kernel_request` calls its OWN internal
    /// `admit_request` first; a generation already revoked before that call
    /// even starts is refused at ADMISSION (`RequestRefused::Stopping`), never
    /// reaching `exchange_once`'s `dispatch()` check at all. Renamed to say
    /// so; the real C4.7a window (revoked between `admit_request` succeeding
    /// and the first `dispatch()` call, inside ONE `exchange_once` call) is
    /// `exchange_once_returns_not_dispatched_when_revoked_before_the_first_
    /// dispatch_call` below, which calls the private `exchange_once` directly.
    #[tokio::test]
    async fn admission_is_refused_once_the_generation_is_already_revoked() {
        let (path, rx) =
            raw_upstream_capturing(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        let generation = running_generation(path);
        // A throwaway probe id, admitted then immediately revoked — just to
        // put the generation into `Revoked` before the real call below.
        let in_flight = generation
            .admit_request(
                "probe".to_owned(),
                sha256(b"tok"),
                Instant::now(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(generation.revoke().is_some());
        drop(in_flight);
        let result =
            send_kernel_request(&generation, &ids(), fired_request(), limits(), None).await;
        assert!(
            matches!(
                result,
                Err(KernelCallError::Refused(RequestRefused::Stopping))
            ),
            "{result:?}"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rx)
                .await
                .is_err(),
            "the module must never have been dialled"
        );
    }

    /// Review round 1, M2 (same reclassification as the test above): a
    /// generation revoked with NOTHING ever admitted into it also refuses
    /// admission — the same `Refused` path, one step simpler.
    #[tokio::test]
    async fn a_generation_with_nothing_ever_admitted_also_refuses_once_revoked() {
        let (path, rx) =
            raw_upstream_capturing(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        let generation = running_generation(path);
        assert!(generation.revoke().is_some());
        let result =
            send_kernel_request(&generation, &ids(), fired_request(), limits(), None).await;
        assert!(
            matches!(result, Err(KernelCallError::Refused(_))),
            "{result:?}"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rx)
                .await
                .is_err()
        );
    }

    /// Review round 1, **M2**, judgement **C4.7a**, the REAL window this time:
    /// `admit_request` succeeds (the generation is `Running`), THEN it is
    /// revoked, THEN `exchange_once` is called on the resulting `InFlight` —
    /// exactly the sequence `send_kernel_request` itself runs, but with the
    /// revoke landing in the one-line gap between admission and the first
    /// `dispatch()` call that no external caller can otherwise pry open (no
    /// `.await` separates them in `send_kernel_request`). Calling the private
    /// `exchange_once` directly, from this same module's test code, is the
    /// only way to land the test deterministically in that exact gap.
    ///
    /// Mutation: delete the `if !in_flight.dispatch() { return Err(
    /// NotDispatched); }` check at the top of `exchange_once` — this test
    /// goes red (the mock upstream then receives a real request instead of
    /// zero bytes).
    #[tokio::test]
    async fn exchange_once_returns_not_dispatched_when_revoked_before_the_first_dispatch_call() {
        let (path, rx) =
            raw_upstream_capturing(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        let generation = running_generation(path);
        let in_flight = generation
            .admit_request(
                "x".to_owned(),
                sha256(b"t"),
                Instant::now(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(generation.revoke().is_some());
        let flag = AtomicBool::new(false);
        let head = std::sync::OnceLock::new();
        let result = exchange_once(
            &in_flight,
            "x",
            "tok",
            fired_request(),
            limits(),
            &flag,
            &head,
            None,
        )
        .await;
        assert!(
            matches!(result, Err(KernelCallError::NotDispatched)),
            "{result:?}"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rx)
                .await
                .is_err(),
            "the module must never have been dialled"
        );
    }

    /// design v2 (H1) / v3 (M-D), judgement **C4.7b**: revoked AFTER the
    /// connection is established but BEFORE `send_guard` lets the send
    /// through — the module must accept the TCP/UDS connection but read zero
    /// request bytes, and the result must be `Abandoned{dispatched:false}`
    /// (→ `Deferred(NeverSent)` in `agent24d::scheduler_deliver::classify`),
    /// not `dispatched:true`. This is the exact seam `before_guard` exists
    /// for (design §5.2): it runs after `Upstream::connect` completes and
    /// before the guard's own `dispatch()` call.
    #[tokio::test]
    async fn revoked_after_connect_but_before_the_send_guard_sends_nothing() {
        let path = unique_sock("c4-7b");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (dial_tx, dial_rx) = tokio::sync::oneshot::channel::<()>();
        let (read_tx, read_rx) = tokio::sync::oneshot::channel::<bool>();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let _ = dial_tx.send(());
            let mut buf = [0u8; 1];
            let wrote = matches!(socket.read(&mut buf).await, Ok(n) if n > 0);
            let _ = read_tx.send(wrote);
        });
        let generation = running_generation(path);
        let revoke_generation = Arc::clone(&generation);
        let before_guard = move || {
            assert!(
                revoke_generation.revoke().is_some(),
                "the generation must still be revocable exactly once here"
            );
        };
        let result = send_kernel_request(
            &generation,
            &ids(),
            fired_request(),
            limits(),
            Some(&before_guard),
        )
        .await;
        match result {
            Err(KernelCallError::Abandoned(Abandoned { dispatched })) => {
                assert!(!dispatched, "C4.7b must record dispatched:false");
            }
            other => panic!("expected Abandoned{{dispatched:false}}, got {other:?}"),
        }
        tokio::time::timeout(Duration::from_secs(5), dial_rx)
            .await
            .expect("the connect must have happened before the guard ran")
            .unwrap();
        let wrote = tokio::time::timeout(Duration::from_secs(5), read_rx)
            .await
            .expect("the server side never observed the connection close")
            .unwrap();
        assert!(
            !wrote,
            "the guard must stop the send before any byte reaches the module"
        );
    }

    /// Mutation check for C4.7b (design's own prescribed mutation): using
    /// `Abandoned.dispatched` (the generation's own bookkeeping — `true`
    /// because `dispatch()` DID return `true` once, before the guard even
    /// ran) instead of this module's local flag would report `dispatched:
    /// true` for the very same scenario above. This test pins that the LOCAL
    /// flag and the generation's raw bookkeeping actually disagree here,
    /// which is exactly why `send_kernel_request` cannot use the latter.
    #[tokio::test]
    async fn c4_7b_scenario_would_mutate_red_on_abandoned_dispatched_alone() {
        // No real listener is needed: this test drives `Generation`/`InFlight`
        // directly and never calls `exchange`, so nothing ever connects to
        // this path.
        let path = unique_sock("c4-7b-mutation");
        let generation = running_generation(path);
        let revoke_generation = Arc::clone(&generation);
        // Reproduce exactly what `exchange_once` does up to (and excluding)
        // the local flag, to observe `Generation`'s own view in isolation.
        let in_flight = generation
            .admit_request(
                "raw".to_owned(),
                sha256(b"tok"),
                Instant::now(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(in_flight.dispatch(), "first dispatch(), before the guard");
        assert!(revoke_generation.revoke().is_some());
        // The guard's own dispatch() (what `exchange`'s `send_guard` would
        // call) now reports `false` — revoked — matching this module's local
        // flag staying `false`. But `Generation`'s bookkeeping already has
        // `dispatched` (the id is in the `dispatched` set) from the FIRST
        // call above, which is exactly why `Abandoned.dispatched` alone
        // (without this module's own flag) would read `true` here.
        assert!(
            !in_flight.dispatch(),
            "the guard's own check must now see revoked"
        );
        match in_flight.finish() {
            Err(Abandoned { dispatched }) => {
                assert!(
                    dispatched,
                    "Generation's raw bookkeeping says dispatched:true for this scenario — \
                     which is exactly the value send_kernel_request's own flag must NOT reuse"
                );
            }
            Ok(()) => panic!("expected Abandoned after revoke()"),
        }
    }

    /// design v3 (H-A), judgement **C4.9** (the design's own headline
    /// scenario): the module reads the ENTIRE request, THEN the generation is
    /// revoked while it sits there before answering. The local flag was set
    /// the moment the send guard let the send through, so this must be
    /// `Abandoned{dispatched:true}` (→ `Failed` in `classify`), never
    /// `Deferred`.
    #[tokio::test]
    async fn revoked_after_the_module_fully_read_the_request_is_dispatched_true() {
        let path = unique_sock("c4-9");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (read_tx, read_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(idx) = text.find("\r\n\r\n") {
                    let head = text[..idx].to_owned();
                    let content_length: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().to_owned())
                        })
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    if buf.len() >= idx + 4 + content_length {
                        break;
                    }
                }
            }
            // Tell the test "I have read the complete request" and then hold
            // the connection open (never answer) until the test drops it.
            let _ = read_tx.send(());
            std::future::pending::<()>().await;
        });
        let generation = running_generation(path);
        let revoke_generation = Arc::clone(&generation);
        let request_task = tokio::spawn(async move {
            send_kernel_request(&revoke_generation, &ids(), fired_request(), limits(), None).await
        });
        tokio::time::timeout(Duration::from_secs(5), read_rx)
            .await
            .expect("the module never finished reading the request")
            .unwrap();
        assert!(generation.revoke().is_some());
        let result = tokio::time::timeout(Duration::from_secs(5), request_task)
            .await
            .expect("send_kernel_request must return promptly once revoked")
            .unwrap();
        match result {
            Err(KernelCallError::Abandoned(Abandoned { dispatched })) => {
                assert!(dispatched, "C4.9 must record dispatched:true");
            }
            other => panic!("expected Abandoned{{dispatched:true}}, got {other:?}"),
        }
    }

    /// Review round 1, **M5**, judgement **C4.6**: the request id
    /// `send_kernel_request` mints is a REAL in-flight request of its
    /// `generation` for exactly as long as the call runs — so a callback
    /// carrying it passes `admit_callback_bound` even once the generation
    /// starts Draining mid-delivery, a random id does not, and once the
    /// delivery has ended the SAME id no longer does either (design §5.2's
    /// own claim, quoted in its doc comment).
    #[tokio::test]
    async fn the_delivery_request_id_is_bound_while_draining_and_unbound_once_it_ends() {
        let path = unique_sock("c4-6");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (read_tx, read_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(idx) = text.find("\r\n\r\n") {
                    let head = text[..idx].to_owned();
                    let content_length: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().to_owned())
                        })
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    if buf.len() >= idx + 4 + content_length {
                        break;
                    }
                }
            }
            // Tell the test "the request is fully in" and then wait for the
            // test's go-ahead before answering — so the delivery is
            // genuinely still in flight while the test drives the generation
            // into Draining and probes `admit_callback_bound`.
            let _ = read_tx.send(());
            let _ = release_rx.await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
        });
        let generation = running_generation(path);
        let call_generation = Arc::clone(&generation);
        // `ids()` mints a fresh `KernelRequestIds` whose first id is always
        // `sch-aaaaaaaa-0` (its counter starts at zero) — this is the only
        // call made against it, so that is the exact id this delivery uses.
        let bound_id = "sch-aaaaaaaa-0";
        let call = tokio::spawn(async move {
            send_kernel_request(&call_generation, &ids(), fired_request(), limits(), None).await
        });
        tokio::time::timeout(Duration::from_secs(5), read_rx)
            .await
            .expect("the module never finished reading the request")
            .unwrap();

        assert!(generation.begin_drain(Instant::now(), Duration::from_secs(30)));
        assert!(
            generation.admit_callback_bound(Some(bound_id)).is_ok(),
            "the in-flight delivery's own request id must still be admitted while Draining"
        );
        assert_eq!(
            generation.admit_callback_bound(Some("forged-id")).err(),
            Some(CallbackRefused::DrainingUnknownRequest),
            "a random id must never be admitted, Draining or not"
        );

        let _ = release_tx.send(());
        let result = tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .expect("send_kernel_request must finish once the module answers")
            .unwrap();
        assert!(
            matches!(result, Ok(KernelResponse { status }) if status.is_success()),
            "{result:?}"
        );

        // The delivery has ended (`InFlight::finish` ran) — the exact same id
        // must not be admitted any more.
        assert_eq!(
            generation.admit_callback_bound(Some(bound_id)).err(),
            Some(CallbackRefused::DrainingUnknownRequest),
            "the id must stop being bound the moment the delivery ends"
        );
    }

    /// design v2 (L2), judgement **C4.13**: a 2xx head is the acknowledgement
    /// even when the body that follows blows past the response cap.
    #[tokio::test]
    async fn a_2xx_head_is_delivered_even_with_an_oversized_body() {
        let path = unique_sock("c4-13-ok");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await; // drain the request, don't parse it
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 200000\r\n\r\n")
                .await;
            let chunk = vec![b'x'; 200_000];
            let _ = socket.write_all(&chunk).await;
        });
        let generation = running_generation(path);
        let result = send_kernel_request(
            &generation,
            &ids(),
            fired_request(),
            short_limits(Duration::from_secs(5)),
            None,
        )
        .await;
        assert!(
            matches!(result, Ok(KernelResponse { status }) if status.is_success()),
            "{result:?}"
        );
    }

    /// The negative control for C4.13: a NON-2xx head with the same oversized
    /// body is a real failure (`ResponseTooLarge`), not silently swallowed.
    #[tokio::test]
    async fn a_non_2xx_head_with_an_oversized_body_is_response_too_large() {
        let path = unique_sock("c4-13-fail");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 200000\r\n\r\n")
                .await;
            let chunk = vec![b'x'; 200_000];
            let _ = socket.write_all(&chunk).await;
        });
        let generation = running_generation(path);
        let result = send_kernel_request(
            &generation,
            &ids(),
            fired_request(),
            short_limits(Duration::from_secs(5)),
            None,
        )
        .await;
        assert!(
            matches!(result, Err(KernelCallError::ResponseTooLarge)),
            "{result:?}"
        );
    }

    /// Review round 1, **L3**: a non-2xx response whose body ends early for a
    /// reason that is NOT the size limit (the module promises more bytes via
    /// `Content-Length` than it ever sends, then hangs up) must be reported
    /// as `ResponseBodyError`, never `ResponseTooLarge` — the body here never
    /// gets anywhere near `limits.max_response_bytes`.
    #[tokio::test]
    async fn a_non_2xx_body_that_ends_early_for_a_non_size_reason_is_not_reported_as_too_large() {
        let path = unique_sock("l3-body-error");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            // Promises 1000 bytes, sends 10, then hangs up — well under the
            // response cap, so a length-limit read can never be the cause.
            let _ = socket
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 1000\r\n\r\n0123456789",
                )
                .await;
        });
        let generation = running_generation(path);
        let result = send_kernel_request(
            &generation,
            &ids(),
            fired_request(),
            short_limits(Duration::from_secs(5)),
            None,
        )
        .await;
        match result {
            Err(KernelCallError::ResponseBodyError(msg)) => {
                assert!(!msg.is_empty(), "the body-read error must say something");
            }
            other => panic!("expected ResponseBodyError, got {other:?}"),
        }
    }

    /// A module that never answers hits the injected (short) timeout without
    /// hanging the caller — the pump-facing half of design's C4.5.
    #[tokio::test]
    async fn a_silent_module_times_out_under_the_injected_limit() {
        let path = unique_sock("timeout");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            std::future::pending::<()>().await
        });
        let generation = running_generation(path);
        let started = Instant::now();
        let result = send_kernel_request(
            &generation,
            &ids(),
            fired_request(),
            short_limits(Duration::from_millis(300)),
            None,
        )
        .await;
        assert!(
            matches!(result, Err(KernelCallError::Timeout)),
            "{result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the timeout must be bounded by the injected limit, not a real multi-second wait"
        );
    }

    /// A module that answers non-2xx is a `Failed`-shaped transport result
    /// (not `Deferred`, not swallowed) — `agent24d::scheduler_deliver::
    /// classify` turns this into `FireOutcome::Failed`.
    #[tokio::test]
    async fn a_500_response_is_reported_as_a_plain_ok_with_a_non_success_status() {
        let (path, _rx) = raw_upstream_capturing(
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        let generation = running_generation(path);
        let result =
            send_kernel_request(&generation, &ids(), fired_request(), limits(), None).await;
        match result {
            Ok(KernelResponse { status }) => assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR),
            other => panic!("expected Ok(500), got {other:?}"),
        }
    }

    /// Same-second retries of one fire carry the exact same `x-a24-fire-id`
    /// and body (design §4.2/§5.3 — every field is read back off the
    /// delivery row, never re-derived per attempt); this module's own
    /// contribution to that guarantee is simply that it forwards whatever
    /// `KernelRequest` it is given unchanged, which this pins directly.
    #[tokio::test]
    async fn retried_headers_are_forwarded_byte_for_byte() {
        let (path, rx) = raw_upstream_capturing(
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        let generation = running_generation(path);
        let _ = send_kernel_request(&generation, &ids(), fired_request(), limits(), None).await;
        let (head, body) = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .unwrap()
            .unwrap();
        assert!(
            head.to_ascii_lowercase()
                .contains("x-a24-fire-id: fire_test")
        );
        assert_eq!(body, b"{\"key\":\"k\"}");

        let (path2, rx2) = raw_upstream_capturing(
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        let generation2 = running_generation(path2);
        let _ = send_kernel_request(&generation2, &ids(), fired_request(), limits(), None).await;
        let (head2, body2) = tokio::time::timeout(Duration::from_secs(5), rx2)
            .await
            .unwrap()
            .unwrap();
        assert!(
            head2
                .to_ascii_lowercase()
                .contains("x-a24-fire-id: fire_test")
        );
        assert_eq!(body2, body);
    }
}
