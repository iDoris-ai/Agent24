//! ME-3b-4 — the constrained proxy (SPEC-ME3-OUT-OF-PROCESS §2).
//!
//! An in-process module returns an `axum::Router` and the kernel nests it. That
//! shape does not cross a process boundary, but its EFFECT does: the module
//! serves HTTP on a socket the kernel opened, and the kernel forwards its own
//! namespace to it.
//!
//! **Forwarding is not proxying.** The kernel authenticates with
//! `Authorization: Bearer` (`agent24d::server::auth`). A verbatim forward hands
//! that bearer to the module, which can then call every kernel API rather than
//! only its own namespace — the exact opposite of "a module never sees kernel
//! credentials". So both directions are filtered, and this module is that filter.
//!
//! # What is load-bearing here, and what is only a naming convention
//!
//! Two of the three rules below are structural; one rests on a convention, and
//! saying which is which is the point of this paragraph.
//!
//! - **The upstream is a [`SocketAddr`], not a URL.** A module therefore has no
//!   way to name a host — the local-SSRF pivot §1 refuses to allow is not
//!   defended against here, it is unrepresentable. Structural.
//! - **Every `X-A24-*` header is dropped in BOTH directions**, then the kernel
//!   writes its own. A client cannot forge a lease and a module cannot echo one
//!   back to the client. Structural, for headers under that prefix.
//! - **Other kernel-private headers are dropped by an explicit list**
//!   ([`strips_from_request`]). Nothing makes that automatic. **So a
//!   kernel-private header MUST be named `X-A24-*`** — that convention, and not
//!   this code, is what keeps a header added next year from reaching modules.
//!   Written here because the person adding it will be reading `server.rs`, not
//!   this file.
//!
//! # Not in this slice
//!
//! No subprocess: the upstream is an address, so this whole slice is testable
//! against a mock and lands independently of ME-3b-3. The
//! `X-A24-Approval-Token` / `X-A24-Request-Lease` injections need ME-3e and a
//! lease table that does not exist; what IS here for those two is the stripping,
//! so they can never arrive from a client and can never leave through a module,
//! before either is ever minted.
//!
//! The concurrency ceiling and both deadlines (§5) ARE here, and deliberately —
//! an earlier draft of this comment deferred them to the supervisor. They do not
//! need one: nothing about "how many requests may this module be handling"
//! needs to know when the process started. What DOES need the supervisor is
//! DRAINING (§4, ME-3b-5), which is why 503 carries a `code` rather than a
//! single meaning.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, Full, Limited};

use agent24_domain::http::{MAX_BODY_BYTES, error_response, read_body_or_response};

/// The non-secret correlation id the kernel injects on every proxied request.
pub const REQUEST_ID_HEADER: &str = "x-a24-request-id";

/// The reserved prefix. Everything under it is kernel-written, in both
/// directions — see the module docs.
pub const A24_HEADER_PREFIX: &str = "x-a24-";

/// The proxy-provenance family, stripped inbound as a whole.
///
/// See [`strips_from_request`] for why a prefix is right here and a list is not.
pub const X_FORWARDED_PREFIX: &str = "x-forwarded-";

/// Total time one proxied request may take — reading the CLIENT's body through
/// the module's last byte.
///
/// Constants rather than settings, per §8: a config knob nobody turns is an
/// untested branch. They are printed into the failure text instead, which is the
/// half of configurability that actually gets used. Tests shorten them through a
/// private field, so the numbers stay out of the public surface while the 504
/// path stays reachable by a test — an unreachable branch and a wrong one read
/// the same from outside.
pub const UPSTREAM_DEADLINE: Duration = Duration::from_secs(30);

/// How long the module may take to produce a response HEAD.
///
/// §5 asks for a first-byte limit as well as a total one, and they answer
/// different questions: a module that accepts a connection and says nothing is
/// wedged, while one that is still streaming a large body is working. Without
/// this, the two are the same reading 30 seconds later.
pub const UPSTREAM_HEAD_DEADLINE: Duration = Duration::from_secs(10);

/// How many proxied requests one module may be handling at once.
///
/// This is what keeps "a module hangs" from becoming "the daemon is out of
/// memory": every in-flight request holds its buffered body, so without a
/// ceiling the bound on the daemon's memory is the client's patience. §8 claims
/// a module cannot take the kernel down with it; this constant is a large part
/// of what makes that claim true rather than a hope.
pub const MAX_INFLIGHT_PER_MODULE: usize = 64;

/// Hop-by-hop headers, plus the two the proxy must recompute.
///
/// `content-length` is in here because the body is rebuilt: forwarding the
/// original length beside a body of a different size is a desync, not a header
/// leak, and it is the kind that a proxy specifically invites.
fn is_hop_by_hop_or_recomputed(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "proxy-authenticate"
            | "proxy-authorization"
            // Non-standard, but honoured by enough intermediaries that leaving
            // it through means leaving a hop-by-hop header through.
            | "proxy-connection"
            | "content-length"
    )
}

/// True when this header must never reach the module.
///
/// See the module docs for why this list is the weak half of the design and
/// `X-A24-*` is the strong half.
pub fn strips_from_request(name: &str) -> bool {
    is_hop_by_hop_or_recomputed(name)
        || name.starts_with(A24_HEADER_PREFIX)
        // The whole `x-forwarded-` family, by PREFIX rather than by name.
        //
        // This file's own argument is that a prefix keeps up on its own and a
        // list does not — and it applies here too. A list of the obvious three
        // misses `X-Forwarded-Ssl` and `-Scheme` (Rack reads both for
        // `Request#ssl?`, so a module would believe the connection was TLS) and
        // `-Port` (Symfony's trusted-proxy path builds links from it).
        //
        // The prefix is safe here in a way a broad denylist would not be, and
        // THAT is the part worth writing down: **the kernel is the only hop**,
        // so no legitimate `x-forwarded-*` can exist on a request reaching a
        // module. There is nothing for the prefix to over-catch. The cost is the
        // same one `X-A24-*` carries — a module must not name its own header
        // `x-forwarded-something` — and that is a rule a person can follow.
        || name.starts_with(X_FORWARDED_PREFIX)
        || matches!(
            name,
            // Kernel credentials. The whole reason this file exists.
            "authorization" | "cookie"
            // Rewritten: the upstream is a different authority.
            | "host"
            // We buffer the body, so a 100-continue dance would be answered by
            // the module for a body it is not the one reading.
            | "expect"
            // A client can send these itself and the kernel does not rewrite
            // them, so a module that rate-limits on `X-Forwarded-For`, or builds
            // a link from `X-Forwarded-Host`, is trusting a value the caller
            // chose. It is also the asymmetry `host` above would otherwise
            // leave: the real authority hidden, a forged one not. Dropped rather
            // than minted — the kernel HAS the peer address, but nothing has
            // asked for it, and minting a header no module reads is inventing a
            // contract (FU-45).
            | "forwarded"
            | "x-real-ip"
        )
}

/// True when this header must never reach the client.
pub fn strips_from_response(name: &str) -> bool {
    is_hop_by_hop_or_recomputed(name)
        // A module echoing a lease or an approval token back would send a kernel
        // secret to the caller. Prefix-wide, so it holds for headers not minted
        // yet.
        || name.starts_with(A24_HEADER_PREFIX)
        || matches!(
            name,
            // A module must not set state in the kernel's cookie jar, nor make
            // the kernel's surface look like it is challenging for credentials
            // — nor look like it is CONTINUING an authentication exchange,
            // which is what the `*-authentication-info` pair does.
            "set-cookie"
                | "www-authenticate"
                | "authorization"
                | "authentication-info"
                | "proxy-authentication-info"
                // `Refresh: 0; url=http://evil.example` is non-standard and
                // honoured by every major browser — the header form of
                // `<meta http-equiv="refresh">`. It navigates, so it does
                // `Location`'s job while going nowhere near `location_within`:
                // no scheme check, no `//host`, no `%2e`, no tab, no duplicate
                // rule. **A rule that names one header is not a rule about
                // redirection.**
                //
                // Dropped rather than checked: checking it would amount to
                // supporting it as a redirect mechanism, and `Location` already
                // covers every legitimate use.
                | "refresh"
        )
}

/// The header names a `Connection:` value nominates as hop-by-hop.
///
/// Without this, `Connection: x-secret` + `X-Secret: …` is a documented way to
/// smuggle a header past a filter that only knows the fixed list.
/// Parsed from BYTES, not from `to_str()`.
///
/// `Connection: x-hop,\x80` is a header hyper accepts and `to_str()` rejects. A
/// parser that skips the whole value on a UTF-8 error therefore drops the
/// `Connection` header itself while forwarding the `X-Hop` it nominated — one
/// invalid byte disables the filter for every token beside it. Splitting on
/// bytes makes the valid tokens survive the invalid one.
fn connection_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .flat_map(|v| v.as_bytes().split(|b| *b == b','))
        .filter_map(|token| {
            let trimmed = trim_ascii_whitespace(token);
            if trimmed.is_empty() || !trimmed.is_ascii() {
                return None;
            }
            std::str::from_utf8(trimmed)
                .ok()
                .map(|s| s.to_ascii_lowercase())
        })
        .collect()
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while let Some((first, rest)) = bytes.split_first() {
        if !first.is_ascii_whitespace() {
            break;
        }
        bytes = rest;
    }
    while let Some((last, rest)) = bytes.split_last() {
        if !last.is_ascii_whitespace() {
            break;
        }
        bytes = rest;
    }
    bytes
}

fn sanitize(headers: &HeaderMap, strip: fn(&str) -> bool) -> HeaderMap {
    let nominated = connection_tokens(headers);
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let n = name.as_str();
        if strip(n) || nominated.iter().any(|t| t == n) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// The headers a module is allowed to see, given what a client sent.
///
/// Does NOT add the injected `X-A24-*` headers — that is the proxy's job, and
/// keeping it separate is what lets this be tested as a pure function.
pub fn sanitize_request_headers(from_client: &HeaderMap) -> HeaderMap {
    sanitize(from_client, strips_from_request)
}

/// The headers a client is allowed to see, given what a module returned.
pub fn sanitize_response_headers(from_module: &HeaderMap) -> HeaderMap {
    sanitize(from_module, strips_from_response)
}

/// Whether a `Location` a module returned points inside its own namespace.
///
/// The danger §2 names is not that the KERNEL would follow it — it would not,
/// and this proxy never does. It is that the kernel would hand it to a browser,
/// which would. So the check is on what leaves, and it applies at ANY status:
/// a `Location` on a `201` redirects nothing but is handed over just the same.
///
/// # Deliberately conservative, in three places
///
/// - **Any scheme, and any protocol-relative `//host`, is refused** — including
///   an absolute URL that happens to name this same daemon. Deciding whether
///   `http://127.0.0.1:8080/api/v1/ns/x` is "us" means knowing the daemon's own
///   external authority, which behind a reverse proxy it does not.
/// - **`%2e` is treated as `.` before normalising.** Browsers decode it, so
///   `/api/v1/ns/%2e%2e/%2e%2e/admin` escapes the namespace in the only place
///   that matters. A module that genuinely wants a literal `%2e` in a path
///   segment loses; that trade is worth naming and it is this one.
/// - **A backslash anywhere is refused.** Several browsers fold `\` to `/`, so
///   `/\evil.example` reads as `//evil.example` to them.
///
/// # What it cannot do
///
/// Nothing here stops a module redirecting through its own BODY — a meta
/// refresh, a line of script. That is not a hole being left open by accident:
/// the module owns its namespace's content, and a rule about the `Location`
/// header is about the header, not about making redirection impossible. What it
/// does buy is that the kernel's own surface never carries the redirect.
///
/// **That last sentence was false when it was first written**, and the header
/// that made it false was `Refresh` — same navigation, same arbitrary URL, and
/// none of this function. It is dropped outright by [`strips_from_response`],
/// which is what makes the sentence true. The lesson is the shape rather than
/// that one header: **this function is a rule about `Location`, and the claim
/// above is about REDIRECTION.** Anything else that navigates has to be handled
/// over there, not here.
pub fn location_within(namespace: &str, request_path: &str, location: &str) -> bool {
    // Browsers DELETE ASCII tab and newline while parsing a URL, so
    // `/api/v1/<ns>/\t..\t/runs` is inside the namespace to this function and
    // `/api/v1/runs` to the thing that acts on it — and `h\tttp://evil.example`
    // is a relative path here and an absolute URL there. Any character a URL
    // parser would drop or that cannot appear in one is refused outright: it
    // means the string this function judges is not the string the client
    // resolves, which is the one defect this whole file keeps meeting.
    if location
        .chars()
        .any(|c| c.is_control() || c.is_whitespace())
    {
        return false;
    }
    if location.contains('\\') || location.starts_with("//") || has_scheme(location) {
        return false;
    }
    let path = location.split(['?', '#']).next().unwrap_or("");
    if path.is_empty() {
        // RFC 3986 §5.3: a reference with no path keeps the base's path exactly
        // — `?page=2` means "this same resource, other query". Resolving it
        // against the base DIRECTORY instead drops the last segment, so
        // `/api/v1/<ns>` + `?page=2` came out as `/api/v1/`, which is outside
        // the namespace. The request reached this proxy through the namespace,
        // so its own path is inside it by construction.
        return true;
    }
    let decoded = path.replace("%2e", ".").replace("%2E", ".");

    let absolute = if decoded.starts_with('/') {
        decoded
    } else {
        // A relative reference resolves against the directory of the request
        // path (RFC 3986 §5.3), NOT against the namespace root — resolving it
        // against the root would silently accept a module that meant to walk up.
        let base = match request_path.rfind('/') {
            Some(i) => &request_path[..=i],
            None => "/",
        };
        format!("{base}{decoded}")
    };

    let Some(normalised) = normalise_dot_segments(&absolute) else {
        // `..` walked above the root. Not "outside the namespace" so much as
        // nonsense, and nonsense is refused rather than clamped.
        return false;
    };
    normalised == namespace || normalised.starts_with(&format!("{namespace}/"))
}

/// True when the reference begins with a URI scheme (`scheme:`), per RFC 3986.
fn has_scheme(reference: &str) -> bool {
    let Some(colon) = reference.find(':') else {
        return false;
    };
    // A colon after the first `/`, `?` or `#` belongs to the path, not a scheme:
    // `/a:b` is a path, `mailto:x` is not.
    if reference[..colon].contains(['/', '?', '#']) {
        return false;
    }
    let mut chars = reference[..colon].chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Collapse `.` and `..`; `None` when `..` escapes the root.
fn normalise_dot_segments(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(format!("/{}", out.join("/")))
}

/// Everything one namespace's proxy needs. Cheap to clone (all `Arc` inside).
#[derive(Clone)]
struct ProxyState {
    namespace: Arc<String>,
    upstream: SocketAddr,
    client: Client,
    ids: Arc<RequestIds>,
    limits: Limits,
    /// The ceiling on concurrent proxied requests for THIS module. One per
    /// module, not one per daemon: a wedged module must degrade its own
    /// namespace, not everyone else's.
    inflight: Arc<tokio::sync::Semaphore>,
    /// How many that is. Carried rather than read back from the constant, for
    /// the same reason `TimedOut` carries its budget: the message has to name
    /// the limit that ACTUALLY applies. Today the two are equal in production,
    /// so the constant is not yet wrong — only unguarded, and the day this
    /// becomes per-module the text starts lying with nothing to catch it.
    capacity: usize,
}

/// Deadlines, as data rather than as constants read at the call site.
///
/// Production always uses [`Limits::default`], which is the constants. The
/// indirection buys one thing: a test can reach the 504 branch in milliseconds.
/// Without it that branch is unreachable from any test, and an unreachable
/// branch and a wrong one look the same from outside — the `502`/`504` split
/// §2.1 pins would be a claim with nothing holding it.
#[derive(Clone, Copy)]
struct Limits {
    total: Duration,
    head: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            total: UPSTREAM_DEADLINE,
            head: UPSTREAM_HEAD_DEADLINE,
        }
    }
}

type Client = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    Full<Bytes>,
>;

/// Correlation ids: a per-daemon random prefix and a counter.
///
/// **Not a secret and not treated as one.** §2 splits the correlation id from
/// the approval token precisely so that the loggable half can be cheap. The
/// prefix exists so ids from two daemon lifetimes do not collide in a log, not
/// to make them unguessable — the thing that must be unguessable is
/// `X-A24-Approval-Token`, which is minted elsewhere (ME-3e) and never here.
struct RequestIds {
    prefix: String,
    next: AtomicU64,
}

impl RequestIds {
    fn new() -> Self {
        Self {
            prefix: short_prefix(),
            next: AtomicU64::new(0),
        }
    }

    fn mint(&self) -> String {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        format!("{}-{n}", self.prefix)
    }
}

/// Eight hex characters of system entropy, or a time-based stand-in.
///
/// The fallback is acceptable HERE and would not be for a token: the only cost
/// of a repeated prefix is two log lines that look related. `launch::mint_token`
/// refuses to fall back for the opposite reason, and the two behaving
/// differently is deliberate rather than an oversight.
fn short_prefix() -> String {
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

/// The router that proxies one namespace to one module.
///
/// It is a bare fallback, because a module owns every path under its namespace
/// and the kernel does not know which ones exist. Mount it with [`mount`]
/// rather than nesting it directly: `nest` does not cover the bare trailing
/// slash (matchit's `{*rest}` will not match an empty segment), and that rule
/// belongs in one place.
pub fn proxy_router(namespace: &str, upstream: SocketAddr) -> Router {
    Router::new()
        .fallback(proxy)
        .with_state(state_for(namespace, upstream))
}

/// Nest [`proxy_router`] under `namespace`, trailing slash included.
///
/// One [`ProxyState`], shared by both routes, so the two cannot drift — in
/// particular so `/api/v1/ns/` and `/api/v1/ns/x` mint request ids from the
/// same sequence rather than from two that look unrelated in a log.
pub fn mount(app: Router, namespace: &str, upstream: SocketAddr) -> Router {
    let state = state_for(namespace, upstream);
    app.nest(
        namespace,
        Router::new().fallback(proxy).with_state(state.clone()),
    )
    .route(
        &format!("{namespace}/"),
        axum::routing::any(proxy).with_state(state),
    )
}

fn state_for(namespace: &str, upstream: SocketAddr) -> ProxyState {
    state_with(
        namespace,
        upstream,
        Limits::default(),
        MAX_INFLIGHT_PER_MODULE,
    )
}

fn state_with(
    namespace: &str,
    upstream: SocketAddr,
    limits: Limits,
    inflight: usize,
) -> ProxyState {
    ProxyState {
        namespace: Arc::new(namespace.to_owned()),
        upstream,
        client: hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http(),
        ids: Arc::new(RequestIds::new()),
        limits,
        inflight: Arc::new(tokio::sync::Semaphore::new(inflight)),
        capacity: inflight,
    }
}

async fn proxy(
    State(state): State<ProxyState>,
    OriginalUri(original): OriginalUri,
    request: Request<Body>,
) -> Response {
    // The deadline starts HERE, not at the upstream call. Reading the client's
    // body is time this handler spends holding memory, and a client that dribbles
    // a chunked body one byte at a time is as good a way to pin the daemon as a
    // wedged module is. A timeout that starts after the body is read cannot see
    // that at all.
    let deadline = tokio::time::Instant::now() + state.limits.total;

    // Refused rather than queued: a queue is memory the caller controls, which
    // is the thing being bounded.
    let Ok(permit) = state.inflight.clone().try_acquire_owned() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_overloaded",
            &format!(
                "the module already has {} requests in flight",
                state.capacity
            ),
        );
    };

    let method = request.method().clone();
    let from_client = request.headers().clone();

    // Read the body under the kernel's own cap: a module must not be the thing
    // that decides how much of the daemon's memory an upload gets.
    let body = match tokio::time::timeout_at(deadline, read_body_or_response(request)).await {
        Err(_) => return timed_out(&state, TimedOut::ClientBody),
        Ok(Err(response)) => return response,
        Ok(Ok(b)) => b,
    };

    let mut headers = sanitize_request_headers(&from_client);
    let request_id = state.ids.mint();
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        headers.insert(HeaderName::from_static(REQUEST_ID_HEADER), value);
    }

    let path_and_query = original
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let upstream_uri = match Uri::builder()
        .scheme("http")
        .authority(state.upstream.to_string())
        .path_and_query(path_and_query)
        .build()
    {
        Ok(u) => u,
        Err(e) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                &format!("the proxied path could not be reconstructed: {e}"),
            );
        }
    };

    let mut upstream_request = match Request::builder()
        .method(method)
        .uri(upstream_uri)
        .body(Full::new(body))
    {
        Ok(r) => r,
        // Not `unwrap_or_default()`: the default is a GET of `/` with an empty
        // body, which would proxy a DIFFERENT request rather than fail one.
        Err(e) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                &format!("the proxied request could not be rebuilt: {e}"),
            );
        }
    };
    *upstream_request.headers_mut() = headers;

    // Two deadlines, because they answer different questions (§5): a module that
    // has not produced a response HEAD is wedged, while one still sending a body
    // is working. Whichever expires first wins.
    let now = tokio::time::Instant::now();
    let head_deadline = deadline.min(now + state.limits.head);
    // Whichever of the two actually applies — otherwise a total deadline shorter
    // than the head one reports "within 10s" for something that gave up after
    // one. A message that names the wrong limit sends the reader to the wrong
    // knob.
    let head_budget = head_deadline.duration_since(now);
    let response = match tokio::time::timeout_at(
        head_deadline,
        state.client.request(upstream_request),
    )
    .await
    {
        Err(_) => return timed_out(&state, TimedOut::UpstreamHead(head_budget)),
        Ok(Err(e)) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                &format!("the module could not be reached: {e}"),
            );
        }
        Ok(Ok(r)) => r,
    };

    let (parts, upstream_body) = response.into_parts();

    // **Judge the head BEFORE collecting the body.** An endless SSE stream is
    // exactly the response this refuses, and a refusal that happens after
    // buffering it answers `504 upstream_timeout` — the code for "the module was
    // slow", about a module that was not slow at all. The check has to sit
    // upstream of the thing it exists to prevent.
    if let Some(refusal) = head_refusal(&state.namespace, original.path(), &parts) {
        return refusal;
    }

    let collected = match tokio::time::timeout_at(
        deadline,
        Limited::new(upstream_body, MAX_BODY_BYTES).collect(),
    )
    .await
    {
        Err(_) => return timed_out(&state, TimedOut::UpstreamBody),
        Ok(Ok(c)) => c.to_bytes(),
        Ok(Err(e)) => {
            return if is_length_limit(&*e) {
                error_response(
                    StatusCode::BAD_GATEWAY,
                    "upstream_response_too_large",
                    &format!("the module's response exceeded {MAX_BODY_BYTES} bytes"),
                )
            } else {
                // The upstream vanished PART WAY THROUGH. Answering 502 rather
                // than the status we already have is the whole point: the
                // headers said 200, and forwarding them with a truncated body
                // reports success for a response that never finished.
                error_response(
                    StatusCode::BAD_GATEWAY,
                    "upstream_unavailable",
                    &format!("the module's response ended early: {e}"),
                )
            };
        }
    };

    // The permit rides with the BYTES.
    //
    // Two versions of this were wrong before this one, in the same direction
    // each time: the permit was released while the memory it was supposed to be
    // counting was still held.
    //
    //   1. Released when the handler returned — but the collected bytes are then
    //      being written to a client that may read slowly, or not at all.
    //   2. Released when the response BODY was dropped — but a body hands its
    //      `Bytes` out in a frame, and hyper holds that frame in its write queue
    //      after dropping the body. The permit went back while the megabyte was
    //      still queued.
    //
    // `Bytes::from_owner` ends the regress: the permit lives inside the
    // allocation, so it is returned when the LAST clone of these bytes is
    // dropped — which is the moment the memory is actually gone, and is not a
    // moment any code here has to remember to name.
    let mut response = Response::new(Body::from(Bytes::from_owner(PermitBytes {
        data: collected,
        _permit: permit,
    })));
    *response.status_mut() = parts.status;
    *response.headers_mut() = sanitize_response_headers(&parts.headers);
    response.into_response()
}

/// Bytes that hold a concurrency permit for as long as they exist.
///
/// Handed to [`Bytes::from_owner`], so every clone hyper makes keeps the permit
/// alive and the last one to be dropped returns it. That is what makes the
/// ceiling a bound on MEMORY rather than on handler count — see the call site
/// for the two earlier versions that bounded neither.
struct PermitBytes {
    data: Bytes,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl AsRef<[u8]> for PermitBytes {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

/// Which deadline expired. §2.1 says the two answer different questions, so one
/// shared message makes the answer unreadable — it names the wrong limit and,
/// for a stalled client, blames the wrong party.
enum TimedOut {
    /// The client never finished sending its request.
    ClientBody,
    /// The module accepted the request and produced no response head, within
    /// the budget that actually applied (the head limit, or what was left of the
    /// total — whichever was shorter).
    UpstreamHead(Duration),
    /// The module started answering and did not finish.
    UpstreamBody,
}

fn timed_out(state: &ProxyState, which: TimedOut) -> Response {
    let message = match which {
        TimedOut::ClientBody => format!(
            "the request body was not received within {}s",
            state.limits.total.as_secs()
        ),
        TimedOut::UpstreamHead(budget) => format!(
            "the module did not begin answering within {:.1}s",
            budget.as_secs_f32()
        ),
        TimedOut::UpstreamBody => format!(
            "the module did not finish answering within {}s",
            state.limits.total.as_secs()
        ),
    };
    error_response(StatusCode::GATEWAY_TIMEOUT, "upstream_timeout", &message)
}

fn is_length_limit(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(e);
    while let Some(err) = source {
        if err.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        source = err.source();
    }
    false
}

/// Everything about an upstream response that can be judged from its HEAD.
///
/// `Some(response)` means the client gets that instead. Kept as one function so
/// that "what can be decided before reading the body" is a list somebody can
/// read, rather than a property of where the calls happen to sit.
fn head_refusal(
    namespace: &str,
    request_path: &str,
    parts: &axum::http::response::Parts,
) -> Option<Response> {
    // §9 refuses streaming this round, and refuses it EXPLICITLY rather than by
    // quietly buffering: a module that believes it is streaming and is in fact
    // being buffered behaves worse than one told plainly that it cannot.
    if parts.status == StatusCode::SWITCHING_PROTOCOLS {
        return Some(error_response(
            StatusCode::BAD_GATEWAY,
            "upstream_streaming_unsupported",
            "protocol upgrades (WebSocket) are not proxied this round",
        ));
    }
    // EVERY content-type value, and the MEDIA TYPE of each rather than a prefix
    // of the header. `Text/Event-Stream` is the same media type as
    // `text/event-stream`; `text/event-streaming` is a different one, and
    // refusing a legitimate response is as much a bug as forwarding a stream. A
    // module that sends two content-types is not one whose first value should
    // decide the question either.
    // Compared on BYTES, for the same reason `connection_tokens` is: a
    // `Content-Type: text/event-stream; x="<non-ASCII>"` makes `to_str()` fail,
    // and a check that treats that as "not a stream" lets the exact response it
    // exists to refuse through — on a parameter the module chooses.
    if parts.headers.get_all(header::CONTENT_TYPE).iter().any(|v| {
        let media = trim_ascii_whitespace(
            v.as_bytes()
                .split(|b| *b == b';')
                .next()
                .unwrap_or_default(),
        );
        media.eq_ignore_ascii_case(b"text/event-stream")
    }) {
        return Some(error_response(
            StatusCode::BAD_GATEWAY,
            "upstream_streaming_unsupported",
            "server-sent events are not proxied this round",
        ));
    }

    // ALL the Location values, not the first.
    //
    // `HeaderMap::get` returns the first; `sanitize_response_headers` forwards
    // every one. So a module that sends a harmless `Location` followed by a
    // cross-namespace one passed the check and shipped the header anyway — the
    // value that was judged was not the value that left. Clients disagree about
    // which duplicate wins, so a second one is refused even when both are
    // in-namespace: "which one did you mean" has no safe default.
    let mut locations = parts.headers.get_all(header::LOCATION).iter();
    if let Some(first) = locations.next() {
        if locations.next().is_some() {
            return Some(error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_location_rejected",
                "the module returned more than one Location header",
            ));
        }
        let ok = first
            .to_str()
            .is_ok_and(|l| location_within(namespace, request_path, l));
        if !ok {
            // The header is not merely dropped: a 302 whose Location is gone is
            // a response no client can act on, and one that silently became a
            // different thing is worse than an error.
            return Some(error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_location_rejected",
                &format!("the module redirected outside {namespace}"),
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use axum::http::Method;
    use std::sync::atomic::AtomicUsize;

    const NS: &str = "/api/v1/zzmock";

    // ── the pure half ────────────────────────────────────────────────────

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn kernel_credentials_and_forged_a24_headers_never_reach_the_module() {
        let out = sanitize_request_headers(&headers(&[
            ("authorization", "Bearer kernel-secret"),
            ("cookie", "session=1"),
            ("host", "kernel.example"),
            ("expect", "100-continue"),
            ("transfer-encoding", "chunked"),
            ("content-length", "7"),
            ("x-a24-request-id", "forged"),
            ("x-a24-request-lease", "forged"),
            ("x-a24-approval-token", "forged"),
            // The positive control. Without it "nothing got through" and
            // "the kernel's credentials got through" are the same reading, and
            // a proxy that forwards an empty header map would pass.
            ("content-type", "application/json"),
            ("accept", "application/json"),
            ("x-module-own-header", "kept"),
        ]));

        for gone in [
            "authorization",
            "cookie",
            "host",
            "expect",
            "transfer-encoding",
            "content-length",
            "x-a24-request-id",
            "x-a24-request-lease",
            "x-a24-approval-token",
        ] {
            assert!(!out.contains_key(gone), "{gone} reached the module");
        }
        assert_eq!(out.get("content-type").unwrap(), "application/json");
        assert_eq!(out.get("accept").unwrap(), "application/json");
        assert_eq!(out.get("x-module-own-header").unwrap(), "kept");
    }

    #[test]
    fn a_header_nominated_by_connection_is_stripped_too() {
        // `Connection: x-smuggled` makes that header hop-by-hop by definition.
        // A filter that only knows the fixed list forwards it, which is the
        // documented way past exactly this kind of filter.
        let out = sanitize_request_headers(&headers(&[
            ("connection", "keep-alive, x-smuggled"),
            ("x-smuggled", "value"),
            ("x-not-smuggled", "value"),
        ]));
        assert!(!out.contains_key("x-smuggled"));
        assert!(!out.contains_key("connection"));
        assert_eq!(out.get("x-not-smuggled").unwrap(), "value");
    }

    #[test]
    fn a_module_cannot_echo_kernel_headers_or_set_cookies() {
        let out = sanitize_response_headers(&headers(&[
            ("x-a24-approval-token", "stolen"),
            ("x-a24-request-lease", "stolen"),
            ("x-a24-anything-at-all", "stolen"),
            ("set-cookie", "a=1"),
            ("www-authenticate", "Bearer"),
            ("connection", "close"),
            ("content-length", "3"),
            // Positive control, same reason as above.
            ("content-type", "text/plain"),
            ("etag", "\"abc\""),
        ]));
        for gone in [
            "x-a24-approval-token",
            "x-a24-request-lease",
            "x-a24-anything-at-all",
            "set-cookie",
            "www-authenticate",
            "connection",
            "content-length",
        ] {
            assert!(!out.contains_key(gone), "{gone} reached the client");
        }
        assert_eq!(out.get("content-type").unwrap(), "text/plain");
        assert_eq!(out.get("etag").unwrap(), "\"abc\"");
    }

    #[test]
    fn location_is_confined_to_the_module_namespace() {
        let req = "/api/v1/zzmock/things/1";
        let allowed = [
            "/api/v1/zzmock/things/2",
            "/api/v1/zzmock",
            "/api/v1/zzmock/",
            "2",                    // relative to the request's directory
            "./2",                  //
            "../things/2",          // up one, back down — still inside
            "/api/v1/zzmock/x?q=1", // query is not part of the path
            "/api/v1/zzmock/x#frag",
        ];
        for l in allowed {
            assert!(location_within(NS, req, l), "{l} should be allowed");
        }

        let refused = [
            "https://evil.example/",             // absolute
            "http://127.0.0.1:8080/api/v1/runs", // absolute, even to ourselves
            "//evil.example/x",                  // protocol-relative
            "/api/v1/runs",                      // another kernel route
            "/api/v1/zzmockery/x",               // prefix without a boundary
            "/api/v1/zzmock/../runs",            // walks out
            "../../runs",                        // ditto, relative
            "/api/v1/zzmock/%2e%2e/%2e%2e/runs", // ditto, percent-encoded
            "/../../..",                         // above the root
            "/",                                 // the kernel's own root
            "\\\\evil.example\\x",               // browsers fold backslashes
            "mailto:x@example.com",              // any scheme at all
        ];
        for l in refused {
            assert!(!location_within(NS, req, l), "{l} should be refused");
        }
    }

    #[test]
    fn one_invalid_byte_in_connection_does_not_disable_the_rest_of_it() {
        // `Connection: x-hop,\x80` is a header hyper accepts. A parser that
        // gives up on the whole value when `to_str()` fails drops `Connection`
        // itself — it is on the fixed list — and forwards the `X-Hop` it
        // nominated. One byte the attacker chooses turns the filter off for the
        // token beside it.
        let mut h = HeaderMap::new();
        h.append(
            header::CONNECTION,
            HeaderValue::from_bytes(b"x-hop, \x80").unwrap(),
        );
        h.append("x-hop", HeaderValue::from_static("private"));
        h.append("x-kept", HeaderValue::from_static("fine"));
        let out = sanitize_request_headers(&h);
        assert!(!out.contains_key("x-hop"));
        assert_eq!(out.get("x-kept").unwrap(), "fine");
    }

    #[test]
    fn proxy_connection_is_hop_by_hop_in_both_directions() {
        let pair = [("proxy-connection", "keep-alive"), ("x-kept", "fine")];
        for out in [
            sanitize_request_headers(&headers(&pair)),
            sanitize_response_headers(&headers(&pair)),
        ] {
            assert!(!out.contains_key("proxy-connection"));
            assert_eq!(out.get("x-kept").unwrap(), "fine");
        }
    }

    #[test]
    fn a_module_cannot_continue_an_authentication_exchange_with_the_client() {
        let out = sanitize_response_headers(&headers(&[
            ("authentication-info", "nextnonce=\"module-controlled\""),
            ("proxy-authentication-info", "nextnonce=\"x\""),
            ("x-kept", "fine"),
        ]));
        assert!(!out.contains_key("authentication-info"));
        assert!(!out.contains_key("proxy-authentication-info"));
        assert_eq!(out.get("x-kept").unwrap(), "fine");
    }

    #[test]
    fn a_location_a_browser_would_rewrite_is_refused() {
        // Browsers DELETE ASCII tab and newline while parsing a URL. Both of
        // these are inside the namespace as written and outside it as resolved
        // — the string judged is not the string acted on.
        let req = "/api/v1/zzmock/things/1";
        for l in [
            "/api/v1/zzmock/\t..\t/runs", // resolves to /api/v1/runs
            "h\tttp://evil.example/",     // resolves to an absolute URL
            "/api/v1/zzmock/ x",          // a space is not URL material either
            "\u{0000}/api/v1/zzmock/x",
        ] {
            assert!(!location_within(NS, req, l), "{l:?} should be refused");
        }
        // Control: the same shape without the rewritten characters is fine, so
        // this is not "refuse everything".
        assert!(location_within(NS, req, "/api/v1/zzmock/x"));
    }

    // ── the wired half: a mock HTTP upstream, no subprocess ──────────────

    #[derive(Clone, Default)]
    struct Hits(Arc<AtomicUsize>);

    async fn upstream_handler(
        State(hits): State<Hits>,
        OriginalUri(uri): OriginalUri,
        method: Method,
        received: HeaderMap,
        body: Bytes,
    ) -> Response {
        hits.0.fetch_add(1, Ordering::SeqCst);
        let path = uri.path().to_owned();
        let query = uri.query().unwrap_or("").to_owned();

        if path.ends_with("/redirect") {
            let to = query.strip_prefix("to=").unwrap_or("/");
            return Response::builder()
                .status(StatusCode::FOUND)
                .header(header::LOCATION, to)
                .body(Body::empty())
                .unwrap();
        }
        if path.ends_with("/refresh") {
            return Response::builder()
                .status(StatusCode::OK)
                .header("refresh", query.strip_prefix("to=").unwrap_or("0"))
                .body(Body::from("ok"))
                .unwrap();
        }
        if path.ends_with("/echo-secrets") {
            return Response::builder()
                .status(StatusCode::OK)
                .header("x-a24-approval-token", "stolen")
                .header("x-a24-request-lease", "stolen")
                .header("x-a24-whatever", "stolen")
                .header(header::SET_COOKIE, "a=1")
                .header("x-module-own-header", "fine")
                .body(Body::from("ok"))
                .unwrap();
        }
        if path.ends_with("/big") {
            let n: usize = query
                .strip_prefix("n=")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            return Response::new(Body::from(vec![b'x'; n]));
        }
        if path.ends_with("/two-locations") {
            let mut b = Response::builder().status(StatusCode::FOUND);
            for to in query.strip_prefix("to=").unwrap_or("").split('|') {
                b = b.header(header::LOCATION, to);
            }
            return b.body(Body::empty()).unwrap();
        }
        if path.ends_with("/sse-cased") {
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "Text/Event-Stream; charset=utf-8")
                .body(Body::from("data: hi\n\n"))
                .unwrap();
        }
        if path.ends_with("/sse") {
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from("data: hi\n\n"))
                .unwrap();
        }

        // Default: report exactly what arrived.
        let seen: std::collections::BTreeMap<String, String> = received
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    v.to_str().unwrap_or("<binary>").to_owned(),
                )
            })
            .collect();
        axum::Json(serde_json::json!({
            "method": method.as_str(),
            "path": path,
            "query": query,
            "body": String::from_utf8_lossy(&body),
            "headers": seen,
        }))
        .into_response()
    }

    async fn serve(app: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        addr
    }

    /// A proxy in front of a mock module. Returns (proxy addr, upstream hits).
    async fn proxied() -> (SocketAddr, Hits) {
        let hits = Hits::default();
        let upstream = serve(
            Router::new()
                .fallback(upstream_handler)
                .with_state(hits.clone()),
        )
        .await;
        let proxy = serve(mount(Router::new(), NS, upstream)).await;
        (proxy, hits)
    }

    struct Got {
        status: StatusCode,
        headers: HeaderMap,
        body: String,
    }

    impl Got {
        fn json(&self) -> serde_json::Value {
            serde_json::from_str(&self.body).unwrap()
        }
    }

    async fn call(
        addr: SocketAddr,
        method: Method,
        path: &str,
        extra: &[(&str, &str)],
        body: &str,
    ) -> Got {
        let client: Client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http();
        let mut request = Request::builder()
            .method(method)
            .uri(format!("http://{addr}{path}"))
            .body(Full::new(Bytes::from(body.to_owned())))
            .unwrap();
        for (k, v) in extra {
            request.headers_mut().append(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        let response = client.request(request).await.unwrap();
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        Got {
            status: parts.status,
            headers: parts.headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    #[tokio::test]
    async fn the_module_sees_the_request_but_not_the_kernels_bearer() {
        let (proxy, _) = proxied().await;
        let got = call(
            proxy,
            Method::POST,
            &format!("{NS}/things?a=1&b=2"),
            &[
                ("authorization", "Bearer kernel-secret"),
                ("cookie", "session=1"),
                ("x-a24-request-lease", "forged-by-the-client"),
                ("content-type", "application/json"),
            ],
            "{\"hello\":\"world\"}",
        )
        .await;

        assert_eq!(got.status, StatusCode::OK);
        let seen = got.json();
        // Method, path, query and body survive — the semantics §2 requires.
        assert_eq!(seen["method"], "POST");
        assert_eq!(seen["path"], format!("{NS}/things"));
        assert_eq!(seen["query"], "a=1&b=2");
        assert_eq!(seen["body"], "{\"hello\":\"world\"}");
        assert_eq!(seen["headers"]["content-type"], "application/json");

        // The credentials do not.
        assert!(seen["headers"].get("authorization").is_none());
        assert!(seen["headers"].get("cookie").is_none());

        // The forged lease is GONE, not merely accompanied by a real one.
        assert!(seen["headers"].get("x-a24-request-lease").is_none());

        // And the kernel's own correlation id IS there — without this the test
        // above passes for a proxy that forwards no headers at all.
        let id = seen["headers"][REQUEST_ID_HEADER].as_str().unwrap();
        assert!(!id.is_empty());
        assert_ne!(id, "forged-by-the-client");
    }

    #[tokio::test]
    async fn a_client_cannot_choose_its_own_request_id() {
        let (proxy, _) = proxied().await;
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/echo"),
            &[(REQUEST_ID_HEADER, "chosen-by-the-caller")],
            "",
        )
        .await;
        let seen = got.json();
        // The non-empty check first, and it is not decoration: if injection were
        // removed while stripping stayed, this key would be JSON `null`, and
        // `null != "chosen-by-the-caller"` — the assertion below passes for a
        // proxy that sets no request id at all.
        let id = seen["headers"][REQUEST_ID_HEADER].as_str().unwrap_or("");
        assert!(!id.is_empty());
        assert_ne!(id, "chosen-by-the-caller");
    }

    #[tokio::test]
    async fn a_redirect_out_of_the_namespace_never_reaches_the_client() {
        let (proxy, _) = proxied().await;
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/redirect?to=https://evil.example/"),
            &[],
            "",
        )
        .await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(!got.headers.contains_key(header::LOCATION));
        assert!(
            got.body.contains("upstream_location_rejected"),
            "{}",
            got.body
        );
    }

    #[tokio::test]
    async fn a_redirect_inside_the_namespace_is_passed_through_and_not_followed() {
        // The control for the test above: without it, a proxy that 502s every
        // redirect would pass. It also pins the "not followed" half — the
        // upstream is hit ONCE, and the client gets the 302 to act on itself.
        let (proxy, hits) = proxied().await;
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/redirect?to={NS}/elsewhere"),
            &[],
            "",
        )
        .await;
        assert_eq!(got.status, StatusCode::FOUND);
        assert_eq!(
            got.headers.get(header::LOCATION).unwrap(),
            &format!("{NS}/elsewhere")
        );
        assert_eq!(hits.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_module_echoing_kernel_headers_reaches_the_client_without_them() {
        let (proxy, _) = proxied().await;
        let got = call(proxy, Method::GET, &format!("{NS}/echo-secrets"), &[], "").await;
        assert_eq!(got.status, StatusCode::OK);
        for gone in [
            "x-a24-approval-token",
            "x-a24-request-lease",
            "x-a24-whatever",
            "set-cookie",
        ] {
            assert!(!got.headers.contains_key(gone), "{gone} reached the client");
        }
        assert_eq!(got.headers.get("x-module-own-header").unwrap(), "fine");
        assert_eq!(got.body, "ok");
    }

    #[tokio::test]
    async fn an_upstream_that_dies_mid_response_is_a_502_not_a_truncated_200() {
        // A raw socket rather than the axum mock: the failure being tested is
        // one an HTTP server will not produce on purpose. It promises 100 bytes,
        // sends 7, and hangs up. Buffering hides this — the status line already
        // said 200 — so a proxy that forwards what it has reports success for a
        // response that never finished.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut socket, _)) = listener.accept().await {
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial")
                    .await;
                let _ = socket.shutdown().await;
            }
        });

        let proxy = serve(mount(Router::new(), NS, upstream)).await;
        let got = call(proxy, Method::GET, &format!("{NS}/thing"), &[], "").await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(!got.body.contains("partial"), "{}", got.body);
    }

    #[tokio::test]
    async fn an_upstream_that_is_not_listening_is_a_502() {
        // Bind then drop, so the port is one nothing is answering on rather than
        // one that might belong to someone else.
        let dead = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap()
        };
        let proxy = serve(mount(Router::new(), NS, dead)).await;
        let got = call(proxy, Method::GET, &format!("{NS}/thing"), &[], "").await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(got.body.contains("upstream_unavailable"), "{}", got.body);
    }

    #[tokio::test]
    async fn an_oversized_response_is_refused_and_the_cap_is_in_the_message() {
        let (proxy, _) = proxied().await;
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/big?n={}", MAX_BODY_BYTES + 1),
            &[],
            "",
        )
        .await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(
            got.body.contains("upstream_response_too_large"),
            "{}",
            got.body
        );
        // §5: the constant is not configurable, so it has to be in the text —
        // that is the half of configurability anyone actually uses.
        assert!(
            got.body.contains(&MAX_BODY_BYTES.to_string()),
            "{}",
            got.body
        );
    }

    #[tokio::test]
    async fn a_response_exactly_at_the_cap_still_gets_through() {
        // The control. Without it, "too large is refused" is also satisfied by
        // refusing every response.
        let (proxy, _) = proxied().await;
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/big?n={MAX_BODY_BYTES}"),
            &[],
            "",
        )
        .await;
        assert_eq!(got.status, StatusCode::OK);
        assert_eq!(got.body.len(), MAX_BODY_BYTES);
    }

    #[tokio::test]
    async fn server_sent_events_are_refused_rather_than_quietly_buffered() {
        let (proxy, _) = proxied().await;
        let got = call(proxy, Method::GET, &format!("{NS}/sse"), &[], "").await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(
            got.body.contains("upstream_streaming_unsupported"),
            "{}",
            got.body
        );
    }

    #[tokio::test]
    async fn the_namespace_root_and_its_trailing_slash_both_reach_the_module() {
        // `nest` does not cover the bare trailing slash; `mount` adds it. Without
        // this, `/api/v1/zzmock/` would 404 from the kernel while every other
        // path under the namespace worked.
        let (proxy, hits) = proxied().await;
        for path in [NS.to_owned(), format!("{NS}/")] {
            let got = call(proxy, Method::GET, &path, &[], "").await;
            assert_eq!(got.status, StatusCode::OK, "{path}");
        }
        assert_eq!(hits.0.load(Ordering::SeqCst), 2);
    }

    /// A proxy with test-sized deadlines and ceiling.
    ///
    /// Production reads the constants; this exists so the 504 and 503 branches
    /// are reachable in milliseconds. A branch no test can reach and a branch
    /// that is wrong read the same from outside.
    fn proxy_with(upstream: SocketAddr, limits: Limits, inflight: usize) -> Router {
        proxy_and_permits(upstream, limits, inflight).0
    }

    /// The same, plus the semaphore — so a test can wait until a permit has
    /// actually been taken instead of sleeping and hoping.
    fn proxy_and_permits(
        upstream: SocketAddr,
        limits: Limits,
        inflight: usize,
    ) -> (Router, Arc<tokio::sync::Semaphore>) {
        let state = state_with(NS, upstream, limits, inflight);
        let sem = state.inflight.clone();
        (Router::new().fallback(proxy).with_state(state), sem)
    }

    /// Wait until the ceiling reads `want`, or fail saying so.
    ///
    /// Replaces a fixed `sleep` in two tests. A sleep does not ESTABLISH that
    /// the first request took its permit — it only makes it likely, and on a
    /// loaded CI box "likely" is how a test that is not about timing starts
    /// failing about timing.
    async fn wait_for_permits(sem: &tokio::sync::Semaphore, want: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while sem.available_permits() != want {
            assert!(
                std::time::Instant::now() < deadline,
                "permits stayed at {} instead of reaching {want}",
                sem.available_permits()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// A socket that accepts and then does exactly what the script says.
    async fn raw_upstream(script: &'static str) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    match script {
                        // Accept, say nothing, hold the connection open.
                        "silence" => std::future::pending::<()>().await,
                        "finite-chunked" => {
                            let _ = socket
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nhello \r\n5\r\nworld\r\n0\r\n\r\n",
                                )
                                .await;
                            let _ = socket.shutdown().await;
                        }
                        // A body that never ends and never says it is a stream:
                        // the case §2.1 admits cannot be judged at the head.
                        "endless-chunked" => {
                            let _ = socket
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                                )
                                .await;
                            let chunk = [b'x'; 8192];
                            loop {
                                let mut frame = format!("{:x}\r\n", chunk.len()).into_bytes();
                                frame.extend_from_slice(&chunk);
                                frame.extend_from_slice(b"\r\n");
                                if socket.write_all(&frame).await.is_err() {
                                    break;
                                }
                            }
                        }
                        // Head immediately, then one byte at a time, forever.
                        "slow-body" => {
                            let _ = socket
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                                .await;
                            loop {
                                if socket.write_all(b"x").await.is_err() {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                        }
                        "endless-sse" => {
                            let _ = socket
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                                )
                                .await;
                            loop {
                                if socket.write_all(b"c\r\ndata: tick\n\r\n").await.is_err() {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(1)).await;
                            }
                        }
                        _ => unreachable!(),
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_second_location_header_cannot_ride_behind_a_harmless_first_one() {
        // `HeaderMap::get` returns the FIRST value while the sanitiser forwards
        // every one. So checking `get` and forwarding `get_all` means the value
        // judged is not the value that leaves — and clients disagree about which
        // duplicate wins, so the module gets to pick the winner.
        let (proxy, _) = proxied().await;
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/two-locations?to={NS}/ok|/api/v1/runs"),
            &[],
            "",
        )
        .await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(!got.headers.contains_key(header::LOCATION));

        // Two in-namespace values are refused too: "which one did you mean" has
        // no safe default. This is also the control on ORDER — a check that only
        // read the LAST value would pass the case above and fail this one.
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/two-locations?to={NS}/a|{NS}/b"),
            &[],
            "",
        )
        .await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(got.body.contains("more than one Location"), "{}", got.body);
    }

    #[tokio::test]
    async fn a_mixed_case_event_stream_is_still_an_event_stream() {
        let (proxy, _) = proxied().await;
        let got = call(proxy, Method::GET, &format!("{NS}/sse-cased"), &[], "").await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(
            got.body.contains("upstream_streaming_unsupported"),
            "{}",
            got.body
        );
    }

    #[tokio::test]
    async fn an_endless_stream_is_refused_at_the_head_not_at_the_deadline() {
        // This is why the head is judged before the body is collected. Buffer
        // first and the answer is 504 `upstream_timeout` — the code for "the
        // module was slow" — about a module that answered instantly.
        let upstream = raw_upstream("endless-sse").await;
        let proxy = serve(proxy_with(upstream, Limits::default(), 8)).await;

        let started = std::time::Instant::now();
        let got = call(proxy, Method::GET, &format!("{NS}/anything"), &[], "").await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(
            got.body.contains("upstream_streaming_unsupported"),
            "{}",
            got.body
        );
        // Not merely "the right code eventually": the claim is that no deadline
        // was involved, and the default deadlines are 10s and 30s.
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_module_that_never_answers_is_a_504_not_a_502() {
        // §2.1 splits these on purpose: 502 says the module answered something
        // the kernel would not pass on, 504 says it did not answer at all.
        // Without this test, swapping the two leaves every other assertion green.
        let upstream = raw_upstream("silence").await;
        let proxy = serve(proxy_with(
            upstream,
            Limits {
                total: Duration::from_secs(5),
                head: Duration::from_millis(500),
            },
            8,
        ))
        .await;
        let started = std::time::Instant::now();
        let got = call(proxy, Method::GET, &format!("{NS}/anything"), &[], "").await;
        assert_eq!(got.status, StatusCode::GATEWAY_TIMEOUT);
        assert!(got.body.contains("upstream_timeout"), "{}", got.body);
        // It was the HEAD deadline that fired, not the total one. Without this,
        // deleting the head deadline entirely leaves the test green — it just
        // passes five seconds later.
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            got.body.contains("did not begin answering"),
            "the message named the wrong deadline: {}",
            got.body
        );
        // This is the `head < total` HALF of the min(). Its twin below is
        // `total < head`, and NEITHER alone pins the min: with only that one,
        // `head_budget = total` stays green; with only this one,
        // `head_budget = head` stays green. A rule with two sides needs a case
        // on each side.
        assert!(got.body.contains("0.5s"), "{}", got.body);
        assert!(!got.body.contains("5.0s"), "{}", got.body);
    }

    #[tokio::test]
    async fn a_wedged_module_degrades_its_namespace_instead_of_the_daemon() {
        // The ceiling from §5. Without it every in-flight request holds its
        // buffered body and the bound on the daemon's memory is the caller's
        // patience.
        let upstream = raw_upstream("silence").await;
        let (app, sem) = proxy_and_permits(
            upstream,
            Limits {
                total: Duration::from_secs(5),
                head: Duration::from_secs(2),
            },
            1,
        );
        let proxy = serve(app).await;

        let first = tokio::spawn(async move {
            call(proxy, Method::GET, &format!("{NS}/wedged"), &[], "").await
        });
        // Waited for as a FACT rather than assumed after a sleep.
        wait_for_permits(&sem, 0).await;
        let refused = call(proxy, Method::GET, &format!("{NS}/second"), &[], "").await;

        // 503, not 502: the namespace cannot take this request right now, which
        // is a different thing from the module answering badly.
        assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            refused.body.contains("module_overloaded"),
            "{}",
            refused.body
        );

        // The control: once the permit is released the namespace takes requests
        // again, so this is a ceiling and not a stuck door.
        assert_eq!(first.await.unwrap().status, StatusCode::GATEWAY_TIMEOUT);
        let after = call(proxy, Method::GET, &format!("{NS}/third"), &[], "").await;
        assert_eq!(after.status, StatusCode::GATEWAY_TIMEOUT);
        assert!(after.body.contains("upstream_timeout"), "{}", after.body);
    }

    #[tokio::test]
    async fn the_handler_holds_its_permit_until_the_response_body_is_dropped() {
        // The claim §2.1 makes is about MEMORY, not about handler count: the
        // collected bytes are still held while hyper writes them to a client
        // that may be reading slowly, or not at all. Release the permit when the
        // handler returns and the ceiling has counted none of that.
        //
        // Driven through the router rather than a socket, and observed on the
        // semaphore rather than on bytes: "enough bytes to fill a kernel write
        // buffer" is a number that differs per OS, so a socket-pressure test
        // would measure the OS. And it has to go through the HANDLER — an
        // earlier version of this test built a `PermitBody` directly, which
        // proved the type works while staying green with the handler wired the
        // old way. It tested the tool, not the call site.
        use tower::ServiceExt;

        let hits = Hits::default();
        let upstream = serve(Router::new().fallback(upstream_handler).with_state(hits)).await;
        let state = state_with(NS, upstream, Limits::default(), 1);
        let sem = state.inflight.clone();
        let app = Router::new().fallback(proxy).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("{NS}/echo"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // The handler has returned. The bytes have not gone anywhere.
        assert_eq!(
            sem.available_permits(),
            0,
            "the permit was released when the handler returned"
        );

        // Now take the bytes OUT, the way hyper does: the frame leaves the body,
        // and the body is dropped while the memory is still queued for a slow
        // client. A permit tied to the BODY goes back right here — which the
        // previous version of this test could not see, because it never polled.
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            sem.available_permits(),
            0,
            "the permit went back while the bytes were still held"
        );

        drop(bytes);
        assert_eq!(sem.available_permits(), 1, "the permit outlived its bytes");
    }

    #[test]
    fn a_media_type_that_merely_starts_the_same_is_not_an_event_stream() {
        // The control on the SSE refusal: without it, "refuses event streams" is
        // also satisfied by refusing anything whose content-type starts with
        // those characters.
        for (ct, refused) in [
            ("text/event-stream", true),
            ("text/event-stream; charset=utf-8", true),
            ("Text/Event-Stream", true),
            ("text/event-streaming", false),
            ("application/json", false),
        ] {
            let mut parts = Response::new(()).into_parts().0;
            parts
                .headers
                .insert(header::CONTENT_TYPE, HeaderValue::from_str(ct).unwrap());
            let got = head_refusal(NS, "/api/v1/zzmock/x", &parts).is_some();
            assert_eq!(got, refused, "{ct}");
        }
    }

    #[tokio::test]
    async fn a_client_that_never_finishes_its_body_times_out_holding_a_permit() {
        // Two claims in one, both of which the earlier tests left ever-green
        // because every one of them sent a body that completed instantly:
        //
        //   1. the total deadline starts at the CLIENT's body, not at the
        //      upstream call — otherwise a dribbling uploader is invisible to it;
        //   2. the permit is taken before that read, so a stalled uploader
        //      occupies the ceiling rather than slipping under it.
        use tokio::io::AsyncWriteExt;

        // The upstream answers instantly, so nothing here can be blamed on it.
        let hits = Hits::default();
        let upstream = serve(
            Router::new()
                .fallback(upstream_handler)
                .with_state(hits.clone()),
        )
        .await;
        let (app, sem) = proxy_and_permits(
            upstream,
            Limits {
                total: Duration::from_secs(2),
                head: Duration::from_secs(10),
            },
            1,
        );
        let proxy = serve(app).await;

        // Promise 50 bytes, send 3, stall.
        let mut stalled = tokio::net::TcpStream::connect(proxy).await.unwrap();
        stalled
            .write_all(
                format!("POST {NS}/upload HTTP/1.1\r\nHost: x\r\nContent-Length: 50\r\n\r\nabc")
                    .as_bytes(),
            )
            .await
            .unwrap();

        // While it stalls, the one permit is taken.
        wait_for_permits(&sem, 0).await;
        let refused = call(proxy, Method::GET, &format!("{NS}/other"), &[], "").await;
        assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            refused.body.contains("module_overloaded"),
            "{}",
            refused.body
        );

        // And the stalled request ends at the deadline, blaming the right party.
        let mut reply = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::io::AsyncReadExt::read_to_end(&mut stalled, &mut reply),
        )
        .await
        .expect("the stalled request never ended")
        .unwrap();
        let reply = String::from_utf8_lossy(&reply);
        assert!(reply.contains("504"), "{reply}");
        assert!(
            reply.contains("request body was not received"),
            "the timeout blamed the module for a client that stalled: {reply}"
        );

        // The module was never dialled for a request whose body never arrived.
        assert_eq!(hits.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_finite_chunked_response_is_buffered_and_passed_on() {
        // §9 says "SSE / chunked / WebSocket 本轮拒绝". Read literally that
        // refuses this response, and this response is fine: it is finite, it is
        // under the cap, and it is already being buffered rather than streamed.
        //
        // The distinction §2.1 now records: `text/event-stream` DECLARES that a
        // response is a stream, so a module sending one can be told plainly that
        // it cannot. `Transfer-Encoding: chunked` declares nothing — it is how
        // an ordinary server sends a body whose length it did not compute in
        // advance. Refusing it buys literal compliance and costs working modules.
        //
        // An ENDLESS chunked body with no SSE content-type is the case this
        // leaves: it is stopped by the 1 MiB cap or the total deadline, not at
        // the head. That is written down in §2.1 rather than hidden here.
        let upstream = raw_upstream("finite-chunked").await;
        let proxy = serve(proxy_with(upstream, Limits::default(), 8)).await;
        let got = call(proxy, Method::GET, &format!("{NS}/thing"), &[], "").await;
        assert_eq!(got.status, StatusCode::OK);
        assert_eq!(got.body, "hello world");
        // And it did NOT reach the client as chunked — the proxy rebuilt the
        // framing, so the hop-by-hop header is gone.
        assert!(!got.headers.contains_key(header::TRANSFER_ENCODING));
    }

    #[test]
    fn a_query_only_redirect_on_the_namespace_root_stays_inside_it() {
        // RFC 3986 §5.3: a reference with no path keeps the base's path — `?page=2`
        // means "this same resource, other query". Resolving it against the base
        // DIRECTORY drops the last segment, so `/api/v1/zzmock` + `?page=2` came
        // out as `/api/v1/`, and a perfectly ordinary pagination link became a
        // 502. Refusing a legitimate response is a defect too.
        assert!(location_within(NS, NS, "?page=2"));
        assert!(location_within(NS, NS, "#section"));
        assert!(
            location_within(NS, NS, ""),
            "the empty reference is the base"
        );
        assert!(location_within(NS, "/api/v1/zzmock/things/1", "?page=2"));
        // Control: the path-bearing forms are still judged on the path.
        assert!(!location_within(NS, NS, "/api/v1/runs?page=2"));
    }

    #[test]
    fn a_non_ascii_content_type_parameter_does_not_hide_a_stream() {
        // Same shape as the `Connection` byte: `to_str()` fails on a parameter
        // the MODULE chooses, and a check that reads "not a stream" from that
        // failure lets through the one response it exists to refuse.
        let mut parts = Response::new(()).into_parts().0;
        parts.headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_bytes(b"text/event-stream; x=\xff\xfe").unwrap(),
        );
        assert!(head_refusal(NS, "/api/v1/zzmock/x", &parts).is_some());
    }

    #[tokio::test]
    async fn an_endless_body_that_never_calls_itself_a_stream_still_terminates() {
        // The gap §2.1 admits: without an SSE content-type there is nothing in
        // the head to judge, so this one is caught by the 1 MiB cap instead of
        // being refused outright. The claim being pinned is that it IS caught —
        // and with the cap's code, not the timeout's.
        let upstream = raw_upstream("endless-chunked").await;
        let proxy = serve(proxy_with(upstream, Limits::default(), 8)).await;
        let started = std::time::Instant::now();
        let got = call(proxy, Method::GET, &format!("{NS}/thing"), &[], "").await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(
            got.body.contains("upstream_response_too_large"),
            "{}",
            got.body
        );
        // The cap, not the 30s deadline.
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_module_that_starts_answering_and_never_finishes_hits_the_total() {
        // The response-body deadline. Deleting it left every other test green
        // while making a production request hang until the client gave up: the
        // head arrives instantly here, so neither the head deadline nor the cap
        // is what ends this.
        let upstream = raw_upstream("slow-body").await;
        let proxy = serve(proxy_with(
            upstream,
            Limits {
                total: Duration::from_millis(600),
                head: Duration::from_secs(10),
            },
            8,
        ))
        .await;
        let got = call(proxy, Method::GET, &format!("{NS}/thing"), &[], "").await;
        assert_eq!(got.status, StatusCode::GATEWAY_TIMEOUT);
        assert!(
            got.body.contains("did not finish answering"),
            "the timeout named the wrong phase: {}",
            got.body
        );
    }

    #[tokio::test]
    async fn the_timeout_names_the_deadline_that_actually_fired() {
        // The head limit is 5s here and the TOTAL is 1s, so the total is what
        // ends this — and the message has to say so. Reporting the head constant
        // would claim "within 5.0s" about something that gave up after one, and
        // a message naming the wrong limit sends the reader to the wrong knob.
        //
        // The first version of this test asserted only the PHRASE, and the
        // phrase is the same either way — mutating only the NUMBER left it
        // green, which is how a mutation goes red for the wrong reason and
        // still looks like coverage.
        //
        // The second version asserted the number but only on THIS side of the
        // min: `head_budget = state.limits.total` also prints 1.0s here. The
        // other side lives in `a_module_that_never_answers_is_a_504_not_a_502`,
        // and a third case below covers the part neither of them reaches — a
        // budget that is neither constant, because the client already spent
        // some of the total.
        let upstream = raw_upstream("silence").await;
        let proxy = serve(proxy_with(
            upstream,
            Limits {
                total: Duration::from_secs(1),
                head: Duration::from_secs(5),
            },
            8,
        ))
        .await;

        let started = std::time::Instant::now();
        let got = call(proxy, Method::GET, &format!("{NS}/anything"), &[], "").await;
        assert_eq!(got.status, StatusCode::GATEWAY_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            got.body.contains("1.0s"),
            "the message named a limit that did not fire: {}",
            got.body
        );
        assert!(!got.body.contains("5.0s"), "{}", got.body);
    }

    #[tokio::test]
    async fn the_head_budget_is_what_is_left_of_the_total_not_a_constant() {
        // The part neither side of the min() reaches. Both other cases would
        // still pass if the budget were `min(total, head)` computed ONCE, but
        // that is not what §2.1 says: the total starts at the client's body, so
        // by the time the module is dialled some of it is already spent.
        //
        // Here the client takes ~800ms of a 2s total before its body is
        // complete, so the module's head budget is what remains — about 1.2s —
        // and never the 2s a constant would print.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let upstream = raw_upstream("silence").await;
        let proxy = serve(proxy_with(
            upstream,
            Limits {
                total: Duration::from_secs(2),
                head: Duration::from_secs(5),
            },
            8,
        ))
        .await;

        let started = std::time::Instant::now();
        let mut client = tokio::net::TcpStream::connect(proxy).await.unwrap();
        client
            .write_all(
                // `Connection: close` so the reply ends at EOF: with a
                // COMPLETE body hyper keeps the connection alive, and
                // `read_to_end` would wait for a second request that never
                // comes. (The stalled-client test above needs no such header —
                // an undrained body ends the connection by itself.)
                format!(
                    "POST {NS}/slow-upload HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: 4\r\n\r\nab"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        client.write_all(b"cd").await.unwrap();

        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut reply))
            .await
            .expect("the request never ended")
            .unwrap();
        let reply = String::from_utf8_lossy(&reply);

        assert!(reply.contains("504"), "{reply}");
        assert!(reply.contains("did not begin answering"), "{reply}");
        // Not the head constant, and not the whole total either.
        assert!(
            !reply.contains("5.0s"),
            "the head constant was reported: {reply}"
        );
        assert!(
            !reply.contains("2.0s"),
            "the budget was the whole total, ignoring what the client spent: {reply}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_module_cannot_navigate_the_client_with_a_refresh_header() {
        // `Refresh` does `Location`'s job and none of `location_within` runs on
        // it: no scheme check, no `//host`, no `%2e`, no tab, no duplicate rule.
        // Every major browser honours it. So the doc line saying "the kernel's
        // own surface never carries the redirect" was false until this header
        // was dropped — a rule that names ONE header is not a rule about
        // redirection.
        let (proxy, _) = proxied().await;
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/refresh?to=0;url=http://evil.example/"),
            &[],
            "",
        )
        .await;
        assert_eq!(got.status, StatusCode::OK);
        assert!(
            !got.headers.contains_key("refresh"),
            "the module navigated the client off the kernel's surface"
        );
        // Control: an in-namespace `Location` still gets through, so this is not
        // "strip anything that looks like navigation".
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/redirect?to={NS}/ok"),
            &[],
            "",
        )
        .await;
        assert_eq!(got.status, StatusCode::FOUND);
        assert!(got.headers.contains_key(header::LOCATION));
    }

    #[tokio::test]
    async fn a_client_cannot_tell_a_module_where_it_came_from() {
        // The kernel is the only hop, and it does not rewrite these — so a
        // module reading `X-Forwarded-For` would be reading a value the caller
        // typed. Also the asymmetry `host` would otherwise leave: the real
        // authority stripped, a forged one forwarded.
        let (proxy, _) = proxied().await;
        let got = call(
            proxy,
            Method::GET,
            &format!("{NS}/echo"),
            &[
                ("x-forwarded-for", "1.2.3.4"),
                ("x-forwarded-host", "evil.example"),
                ("x-forwarded-proto", "https"),
                ("x-real-ip", "1.2.3.4"),
                ("forwarded", "for=1.2.3.4"),
                // The three that a list of the obvious ones misses: Rack reads
                // the first two for `Request#ssl?`, Symfony builds links from
                // the third.
                ("x-forwarded-ssl", "on"),
                ("x-forwarded-scheme", "https"),
                ("x-forwarded-port", "443"),
                // Positive control — and it has to sit OUTSIDE the prefix now,
                // because the whole `x-forwarded-` family is stripped on
                // purpose: the kernel is the only hop, so none of that family
                // can be legitimate here.
                ("x-module-own-header", "kept"),
            ],
            "",
        )
        .await;
        let seen = got.json();
        for gone in [
            "x-forwarded-for",
            "x-forwarded-host",
            "x-forwarded-proto",
            "x-forwarded-ssl",
            "x-forwarded-scheme",
            "x-forwarded-port",
            "x-real-ip",
            "forwarded",
        ] {
            assert!(
                seen["headers"].get(gone).is_none(),
                "{gone} reached the module"
            );
        }
        assert_eq!(seen["headers"]["x-module-own-header"], "kept");
    }

    #[tokio::test]
    async fn the_overload_message_names_the_ceiling_that_applies() {
        // Same defect as the head budget, caught before it could bite: the text
        // read the CONSTANT while the ceiling came from the state. They are
        // equal in production, so nothing would have reported it — until the day
        // this becomes per-module, when the message starts lying quietly.
        let upstream = raw_upstream("silence").await;
        let (app, sem) = proxy_and_permits(
            upstream,
            Limits {
                total: Duration::from_secs(5),
                head: Duration::from_secs(2),
            },
            1,
        );
        let proxy = serve(app).await;

        let first = tokio::spawn(async move {
            call(proxy, Method::GET, &format!("{NS}/wedged"), &[], "").await
        });
        wait_for_permits(&sem, 0).await;
        let refused = call(proxy, Method::GET, &format!("{NS}/second"), &[], "").await;
        assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            refused.body.contains("has 1 requests"),
            "the message named a ceiling that is not the one in force: {}",
            refused.body
        );
        assert!(
            !refused.body.contains(&MAX_INFLIGHT_PER_MODULE.to_string()),
            "{}",
            refused.body
        );
        let _ = first.await;
    }
}
