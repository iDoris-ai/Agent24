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
//! - **The upstream is a filesystem path to a Unix domain socket, not a
//!   URL** (FU-60) — and it names the generation a request was admitted into
//!   (SUP-3b), set by the kernel when it spawned that run on a socket of its
//!   own (D4). A module therefore has no way to name a host — the local-SSRF
//!   pivot §1 refuses to allow is not defended against here, it is
//!   unrepresentable. Structural. The `Host` header forwarded to the module is
//!   a fixed, kernel-chosen value (`agent24-module.invalid`), never derived
//!   from this path — a path is not an HTTP authority, may hold bytes a header
//!   cannot, and must not tell the module where the kernel's state lives.
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
//! against a mock and lands independently of ME-3b-3. `X-A24-Approval-Token`
//! is minted and injected here as of T7b/ME-3e (see [`mint_approval_token`]) —
//! `X-A24-Request-Lease` still needs a lease table that does not exist; what
//! IS here for it is the stripping, so it can never arrive from a client and
//! can never leave through a module, before it is ever minted.
//!
//! The concurrency ceiling and both deadlines (§5) ARE here, and deliberately —
//! an earlier draft of this comment deferred them to the supervisor. They do not
//! need one: nothing about "how many requests may this module be handling"
//! needs to know when the process started.
//!
//! # Admission (ME-3b-5)
//!
//! Every request is admitted into the module's current
//! [`crate::drain::Generation`] before anything else happens. A module that is
//! starting, draining or stopped answers 503 with its own `code`
//! (`module_not_ready` / `module_draining` / `module_stopping`) — which is why
//! 503 carries a `code` rather than a single meaning.
//!
//! A request whose generation is revoked while it is in flight stops waiting at
//! that moment — not when the process finally dies — and answers 503. Which 503
//! depends on whether it had been sent to the module: if it had,
//! `request_abandoned` and a log line, because the module may or may not have
//! acted on it; if it had not (still reading the client's body, say),
//! `module_stopping`, because then the outcome is known — nothing ran.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, Full, Limited};

use agent24_domain::http::{
    MAX_BODY_BYTES, RESTART_DAEMON_INSTRUCTION, error_response, error_response_with_hint,
    read_body_or_response,
};

use crate::drain::{Abandoned, Current, Generation, RequestRefused};

/// The non-secret correlation id the kernel injects on every proxied request.
pub const REQUEST_ID_HEADER: &str = "x-a24-request-id";

/// The SECRET the kernel mints alongside `request_id` and injects on every
/// proxied request (T7b/ME-3e design doc, decision 2). A module presents it
/// back on `_a24/approval/gate`/`advise` to prove the submission is tied to
/// THIS proxied request, not merely to a `request_id` it can read off the
/// wire (`X-A24-Request-Id` is not a secret — see [`RequestIds`] below).
/// `strips_from_request`/`strips_from_response` cover it automatically: both
/// already strip every `X-A24-*` header by prefix.
pub const APPROVAL_TOKEN_HEADER: &str = "x-a24-approval-token";

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

/// The `Host` header every proxied request carries to the module (FU-60):
/// fixed and kernel-chosen, independent of how the kernel actually reaches the
/// module (a `SocketAddr` before this change; a filesystem path since). A
/// path is not a valid HTTP authority and must not be handed to the module —
/// it would leak where the kernel's state lives. `.invalid` is RFC 2606's
/// reserved, never-resolvable TLD.
const UPSTREAM_HOST: &str = "agent24-module.invalid";

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
    /// Which run of the module serves this namespace, whether it is taking
    /// requests (ME-3b-5), and where it listens (SUP-3b: each run its own
    /// Unix socket, D4/FU-60). Read once per request.
    module: Arc<Current>,
    ids: Arc<RequestIds>,
    limits: Limits,
    /// The ceiling on concurrent proxied requests for THIS module. One per
    /// module, not one per daemon: a wedged module must degrade its own
    /// namespace, not everyone else's.
    inflight: Arc<tokio::sync::Semaphore>,
    /// Connections kept for the next request to the same generation.
    idle: Arc<IdleConnections>,
    /// How many that is. Carried rather than read back from the constant, for
    /// the same reason `TimedOut` carries its budget: the message has to name
    /// the limit that ACTUALLY applies. Today the two are equal in production,
    /// so the constant is not yet wrong — only unguarded, and the day this
    /// becomes per-module the text starts lying with nothing to catch it.
    capacity: usize,
    /// ME4-1.3.2: bounds how often a refused (reserved/rejected) request
    /// logs — a probe or a confused client must not be able to fill the
    /// daemon's log by hammering `_a24` or a malformed path.
    reject_log: Arc<RejectedPathLog>,
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

/// A connection's driver task, aborted when this is dropped: the connection
/// ends with it, and with it whatever request body hyper still held (FU-47).
struct UpstreamConnection(tokio::task::JoinHandle<()>);

impl Drop for UpstreamConnection {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One HTTP/1 connection to one generation's process.
///
/// Reused only by requests admitted into that same generation, only after a
/// request that carried **no body**, and only once hyper reports it ready for
/// another request — its response fully read (SUP-3b). A connection that
/// carried a body is never reused: hyper can only say the body left this
/// process, not that the module read it, and a module that answered without
/// reading would take the NEXT request the proxy sent on that connection
/// after that body's bytes — letting a client's body smuggle a request of its
/// own, kernel headers and all, past the proxy's filter (review of SUP-3b,
/// round 3). That closes the proxy's own reuse as a vector, and only that
/// one: a NON-compliant module that answers without reading the body and
/// then parses what is left of it as a pipelined request can still find a
/// forged request in it on the same connection, with no second request from
/// the proxy at all. No HTTP/1.1 proxy can prevent a module's own parser
/// doing that (a compliant server — hyper, under axum — drains a body it
/// did not consume or closes the connection when it cannot, and never
/// reads framed body bytes as a request head); a forged `x-a24-*` token in
/// it is still one the kernel never minted, and moving to Unix sockets
/// (FU-60) would not change it (FU-62). Every way a request ends
/// other than a clean reuse drops the connection, which aborts the driver:
/// nothing of a request outlives it inside the kernel (FU-47). An idle
/// connection's driver also ends the moment its generation is revoked, so it
/// never keeps a stopping module waiting out its grace. One in use ends too,
/// through the proxy: the revocation wins its race against `forward`, which is
/// dropped with its connection, and the client gets `request_abandoned`.
///
/// Reuse does not make one caller's request the module's to answer with
/// another's: the module writes every response on a connection, and a module
/// that sends one unasked-for, to be read as the next request's, could have
/// sent those bytes as that request's answer anyway. What the kernel
/// guarantees of a response — its headers filtered, no kernel secret in it —
/// holds for whatever response a connection carries (review of SUP-3b,
/// round 4, assessed). Reuse keeps a stream of
/// short requests from closing one socket each into TIME_WAIT (round 2);
/// keying it by generation rather than by address keeps a connection from
/// ever serving a later run (FU-50).
pub(crate) struct Upstream {
    generation: Arc<Generation>,
    sender: hyper::client::conn::http1::SendRequest<Full<Bytes>>,
    /// In the pool, waiting: what a revocation ends the driver for.
    idle: Arc<std::sync::atomic::AtomicBool>,
    /// When this connection most recently entered the idle pool (`put`) —
    /// `None` for one that has never been idle yet. Only ever read/written
    /// while the `Upstream` is owned outright (inside `IdleConnections`'s
    /// lock, or before it has been shared), so a plain field is enough
    /// (FU-64 §B).
    idle_since: Option<Instant>,
    _connection: UpstreamConnection,
}

impl Upstream {
    async fn connect(generation: Arc<Generation>, path: &Path) -> Result<Self, String> {
        // FU-60: a Unix domain socket, not a TCP port — no TIME_WAIT to
        // accumulate for request-bodied requests this pool cannot reuse. No
        // Nagle's algorithm on a Unix socket, so no `set_nodelay` equivalent.
        let stream = tokio::net::UnixStream::connect(path)
            .await
            .map_err(|e| e.to_string())?;
        let (sender, driver) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .map_err(|e| e.to_string())?;
        let revoked = generation.clone();
        let idle = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let idle_now = idle.clone();
        Ok(Self {
            generation,
            sender,
            idle,
            idle_since: None,
            _connection: UpstreamConnection(tokio::spawn(async move {
                let mut driver = std::pin::pin!(driver);
                tokio::select! {
                    _ = &mut driver => return,
                    () = revoked.revoked() => {}
                }
                // Revoked. Idle: end now — a stopping module must not wait for
                // this socket (round 3). In use: finish the exchange.
                if !idle_now.load(std::sync::atomic::Ordering::SeqCst) {
                    let _ = driver.await;
                }
            })),
        })
    }
}

/// How many idle connections a module's proxy keeps. ⚖️ Enough to absorb a
/// burst of sequential requests without reconnecting; the rest close.
const MAX_IDLE_CONNECTIONS: usize = 8;

/// How long, after a response is read, a connection may take to report ready
/// for another request before it is closed instead of kept. Microseconds when
/// the exchange is complete.
const IDLE_SETTLE: Duration = Duration::from_millis(50);

/// (FU-64 §B) The env var that overrides [`IDLE_MAX_AGE_DEFAULT_MS`], read
/// once when a module's `IdleConnections` pool is built. Must be forwarded by
/// `agent24-cli/src/service.rs`'s `PASSTHROUGH_VARS` for a launchd-installed
/// daemon to ever see it — a source-scanning test (SHUT-1b's precedent)
/// checks that.
pub const IDLE_MAX_AGE_ENV: &str = "A24_MODULE_IDLE_CONN_MAX_MS";
const IDLE_MAX_AGE_DEFAULT_MS: u64 = 4000;
/// A sanity ceiling against a likely unit mistake (seconds typed where
/// milliseconds were meant, say), not a claim that every value below it is
/// meaningful or that one above it is unsafe in some way this knob needs to
/// prevent — see the design doc's §B: this whole knob is a best-effort
/// optimization, not a correctness boundary. A value above this ceiling is
/// treated the same as an unparseable one (code review round 1 Low 1): logged
/// and replaced with the default, not honored literally.
const IDLE_MAX_AGE_MAX_MS: u64 = 300_000;

/// Parse an already-fetched [`IDLE_MAX_AGE_ENV`] value, the same shape as
/// `agent24d::lifecycle::Params::from_env`'s two variables: a value that
/// fails to parse is **not** fatal — it is logged and the default is used, so
/// a typo in a tuning knob for a best-effort optimization cannot take a
/// 24/7 daemon down. Takes the value already read rather than reading it
/// itself (unlike an earlier version of this function, which took a
/// generic name-to-value closure and read the environment on a runtime
/// parameter inside it) — `agent24-cli/src/service.rs`'s `PASSTHROUGH_VARS`
/// scanner does lexical, not semantic, analysis: given the right call shape
/// it can resolve a quoted variable name or an ALL-CAPS constant it can
/// trace back to a `pub const` declaration, but it cannot see through a
/// closure indirection where the variable name only appears as a runtime
/// value. [`idle_max_age_from_process_env`] is the one call site that
/// actually reads the environment, in the shape the scanner resolves.
fn idle_max_age_from_env(raw: Option<String>) -> Duration {
    let Some(raw) = raw else {
        return Duration::from_millis(IDLE_MAX_AGE_DEFAULT_MS);
    };
    match raw.trim().parse::<u64>() {
        Ok(ms) if ms <= IDLE_MAX_AGE_MAX_MS => Duration::from_millis(ms),
        _ => {
            tracing::warn!(
                "{IDLE_MAX_AGE_ENV}={raw:?} is not a whole number of milliseconds in \
                 0..={IDLE_MAX_AGE_MAX_MS}; using the default, {IDLE_MAX_AGE_DEFAULT_MS}ms"
            );
            Duration::from_millis(IDLE_MAX_AGE_DEFAULT_MS)
        }
    }
}

/// [`idle_max_age_from_env`] over this process's actual environment, read by
/// [`IDLE_MAX_AGE_ENV`]'s own name — literal enough for the CLI's
/// `PASSTHROUGH_VARS` scanner to resolve (see that function's doc comment for
/// why the indirection matters).
fn idle_max_age_from_process_env() -> Duration {
    idle_max_age_from_env(std::env::var(IDLE_MAX_AGE_ENV).ok())
}

/// A module proxy's idle connections, each to the generation it was opened for.
pub(crate) struct IdleConnections {
    connections: std::sync::Mutex<Vec<Upstream>>,
    /// (FU-64 §B) A connection idle longer than this is treated as already
    /// dead on `take()` and dropped without being tried — best-effort, not a
    /// correctness guarantee: the C-section race this design actually
    /// depends on can happen to a connection of any age.
    max_age: Duration,
}

impl Default for IdleConnections {
    fn default() -> Self {
        Self {
            connections: std::sync::Mutex::new(Vec::new()),
            max_age: idle_max_age_from_process_env(),
        }
    }
}

impl IdleConnections {
    #[cfg(test)]
    fn with_max_age(max_age: Duration) -> Self {
        Self {
            connections: std::sync::Mutex::new(Vec::new()),
            max_age,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Upstream>> {
        self.connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// An idle connection to `generation`, if one is ready. Connections that
    /// have closed, whose generation was revoked, or that have sat idle
    /// longer than `max_age` (FU-64 §B), are dropped on the way.
    fn take(&self, generation: &Arc<Generation>) -> Option<Upstream> {
        let mut idle = self.lock();
        let max_age = self.max_age;
        idle.retain(|c| {
            !c.sender.is_closed()
                && c.generation.state() != crate::drain::DrainState::Revoked
                && c.idle_since.is_none_or(|since| since.elapsed() < max_age)
        });
        let i = idle
            .iter()
            .rposition(|c| Arc::ptr_eq(&c.generation, generation) && c.sender.is_ready())?;
        let taken = idle.swap_remove(i);
        taken.idle.store(false, std::sync::atomic::Ordering::SeqCst);
        Some(taken)
    }

    fn put(&self, mut connection: Upstream) {
        // Marked idle first, then checked: a revocation either happens after
        // the mark — and the driver, seeing it, ends — or before the check,
        // and the connection is not kept. Either way none stays.
        connection
            .idle
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if connection.generation.state() == crate::drain::DrainState::Revoked {
            return;
        }
        connection.idle_since = Some(Instant::now());
        let mut idle = self.lock();
        if idle.len() < MAX_IDLE_CONNECTIONS {
            idle.push(connection);
        }
    }
}

/// This attempt's outcome when it did not get a response (FU-64 §A/§C):
/// classified by whether the module could possibly have received it, not by
/// whether the connection it went out on was reused or freshly opened — a
/// freshly-connected `send_request` can fail just as ambiguously as a reused
/// one, once its handshake has completed.
#[derive(Debug)]
pub(crate) enum ExchangeError {
    /// This attempt is certain never to have reached the module: either a
    /// reused connection told hyper so before any bytes went out (already
    /// retried on a fresh connection inside `exchange`, and that retry also
    /// failed this way), or opening a fresh connection failed before a
    /// request could be handed to it at all (`connect()`, before any
    /// handshake completed).
    NotSent(String),
    /// hyper cannot rule out that the module received this attempt: a reused
    /// connection's `try_send_request` failed without handing the request
    /// back, or a connection (reused or freshly opened) failed after its
    /// handshake completed — a successful write at the socket layer says
    /// nothing about whether the peer read it before closing.
    MaybeSent(String),
}

impl std::fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSent(e) => write!(f, "not sent: {e}"),
            Self::MaybeSent(e) => write!(f, "maybe sent: {e}"),
        }
    }
}

/// FU-64 window (a) test hook (judgement criterion 3): `ready` fires once
/// `exchange` has registered its wait on `proceed`, so a test driving it
/// with `tokio::select!` against `ready.notified()` knows `exchange` is
/// truly parked — not just that a `Notify::notify_one()` landed early and
/// was banked for later. Only ever constructed inside `#[cfg(test)]`.
pub(crate) struct TakeSendGate<'a> {
    ready: &'a tokio::sync::Notify,
    proceed: &'a tokio::sync::Notify,
}

/// Send `request` to `generation`'s process: on an idle connection to it if
/// there is one, else on a new one (unless `force_fresh`, which skips the
/// pool outright — FU-64 §A's explicit retry needs a connection it knows is
/// not already stale). A reused connection the module closed in the
/// meantime, if hyper sees the close before it takes the request to send,
/// hands the request back unsent, and it goes once more on a new connection
/// — so a module's keep-alive timeout seen in time is not a client's 502.
/// One that lands after hyper took it cannot be told from a module that
/// failed while handling it, and is reported [`ExchangeError::MaybeSent`]:
/// the request may have been sent and acted on, so `exchange` itself never
/// sends it again — that decision belongs to the caller (FU-64).
/// Returns the response head and the connection that carries its body.
pub(crate) async fn exchange(
    idle: &IdleConnections,
    generation: &Arc<Generation>,
    path: &Path,
    request: Request<Full<Bytes>>,
    force_fresh: bool,
    // A rendezvous a test can use to land a close exactly between `take()`
    // returning a connection and this function trying to send on it — a
    // window otherwise unobservable from outside `exchange` (window (a)'s
    // deterministic test, FU-64 judgement criterion 3). Always `None` on
    // every production call site; an ordinary parameter rather than a
    // `#[cfg(test)]` one, so this function's shape never forks between test
    // and release builds.
    sync_after_take: Option<&TakeSendGate<'_>>,
    // FU-64 §A, code review round 1 High 1: a revocation-recheck run
    // immediately before the physical `send_request`, AFTER `Upstream::connect`
    // has already completed — on the fresh-connect tail only (the
    // reused-connection tail's own `try_send_request` is not gated — that path
    // already existed before FU-64 and is unchanged). `false` means "do not
    // send" and produces `NotSent` without a dial being wasted on a write.
    // Because the guard runs after `connect`, a revocation that lands DURING
    // `connect` is exactly what this guard catches — that is not the gap.
    // What this narrows, not eliminates, is the much smaller TOCTOU between
    // the guard returning `true` (below) and hyper's dispatcher actually
    // accepting the request inside `send_request` a few lines later — an
    // ordinary async fn call, not a connect-sized window. That residual gap
    // is the same shape as the one that has always existed for a request's
    // FIRST attempt (`InFlight::dispatch` releases its lock before that
    // send, too), and is not a new correctness hole: `proxy()`'s outer
    // `in_flight.finish()` check is authoritative once `dispatch()` has ever
    // succeeded — true from the very first attempt — and discards whatever
    // this function returns in favor of `request_abandoned` if the
    // generation was revoked, regardless of whether this guard caught it.
    send_guard: Option<&(dyn Fn() -> bool + Send + Sync)>,
) -> Result<(hyper::Response<hyper::body::Incoming>, Upstream), ExchangeError> {
    let request = if force_fresh {
        request
    } else {
        match idle.take(generation) {
            Some(mut reused) => {
                if let Some(gate) = sync_after_take {
                    // `enable()` registers this wait BEFORE the "ready" signal
                    // fires, so a test that is only watching for `ready` can
                    // never observe it before this point is truly parked —
                    // the same double-rendezvous `gated()` below uses, not a
                    // `Notify::notify_one()` a caller could race ahead of.
                    let proceed = gate.proceed.notified();
                    tokio::pin!(proceed);
                    proceed.as_mut().enable();
                    gate.ready.notify_one();
                    proceed.await;
                    // The test that drives this gate (window (a), FU-64
                    // judgement criterion 3) has, by the time it releases
                    // `proceed`, triggered the module closing exactly this
                    // connection — but from OUTSIDE `exchange` there is no
                    // handle left to confirm hyper's own driver has
                    // processed that close (`take()` already moved `reused`
                    // in here). Poll the fact from the one place that still
                    // holds it, bounded so a genuine hang surfaces as a
                    // clear timeout rather than wedging the test (code
                    // review round 2 Medium, replacing a fixed yield count
                    // that was still a race). If `TakeSendGate` ever grows a
                    // second, differently-purposed caller that does not
                    // expect a close here, this will need to become opt-in
                    // rather than unconditional — today there is exactly one
                    // caller, and it does.
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                    while !reused.sender.is_closed() {
                        assert!(
                            tokio::time::Instant::now() < deadline,
                            "hyper's client driver never observed the module's close"
                        );
                        tokio::task::yield_now().await;
                    }
                }
                match reused.sender.try_send_request(request).await {
                    Ok(response) => return Ok((response, reused)),
                    Err(mut e) => match e.take_message() {
                        Some(unsent) => unsent,
                        None => return Err(ExchangeError::MaybeSent(e.into_error().to_string())),
                    },
                }
            }
            None => request,
        }
    };
    let mut fresh = Upstream::connect(generation.clone(), path)
        .await
        .map_err(ExchangeError::NotSent)?;
    if let Some(guard) = send_guard
        && !guard()
    {
        return Err(ExchangeError::NotSent(
            "the caller withdrew permission to send just before the connection was used".to_owned(),
        ));
    }
    let response = fresh
        .sender
        .send_request(request)
        .await
        .map_err(|e| ExchangeError::MaybeSent(e.to_string()))?;
    Ok((response, fresh))
}

/// Correlation ids: a per-daemon random prefix and a counter.
///
/// **Not a secret and not treated as one.** §2 splits the correlation id from
/// the approval token precisely so that the loggable half can be cheap. The
/// prefix exists so ids from two daemon lifetimes do not collide in a log, not
/// to make them unguessable — the thing that must be unguessable is
/// `X-A24-Approval-Token`, minted separately by [`mint_approval_token`] (never
/// derived from an id here) precisely because these are not.
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

/// Mint the plaintext `X-A24-Approval-Token` for one proxied request: 32
/// bytes of `/dev/urandom`, hex-encoded (T7b/ME-3e design doc, decision 2).
///
/// Unlike [`short_prefix`], there is NO time-based fallback: a repeated
/// prefix costs two log lines that look related, but a repeated (or
/// otherwise weak) approval token is the one secret this whole design's
/// replay/mismatch guarantees rest on — the same reasoning
/// `launch::mint_token` already applies to the handshake token. Returns
/// `None` when the system entropy source cannot be read; the caller fails
/// just this one request rather than admitting it with a predictable token.
pub(crate) fn mint_approval_token() -> Option<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// ME4-1.3.2 (design `docs/design/ME4-S1-scheduler-callback.md` §7) — the
/// reserved path `/api/v1/<ns>/_a24/…`.
///
/// The kernel forwards this namespace's traffic to a module it does not
/// control, and later capabilities (ME4-1.4.1, the scheduler `fired`
/// callback) need one path prefix the module must never see coming from an
/// arbitrary client — only from the kernel itself, over the callback channel
/// in `kernel_call.rs`. A module that trusts a request just because it
/// arrived on `/_a24/scheduler/fired` can be fooled by any client that can
/// reach this proxy, unless the proxy refuses to forward anything that could
/// canonicalise to `_a24` as its first segment on ANY server's idea of
/// canonicalisation.
///
/// The first segment a kernel keeps for itself, never forwarded to a module.
const RESERVED_SEGMENT: &str = "_a24";
/// Decoding rounds after the first (strict) one — enough for `%25`-nested
/// encodings a module might peel one layer per framework hop.
const EXTRA_DECODE_ROUNDS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathVerdict {
    /// Forward unchanged — the RAW path goes to the module, as today.
    Forward,
    /// First canonical segment is `_a24` → 404, not forwarded.
    Reserved,
    /// Cannot be canonicalised unambiguously → 400 `invalid_request_path`,
    /// not forwarded (fail-closed: if the kernel cannot say what the module
    /// will see, it does not send it). A distinct code from `Reserved`'s 404
    /// so an operator can tell "your path is malformed" from "that path is
    /// the kernel's" (design v2, L8).
    Rejected,
}

/// Judge one request path against `namespace` (design §7.2).
///
/// `raw_path`: [`OriginalUri::path`] exactly as received — percent-encoded,
/// no query (the query is never part of the judgement). `namespace`:
/// `/api/v1/<ns>`, which axum's `nest` has already matched byte-exactly.
///
/// This is a PURE judgement of what the request COULD mean; the path that is
/// actually forwarded on [`PathVerdict::Forward`] is still the untouched raw
/// path — this function never changes what a module sees, only whether it
/// sees anything at all.
#[must_use]
fn judge(namespace: &str, raw_path: &str) -> PathVerdict {
    let Some(rest) = raw_path.strip_prefix(namespace) else {
        return PathVerdict::Rejected;
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return PathVerdict::Rejected;
    }
    // Design v3 (H-B): ANY dot segment is refused, not resolved. The kernel
    // forwards the RAW path, so resolving `..` only in ITS OWN view let
    // `/_a24/scheduler/fired/../../..` (first canonical segment: none) or
    // `/_a24/..` through to a module that prefix-matches `/_a24/…`. A dot
    // segment is judged in every form a server may fold to one: after
    // decoding, after cutting at `;`/`?`/`#` (servlet path parameters:
    // `..;`, `..;a=b`, `..%3B`; a decoded `..%3F`, `..%23frag`), and after
    // dropping trailing spaces/dots (IIS: `..%20`, `...`) — i.e. a non-empty
    // base made only of `.` and ` ` (design v3.1, L1/L2; also covers an
    // all-space segment, which an IIS-style server collapses to empty).
    let mut stack: Vec<String> = Vec::new();
    for raw in rest.split('/') {
        let Some(seg) = canonical_segment(raw) else {
            return PathVerdict::Rejected;
        };
        let base = seg.split([';', '?', '#']).next().unwrap_or("");
        if !base.is_empty() && base.chars().all(|c| c == '.' || c == ' ') {
            return PathVerdict::Rejected;
        }
        if base.is_empty() {
            continue; // `//`, a trailing `/`, or a segment that is only `;params`
        }
        stack.push(seg);
    }
    match stack.first() {
        Some(first) if is_reserved(first) => PathVerdict::Reserved,
        _ => PathVerdict::Forward,
    }
}

/// `_a24`, ASCII case-insensitively, after cutting the segment at the first
/// `;` (path parameters), `?` or `#` (a decoded delimiter a sloppy module
/// might re-parse), and dropping trailing `.` / space (IIS-style servers
/// strip them — design v2, H2).
fn is_reserved(segment: &str) -> bool {
    let head = segment.split([';', '?', '#']).next().unwrap_or("");
    head.trim_end_matches(['.', ' '])
        .eq_ignore_ascii_case(RESERVED_SEGMENT)
}

/// Round 1 strict (any `%` not followed by two hex digits → reject), later
/// rounds lenient (decode valid `%XX`, leave the rest), until a fixed point.
/// Every intermediate form must be valid UTF-8 and free of `/`, `\`, NUL and
/// other control characters — an encoded slash at ANY depth is refused.
fn canonical_segment(raw: &str) -> Option<String> {
    if raw.bytes().any(|b| b == b'\\') {
        return None;
    }
    let mut cur = decode_percent(raw, true)?;
    reject_structural(&cur)?;
    for _ in 0..EXTRA_DECODE_ROUNDS {
        let next = decode_percent(&cur, false)?;
        if next == cur {
            return Some(cur);
        }
        reject_structural(&next)?;
        cur = next;
    }
    // Still changing after the last round: pathological nesting.
    (decode_percent(&cur, false)? == cur).then_some(cur)
}

fn reject_structural(s: &str) -> Option<()> {
    let bad = s.chars().any(|c| c == '/' || c == '\\' || c.is_control());
    (!bad).then_some(())
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `strict`: any `%` not followed by two hex digits is a rejection (round 1).
/// Otherwise (later rounds): decode valid `%XX`, leave everything else —
/// including a lone `%` — untouched, so the fixed-point loop above can tell
/// "nothing left to decode" from "this round changed something".
fn decode_percent(s: &str, strict: bool) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let pair = bytes
                .get(i + 1)
                .and_then(|&h| hex_digit(h))
                .zip(bytes.get(i + 2).and_then(|&l| hex_digit(l)));
            match pair {
                Some((h, l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                None if strict => return None,
                None => {}
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// At most this many "reserved/rejected path" debug lines per second, per
/// mounted namespace — a client hammering `_a24` (or sending malformed
/// paths) must not be able to fill the daemon's log, mirroring
/// [`crate::launch::LOG_LINES_PER_SECOND`]'s reasoning for module output.
const REJECTED_PATH_LOG_LINES_PER_SECOND: u32 = 20;

/// A fixed one-second-window counter, independent per [`ProxyState`] (so one
/// namespace's probing does not steal another's log budget).
struct RejectedPathLog {
    window: std::sync::Mutex<(Instant, u32)>,
}

impl Default for RejectedPathLog {
    fn default() -> Self {
        Self {
            window: std::sync::Mutex::new((Instant::now(), 0)),
        }
    }
}

impl RejectedPathLog {
    /// `true` the first [`REJECTED_PATH_LOG_LINES_PER_SECOND`] times in any
    /// rolling one-second window, `false` after — the caller only logs when
    /// this returns `true`, so log volume is bounded even under a flood.
    fn admit(&self) -> bool {
        let mut w = self
            .window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if now.duration_since(w.0) >= Duration::from_secs(1) {
            *w = (now, 0);
        }
        if w.1 < REJECTED_PATH_LOG_LINES_PER_SECOND {
            w.1 += 1;
            true
        } else {
            false
        }
    }
}

/// The router that proxies one namespace to one module.
///
/// It is a bare fallback, because a module owns every path under its namespace
/// and the kernel does not know which ones exist. Mount it with [`mount`]
/// rather than nesting it directly: `nest` does not cover the bare trailing
/// slash (matchit's `{*rest}` will not match an empty segment), and that rule
/// belongs in one place.
pub fn proxy_router(namespace: &str, module: Arc<Current>) -> Router {
    Router::new()
        .fallback(proxy)
        .with_state(state_for(namespace, module))
}

/// Nest [`proxy_router`] under `namespace`, trailing slash included.
///
/// One [`ProxyState`], shared by both routes, so the two cannot drift — in
/// particular so `/api/v1/ns/` and `/api/v1/ns/x` mint request ids from the
/// same sequence rather than from two that look unrelated in a log.
///
/// There is no address here: each request goes to the address of the
/// generation it was admitted into (SUP-3b, D4).
pub fn mount(app: Router, namespace: &str, module: Arc<Current>) -> Router {
    let state = state_for(namespace, module);
    app.nest(
        namespace,
        Router::new().fallback(proxy).with_state(state.clone()),
    )
    .route(
        &format!("{namespace}/"),
        axum::routing::any(proxy).with_state(state),
    )
}

fn state_for(namespace: &str, module: Arc<Current>) -> ProxyState {
    state_with(
        namespace,
        module,
        Limits::default(),
        MAX_INFLIGHT_PER_MODULE,
    )
}

fn state_with(
    namespace: &str,
    module: Arc<Current>,
    limits: Limits,
    inflight: usize,
) -> ProxyState {
    ProxyState {
        namespace: Arc::new(namespace.to_owned()),
        module,
        ids: Arc::new(RequestIds::new()),
        limits,
        inflight: Arc::new(tokio::sync::Semaphore::new(inflight)),
        idle: Arc::new(IdleConnections::default()),
        capacity: inflight,
        reject_log: Arc::new(RejectedPathLog::default()),
    }
}

async fn proxy(
    State(state): State<ProxyState>,
    OriginalUri(original): OriginalUri,
    request: Request<Body>,
) -> Response {
    // ME4-1.3.2 (design §7.1): the reserved-path judgement runs BEFORE
    // minting a request id or admitting into the generation — a request the
    // kernel refuses here is never registered as in flight, and the module
    // process sees zero bytes of it. `original.path()` is the raw,
    // percent-encoded, query-free path; `state.namespace` is the same
    // `/api/v1/<ns>` axum's `nest` already matched byte-exactly.
    let verdict = judge(&state.namespace, original.path());
    // `{:?}` (not `{}`) so a refused path's own bytes cannot inject a
    // newline or other control character into the log — Rust's `Debug` for
    // `str` escapes them, same reasoning as `launch::log_line`'s cap on
    // module output, just for content rather than volume.
    if !matches!(verdict, PathVerdict::Forward) && state.reject_log.admit() {
        tracing::debug!(
            namespace = %state.namespace,
            path = ?original.path(),
            reserved = matches!(verdict, PathVerdict::Reserved),
            "request path refused before admission"
        );
    }
    match verdict {
        PathVerdict::Reserved => {
            return error_response(StatusCode::NOT_FOUND, "not_found", "no such route");
        }
        PathVerdict::Rejected => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_path",
                "the request path could not be canonicalised",
            );
        }
        PathVerdict::Forward => {}
    }

    // Admission comes first (ME-3b-5, SPEC §4): a module that is starting,
    // draining or stopped takes no new request, and says which — the three are
    // different things to an operator, so they are different `code`s on one 503.
    let request_id = state.ids.mint();
    // T7b/ME-3e (decision 2): minted and hashed BEFORE `admit_request`, so the
    // hash can be registered in the SAME call that admits the id — never a
    // second, later write. A caller that cannot mint a real secret gets a
    // failed request, not one admitted with a predictable token.
    let Some(approval_token) = mint_approval_token() else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "entropy_unavailable",
            "could not mint an approval token for this request",
        );
    };
    let token_hash = crate::drain::sha256(approval_token.as_bytes());
    let generation = state.module.get();
    // T8.5a decision 5: `now`/`budget` are this call's, not a constant read
    // at the call site — `state.limits.total` is the authoritative ceiling
    // for this proxied request (see `Limits`'s own doc comment above), and
    // is what a test can shrink to reach the timeout branch in milliseconds.
    let in_flight = match generation.admit_request(
        request_id.clone(),
        token_hash,
        Instant::now(),
        state.limits.total,
    ) {
        Ok(f) => f,
        Err(refused) => return refused_response(refused, &state.module),
    };

    // Race the request against its generation's revocation. Without the race a
    // revoked request waits for the process to actually die — or for the 30s
    // total deadline, if it ignores SIGTERM — while the module's answer is still
    // being read into memory, for a response that will be thrown away.
    let response = tokio::select! {
        r = forward(&state, &original, request, &request_id, &approval_token, &in_flight) => Some(r),
        () = in_flight.revoked() => None,
    };

    // Whatever `forward` produced — a 200, or a 502 because the process was
    // killed under it — a request whose generation was revoked while it was in
    // flight does not pass it on. Passing a 200 on would report success for a
    // run the kernel already decided to stop; passing the 502 on would blame the
    // module for a kill.
    match (in_flight.finish(), response) {
        (Ok(()), Some(response)) => response,
        // Revoked, but the request never reached the module: the outcome is
        // known, and calling it "unknown" would send an operator looking for a
        // side effect that cannot exist. (`Ok` with no response cannot happen —
        // the revocation signal only fires after the state is Revoked — and is
        // answered the same way rather than trusted to be impossible.)
        (Err(Abandoned { dispatched: false }), _) | (Ok(()), None) => {
            refused_response(RequestRefused::Stopping, &state.module)
        }
        (Err(Abandoned { dispatched: true }), _) => {
            tracing::warn!(
                namespace = %state.namespace,
                request_id = %request_id,
                "request abandoned: the module was stopped while it was in flight; \
                 whether it acted on the request is unknown"
            );
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                Abandoned::CODE,
                "the module was stopped while this request was in flight; \
                 whether it acted on the request is unknown",
            )
        }
    }
}

fn refused_response(refused: RequestRefused, current: &Current) -> Response {
    match refused {
        RequestRefused::NotReady => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            refused.code(),
            "the module is starting and has not completed its handshake",
            "wait a moment and retry; if this module stays stuck here, `agent24 os list` shows \
             whether it is crash-looping",
        ),
        RequestRefused::Draining => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            refused.code(),
            "the module is being stopped: it is finishing the requests it already has \
             and takes no new ones",
            "retry once `agent24 os list` shows the module running again",
        ),
        RequestRefused::Stopping => stopping_response(current),
        RequestRefused::DuplicateId => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            refused.code(),
            "the kernel minted a request id that is already in flight",
        ),
    }
}

/// ERR-1 §D: when the generation is `Revoked` and there is nothing to admit
/// into, the slot's `Status` usually says which of several very different
/// terminal states this is — a distinction `agent24 os list` already shows
/// an operator but a proxied request historically could not (FU-61 判据 6).
/// Falls back to today's plain `module_stopping` for any `Status` this match
/// does not recognize — including the narrow implementation windows where
/// the generation was revoked a moment before the status feed caught up
/// (design doc §D, Codex round 2 Medium 1 / round 3 High 2) — never a panic,
/// never a guess dressed up as one.
fn stopping_response(current: &Current) -> Response {
    use crate::supervisor::Status;
    match current.status() {
        Some(Status::Stopping) => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_stopping",
            "this run is being stopped",
            "it may come back on its own (crash recovery) or it may not; check `agent24 os list` \
             again shortly, or just retry this request",
        ),
        Some(Status::GaveUp {
            failures, within, ..
        }) => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            "circuit_breaker_tripped",
            &format!(
                "the module failed {failures} times within {within:?} and the breaker tripped; \
                 it will not restart on its own"
            ),
            &format!(
                "check `agent24 os list` for the last failure; once it is fixed, \
                 {RESTART_DAEMON_INSTRUCTION}"
            ),
        ),
        Some(Status::PackageChanged { reason }) => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            "package_changed",
            &format!(
                "the installed package changed or became unusable since this module was \
                 mounted: {reason}"
            ),
            RESTART_DAEMON_INSTRUCTION,
        ),
        Some(Status::StopFailed { error }) => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            "stop_failed",
            &format!("this module's last run could not be confirmed stopped: {error}"),
            RESTART_DAEMON_INSTRUCTION,
        ),
        Some(Status::Panicked) => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_panicked",
            "this run's supervisor itself hit a bug and stopped",
            "check the daemon's own logs for the panic; the module itself may be fine — the bug \
             is in the supervisor",
        ),
        Some(Status::Killed) => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_killed",
            "this run was killed outright when the daemon shut down or its supervisor loop was \
             cancelled, not through an ordinary stop",
            "retry once the daemon has fully started or stopped; if this keeps happening, check \
             the daemon's logs",
        ),
        Some(Status::Stopped) => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_stopping",
            "this run has stopped and will not restart on its own",
            &format!(
                "if it should keep serving, first make sure `agent24 os list` shows it enabled \
                 (`agent24 os enable` if not), then {RESTART_DAEMON_INSTRUCTION} — enabling alone \
                 only takes effect at the daemon's next start"
            ),
        ),
        // `Starting`/`Running`/`Backoff` paired with an already-revoked
        // generation, and `None` (no status ever attached — a bare test
        // fixture), are the narrow windows this classification cannot
        // resolve: fall back to today's behavior rather than guess.
        _ => error_response_with_hint(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_stopping",
            "the module has been stopped",
            "this module's state just changed; `agent24 os list` has the current picture",
        ),
    }
}

async fn forward(
    state: &ProxyState,
    original: &Uri,
    request: Request<Body>,
    request_id: &str,
    approval_token: &str,
    in_flight: &crate::drain::InFlight,
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
    // The permit rides with the RESPONSE bytes only (see below), not with the
    // request body handed to hyper. The request body is held only while this
    // request is: it lives on the request's own connection, whose driver is
    // aborted when `forward` finishes or is dropped (`UpstreamConnection`,
    // SUP-3b) — so neither a revocation nor a module that answers without
    // reading its body and keeps the connection open can make hyper keep it
    // (FU-47). Attaching the permit to the request body was tried before that
    // and was worse: such a module made hyper hold the body — permit inside —
    // indefinitely, and 64 of them starved the namespace for good.

    let method = request.method().clone();
    let from_client = request.headers().clone();

    // Read the body under the kernel's own cap: a module must not be the thing
    // that decides how much of the daemon's memory an upload gets.
    let body = match tokio::time::timeout_at(deadline, read_body_or_response(request)).await {
        Err(_) => return timed_out(state, TimedOut::ClientBody),
        Ok(Err(response)) => return response,
        Ok(Ok(b)) => b,
    };
    // Only a connection that carried no body may be reused (see `Upstream`).
    let reusable = body.is_empty();

    let mut headers = sanitize_request_headers(&from_client);
    if let Ok(value) = HeaderValue::from_str(request_id) {
        headers.insert(HeaderName::from_static(REQUEST_ID_HEADER), value);
    }
    // T7b/ME-3e (decision 2): the secret twin of `request_id`, injected at the
    // same admission point and the same way — never minted anywhere else.
    if let Ok(value) = HeaderValue::from_str(approval_token) {
        headers.insert(HeaderName::from_static(APPROVAL_TOKEN_HEADER), value);
    }

    // The address of the generation this request was admitted into (D4) —
    // never the slot's by now, which a restart may have replaced.
    let Some(upstream) = in_flight.upstream() else {
        // Unreachable: only a `Running` generation admits, and `ready` gives
        // that state only to a generation with an address. Refused rather than
        // trusted to be impossible.
        return error_response(
            StatusCode::BAD_GATEWAY,
            "upstream_unavailable",
            "the module's run has no address to send the request to",
        );
    };
    let path_and_query = original
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    // Origin-form, with the authority in `Host`: this is a connection to the
    // module, not a request to be routed by a proxy.
    let upstream_uri = match Uri::builder().path_and_query(path_and_query).build() {
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
    // FU-60: fixed and kernel-chosen, never derived from `upstream` — a
    // filesystem path is not an HTTP authority and must not reach the module
    // (design v2, Medium 2). `.invalid` is RFC 2606's reserved,
    // never-resolvable TLD, so the value cannot be mistaken for anywhere real.
    upstream_request.headers_mut().insert(
        axum::http::header::HOST,
        HeaderValue::from_static(UPSTREAM_HOST),
    );

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
    // From here on the module may act on the request, so a revocation after
    // this point makes its outcome unknown — and one BEFORE it means the request
    // must not be sent at all (see `InFlight::dispatch`).
    if !in_flight.dispatch() {
        return refused_response(RequestRefused::Stopping, &state.module);
    }

    // FU-64 §A: only a request hyper's own send API can retry with zero risk
    // of duplicating a side effect — no request body, and a method RFC 9110
    // §9.2.2 defines as safe — gets a kept copy of its parts to retry with.
    // Read back off `upstream_request` itself rather than kept separately:
    // cheaper for the common (non-retryable) path, since nothing is cloned
    // unless this request actually qualifies.
    let retry_parts = (reusable
        && matches!(
            *upstream_request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        ))
    .then(|| {
        (
            upstream_request.method().clone(),
            upstream_request.uri().clone(),
            upstream_request.headers().clone(),
        )
    });

    // `connection` lives to the end of this function: it is kept for reuse
    // only once the response has been read and hyper says it can take another
    // request; on every other way out it is dropped, and ends (FU-47).
    let (response, mut connection) = match tokio::time::timeout_at(
        head_deadline,
        exchange(
            &state.idle,
            in_flight.generation(),
            upstream,
            upstream_request,
            false,
            None,
            None,
        ),
    )
    .await
    {
        Err(_) => return timed_out(state, TimedOut::UpstreamHead(head_budget)),
        Ok(Ok(r)) => r,
        Ok(Err(ExchangeError::NotSent(e))) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                &format!("the module could not be reached: {e}"),
            );
        }
        Ok(Err(ExchangeError::MaybeSent(first_err))) => {
            let Some((method, uri, headers)) = retry_parts else {
                tracing::debug!(
                    namespace = %state.namespace,
                    request_id = %request_id,
                    error = %first_err,
                    "upstream connection closed at the same moment as the send; \
                     not idempotent, not retrying"
                );
                return maybe_sent_response();
            };
            // Re-checked here, and AGAIN immediately before the physical
            // send inside `exchange` itself (the `send_guard` closure passed
            // below) — two layers, not trusted from the first check before
            // the FIRST attempt: the generation can be revoked at any point
            // between the two attempts, and this is a genuinely new race
            // FU-64 introduces by sending the request a second time (design
            // doc §A, Codex round 1 High 3). `dispatch()` is a safe,
            // idempotent re-insert — not a fresh kind of check — so calling
            // it twice costs nothing.
            //
            // Neither check closes the window completely: `exchange`'s own
            // `send_guard` runs AFTER its `Upstream::connect` completes, so a
            // revocation landing during THAT connect is exactly what it
            // catches — not the gap. What remains is the much smaller window
            // between `send_guard` returning `true` and hyper's dispatcher
            // actually accepting the request inside `send_request` a few
            // lines later (code review round 2 Low, correcting round 1
            // High 1's own comment, which mislocated this as the connect
            // duration). That residual gap is not new: it is the same shape
            // as the one that has always existed between a request's first
            // `dispatch()` and its first physical send, above. Whatever this
            // function returns
            // when either check (or neither) catches the revocation is moot
            // either way: `dispatch()` already succeeded once, before the
            // FIRST attempt, so `proxy()`'s outer `in_flight.finish()` check
            // is authoritative once the generation is revoked and discards
            // this in favor of `request_abandoned` regardless (design doc
            // §A, Codex round 2 Medium 1) — these two rechecks are a latency
            // optimization (skip a doomed connect/send sooner), not the
            // thing that makes a revoked-and-resent request safe.
            if !in_flight.dispatch() {
                return maybe_sent_response();
            }
            let mut retry_request = match Request::builder()
                .method(method)
                .uri(uri)
                .body(Full::new(Bytes::new()))
            {
                Ok(r) => r,
                Err(_) => return maybe_sent_response(),
            };
            *retry_request.headers_mut() = headers;
            match tokio::time::timeout_at(
                head_deadline,
                // `force_fresh`: the one automatic retry must not risk
                // landing on another connection that is just as stale as the
                // first — it exists specifically to remove that variable
                // (design doc §A, Codex round 1 High 2).
                exchange(
                    &state.idle,
                    in_flight.generation(),
                    upstream,
                    retry_request,
                    true,
                    None,
                    Some(&|| in_flight.dispatch()),
                ),
            )
            .await
            {
                // The retry timing out does not undo the first attempt's
                // ambiguity — "once `MaybeSent`, always `MaybeSent`" (design
                // doc §C, round 1 High 1) applies here too, not just to a
                // definite second failure (code review round 1 Medium 1):
                // reporting the generic timeout would silently drop the fact
                // that the FIRST attempt may already have reached the module.
                Err(_) => {
                    tracing::debug!(
                        namespace = %state.namespace,
                        request_id = %request_id,
                        first_error = %first_err,
                        "idempotent retry after an ambiguous send timed out; \
                         reporting the first attempt as possibly delivered"
                    );
                    return maybe_sent_response();
                }
                Ok(Ok(r)) => r,
                // Once `MaybeSent`, always `MaybeSent` (design doc §C, Codex
                // round 1 High 1): a definite `NotSent` on the retry does not
                // undo the ambiguity the FIRST attempt already introduced —
                // that attempt may still have executed.
                Ok(Err(second_err)) => {
                    tracing::debug!(
                        namespace = %state.namespace,
                        request_id = %request_id,
                        first_error = %first_err,
                        retry_error = %second_err,
                        "idempotent retry after an ambiguous send also failed; \
                         reporting the first attempt as possibly delivered"
                    );
                    return maybe_sent_response();
                }
            }
        }
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
        Err(_) => return timed_out(state, TimedOut::UpstreamBody),
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
    // Kept for reuse only after a bodiless request, and only if hyper reports
    // the connection ready within a moment; otherwise it is dropped, and ends.
    // Waited for here, under this request's concurrency permit, so connections
    // waiting to settle are bounded by the permits (a detached task per
    // response was not bounded at all — review of SUP-3b, round 4). The wait
    // costs microseconds when the exchange is complete; the full
    // `IDLE_SETTLE` only for a connection that is neither ready nor closed.
    if reusable
        && matches!(
            tokio::time::timeout(IDLE_SETTLE, connection.sender.ready()).await,
            Ok(Ok(()))
        )
    {
        state.idle.put(connection);
    }

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

/// FU-64 §C: the 502 for a request that may or may not have reached the
/// module — text lifted verbatim from `followups.md`/`tasks.md`'s judged
/// wording rather than re-composed, so there is nothing here for a future
/// review to say "the hint doesn't match the task description" about. The
/// raw hyper error that led here is logged at the call site, not put in this
/// response — an operator gets it from the log, a client gets the fixed,
/// actionable text.
fn maybe_sent_response() -> Response {
    error_response_with_hint(
        StatusCode::BAD_GATEWAY,
        "upstream_connection_closed",
        "the module closed the connection at the same moment this request was sent to it; \
         whether it was processed is unknown",
        "confirm there was no side effect before retrying; if this happens often, raise the \
         module's keep-alive",
    )
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
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;

    const NS: &str = "/api/v1/zzmock";

    /// A `/tmp` path for a throwaway mock-module socket, unique enough that a
    /// later run reusing this process's pid never collides with a node this
    /// one left behind (review of FU-60, round 1: a pid-only name did). Pid
    /// plus an atomic counter tells apart calls within one process; the
    /// nanosecond timestamp tells apart this process from a past one that
    /// happened to get the same pid and left a node behind. The node itself
    /// is left behind on purpose (review of FU-60, round 3: no test here
    /// unlinks it) — these mock modules run in detached `tokio::spawn` tasks
    /// with no natural moment to clean up, and uniqueness (not cleanup) is
    /// what keeps the tests hermetic; the OS reaps `/tmp` on its own schedule.
    fn unique_sock(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        PathBuf::from(format!(
            "/tmp/a24-{tag}-{}-{nanos}-{n}.sock",
            std::process::id()
        ))
    }

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

    /// A module that has completed its handshake — what every test before
    /// ME-3b-5 implicitly assumed.
    fn running_module(upstream: PathBuf) -> Arc<Current> {
        Current::new(running_generation(upstream))
    }

    async fn serve(app: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        addr
    }

    /// A mock module: axum served over a Unix domain socket (FU-60), the way
    /// a real one is. A short, unique `/tmp` path — not `CallbackDir`'s
    /// machinery, which these tests have no daemon-lifetime process to tie
    /// its cleanup to; the socket file is left behind, same as other
    /// throwaway state this test module leaks on purpose.
    async fn serve_unix(app: Router) -> PathBuf {
        let path = unique_sock("mock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        path
    }

    /// A proxy in front of a mock module. Returns (proxy addr, upstream hits).
    async fn proxied() -> (SocketAddr, Hits) {
        let hits = Hits::default();
        let upstream = serve_unix(
            Router::new()
                .fallback(upstream_handler)
                .with_state(hits.clone()),
        )
        .await;
        let proxy = serve(mount(Router::new(), NS, running_module(upstream))).await;
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

    /// The test's own HTTP client — a pooled one is fine on this side.
    type Client = hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        Full<Bytes>,
    >;

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

    /// FU-60 (design v4, Medium 2): the `Host` the module sees is the fixed,
    /// kernel-chosen value — never the filesystem path the kernel actually
    /// dials, which is not a valid HTTP authority and would leak where the
    /// kernel's state lives.
    #[tokio::test]
    async fn the_module_sees_a_fixed_host_never_the_sockets_path() {
        let (proxy, _) = proxied().await;
        let got = call(proxy, Method::GET, &format!("{NS}/things"), &[], "").await;
        assert_eq!(got.status, StatusCode::OK);
        let seen = got.json();
        let host = seen["headers"]["host"].as_str().unwrap();
        assert_eq!(host, UPSTREAM_HOST);
        for leaked in [".sock", ".l", "/tmp", "run/"] {
            assert!(
                !host.contains(leaked),
                "the Host header leaked a path fragment ({leaked:?}): {host}"
            );
        }
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
        let upstream = unique_sock("partial");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut socket, _)) = listener.accept().await {
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial")
                    .await;
                let _ = socket.shutdown().await;
            }
        });

        let proxy = serve(mount(Router::new(), NS, running_module(upstream))).await;
        let got = call(proxy, Method::GET, &format!("{NS}/thing"), &[], "").await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert!(!got.body.contains("partial"), "{}", got.body);
    }

    #[tokio::test]
    async fn an_upstream_that_is_not_listening_is_a_502() {
        // Never bound: a path nothing is listening on, not one that might
        // belong to someone else.
        let dead = unique_sock("dead");
        let proxy = serve(mount(Router::new(), NS, running_module(dead))).await;
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
    fn proxy_with(upstream: PathBuf, limits: Limits, inflight: usize) -> Router {
        proxy_and_permits(upstream, limits, inflight).0
    }

    /// The same, plus the semaphore — so a test can wait until a permit has
    /// actually been taken instead of sleeping and hoping.
    fn proxy_and_permits(
        upstream: PathBuf,
        limits: Limits,
        inflight: usize,
    ) -> (Router, Arc<tokio::sync::Semaphore>) {
        proxy_and_permits_for(running_module(upstream), limits, inflight)
    }

    fn proxy_and_permits_for(
        module: Arc<Current>,
        limits: Limits,
        inflight: usize,
    ) -> (Router, Arc<tokio::sync::Semaphore>) {
        let state = state_with(NS, module, limits, inflight);
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
    async fn raw_upstream(script: &'static str) -> PathBuf {
        let path = unique_sock("raw");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let addr = path.clone();
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
                                // `data: tick\n` is 11 bytes = 0xb (review of
                                // FU-60, round 1: this was `c`, one byte off —
                                // harmless only because this test rejects
                                // before parsing the body).
                                if socket.write_all(b"b\r\ndata: tick\n\r\n").await.is_err() {
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
        let upstream = serve_unix(Router::new().fallback(upstream_handler).with_state(hits)).await;
        let state = state_with(NS, running_module(upstream), Limits::default(), 1);
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
        let upstream = serve_unix(
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

    // ── SUP-3b: the address is the admitted generation's; connections are
    //    reused per generation, and end with any request that is given up ──

    /// A module that reads a request's head and then either answers at once —
    /// without reading the body, keeping the connection open — or never
    /// answers. Resolves `closed` when the proxy ends the connection.
    async fn upstream_watching_close(
        answer: bool,
    ) -> (PathBuf, tokio::sync::oneshot::Receiver<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let addr = unique_sock("close");
        let listener = tokio::net::UnixListener::bind(&addr).unwrap();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let mut seen = Vec::new();
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                seen.extend_from_slice(&buf[..n]);
            }
            if answer {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await
                    .unwrap();
            }
            // Never close from this side: only the proxy can end it now.
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = closed_tx.send(());
        });
        (addr, closed_rx)
    }

    /// Dropping a connection ends it even while hyper is still blocked on it: a
    /// request body far larger than any socket buffer, to a module that
    /// answered at once and then stopped reading. Dropping the guard ends the
    /// connection with most of the body unsent — the module, reading again
    /// afterwards, finds EOF well before the declared length. Without the
    /// abort, hyper keeps writing, and the module would receive all of it
    /// (review of SUP-3b, round 1: a module that kept reading drained the body
    /// and hid the difference).
    #[tokio::test]
    async fn dropping_the_connection_guard_ends_a_connection_blocked_on_its_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const BODY: usize = 64 * 1024 * 1024;
        let upstream = unique_sock("body");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel::<()>();
        let module = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 64 * 1024];
            let mut seen = Vec::new();
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "closed before the head");
                seen.extend_from_slice(&buf[..n]);
            }
            let mut received = seen.len();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
            // Stop reading: hyper blocks on the body. Read again only once the
            // test has dropped the guard, and count what still arrives.
            resume_rx.await.unwrap();
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => received += n,
                }
            }
            received
        });
        let request = Request::builder()
            .method(Method::POST)
            .uri("/a")
            .header(axum::http::header::HOST, UPSTREAM_HOST)
            .body(Full::new(Bytes::from(vec![b'x'; BODY])))
            .unwrap();
        let mut connection = Upstream::connect(running_generation(upstream.clone()), &upstream)
            .await
            .unwrap();
        let response = connection.sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _ = response.into_body().collect().await;
        drop(connection);
        resume_tx.send(()).unwrap();
        let received = tokio::time::timeout(Duration::from_secs(10), module)
            .await
            .expect("the connection outlived its guard")
            .unwrap();
        assert!(
            received < BODY,
            "the whole body was still written after the guard was dropped ({received} bytes)"
        );
    }

    /// A module that records which CONNECTION (not address — a Unix socket
    /// peer has no distinguishing one, FU-60) each request arrived on, so a
    /// test can count the connections the proxy opened. A raw accept loop,
    /// like the other hand-rolled mock modules in this file: each accepted
    /// connection gets a sequential id, kept alive across requests exactly as
    /// a real keep-alive module would.
    async fn peer_counting_upstream() -> (
        PathBuf,
        Arc<std::sync::Mutex<std::collections::HashSet<u64>>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let path = unique_sock("peers");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let peers = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        let seen = peers.clone();
        tokio::spawn(async move {
            static CONN_SEQ: AtomicU64 = AtomicU64::new(0);
            while let Ok((mut socket, _)) = listener.accept().await {
                let conn_id = CONN_SEQ.fetch_add(1, Ordering::SeqCst);
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let mut head = Vec::new();
                    loop {
                        head.clear();
                        loop {
                            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                            match socket.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buf[..n]),
                            }
                        }
                        seen.lock().unwrap().insert(conn_id);
                        if socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        (path, peers)
    }

    /// FU-63: a module whose keep-alive ends between requests — it closes
    /// an idle connection without having said `Connection: close` — costs
    /// the client no 502 once the close is visible: `IdleConnections::take`
    /// passes over a connection seen closed, and one handed over closed
    /// before its request was written is taken back and sent again on a new
    /// connection. Either suffices; with both gone, the requests after the
    /// first are answered 502. Each step waits on the proxy's own state, not
    /// on time: the connection is in the pool, the module closes it, hyper
    /// has seen the close — then the next request goes. A close landing as
    /// hyper takes a request can still be answered 502: once taken, a
    /// request may have been sent and acted on, so it is not sent again
    /// (FU-64).
    #[tokio::test]
    async fn a_module_closing_an_idle_connection_costs_no_502() {
        let upstream = unique_sock("idle");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let close = Arc::new(tokio::sync::Notify::new());
        let (closed_tx, mut closed) = tokio::sync::mpsc::unbounded_channel::<()>();
        {
            let close = close.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                while let Ok((mut socket, _)) = listener.accept().await {
                    let (close, closed_tx) = (close.clone(), closed_tx.clone());
                    tokio::spawn(async move {
                        let mut head = Vec::new();
                        let mut buf = [0u8; 4096];
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match socket.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buf[..n]),
                            }
                        }
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                            .await;
                        close.notified().await;
                        drop(socket);
                        let _ = closed_tx.send(());
                    });
                }
            });
        }
        let state = state_for(NS, running_module(upstream));
        let idle = state.idle.clone();
        let proxy = serve(Router::new().fallback(proxy).with_state(state)).await;
        for i in 0..20 {
            let got = call(proxy, Method::GET, &format!("{NS}/{i}"), &[], "").await;
            assert_eq!(got.status, StatusCode::OK, "request {i}: {}", got.body);
            // `call` returns as the response ends; the proxy pools the
            // connection just after.
            for _ in 0..500 {
                if idle.lock().len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            assert_eq!(
                idle.lock().len(),
                1,
                "request {i}: its connection was not pooled"
            );
            close.notify_one();
            tokio::time::timeout(Duration::from_secs(5), closed.recv())
                .await
                .expect("the module never closed the connection")
                .unwrap();
            for _ in 0..500 {
                if idle.lock().iter().all(|c| c.sender.is_closed()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            assert!(
                idle.lock().iter().all(|c| c.sender.is_closed()),
                "request {i}: the proxy never saw the close"
            );
        }
    }

    /// Round 2 of the SUP-3b review: one connection per request made a stream
    /// of short requests close one socket each into TIME_WAIT, enough to use
    /// up loopback's ephemeral ports. Requests to one generation reuse a
    /// connection once its exchange is complete.
    #[tokio::test]
    async fn sequential_requests_to_one_generation_reuse_a_connection() {
        let (upstream, peers) = peer_counting_upstream().await;
        let proxy = serve(mount(Router::new(), NS, running_module(upstream))).await;
        for i in 0..20 {
            let got = call(proxy, Method::GET, &format!("{NS}/{i}"), &[], "").await;
            assert_eq!(got.status, StatusCode::OK, "{}", got.body);
        }
        assert_eq!(peers.lock().unwrap().len(), 1, "a connection per request");
    }

    /// FU-50: a connection is reused only by requests to the generation it was
    /// opened for — never by a later run, even one at the same address (which
    /// D4 rules out in production; a test can arrange it, and that is the case
    /// an address-keyed pool would get wrong).
    #[tokio::test]
    async fn a_connection_is_never_reused_by_another_generation() {
        let (upstream, peers) = peer_counting_upstream().await;
        let current = Current::new(running_generation(upstream.clone()));
        let proxy = serve(mount(Router::new(), NS, current.clone())).await;
        let got = call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await;
        assert_eq!(got.status, StatusCode::OK);
        let _ = current.replace(running_generation(upstream));
        let got = call(proxy, Method::GET, &format!("{NS}/b"), &[], "").await;
        assert_eq!(got.status, StatusCode::OK);
        assert_eq!(
            peers.lock().unwrap().len(),
            2,
            "a later run was sent a request on its predecessor's connection"
        );
    }

    /// A connection that carried a request body is never kept for reuse:
    /// hyper can only say the body left this process, not that the module
    /// read it, and a module that answered without reading would parse the
    /// next request's bytes after it — a client's body smuggling a request of
    /// its own past the proxy's header filter (review of SUP-3b, round 3). So
    /// after a small body the module never reads, the connection is closed.
    #[tokio::test]
    async fn a_connection_that_carried_a_body_is_never_kept() {
        let (upstream, closed) = upstream_watching_close(true).await;
        let proxy = serve(mount(Router::new(), NS, running_module(upstream))).await;
        let smuggled = "GET /x HTTP/1.1\r\nx-a24-approval-token: forged\r\n\r\n";
        let got = call(proxy, Method::POST, &format!("{NS}/a"), &[], smuggled).await;
        assert_eq!(got.status, StatusCode::OK);
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("a connection that carried a body was kept for reuse")
            .unwrap();
    }

    /// A revoked generation's idle connection is closed at the revocation —
    /// not when some later request happens by: a stopping module waiting for
    /// its connections to close must not wait out its grace (round 3).
    #[tokio::test]
    async fn a_revoked_generations_idle_connection_is_closed() {
        let (old_addr, closed) = upstream_watching_close(true).await;
        let old = running_generation(old_addr);
        let current = Current::new(old.clone());
        let proxy = serve(mount(Router::new(), NS, current.clone())).await;
        let got = call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await;
        assert_eq!(got.status, StatusCode::OK);
        // The connection is in the pool before the response is sent. Revoke —
        // and nothing else.
        let _ = old.revoke();
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("a revoked run's idle connection was kept")
            .unwrap();
    }

    /// FU-47, the other way a request ends: revoked while the module sits on
    /// it. The client is answered at once, and the connection — with the body
    /// hyper was given — ends with it.
    #[tokio::test]
    async fn a_revoked_request_takes_its_upstream_connection_with_it() {
        let (upstream, closed) = upstream_watching_close(false).await;
        let generation = running_generation(upstream);
        let proxy = serve(mount(Router::new(), NS, Current::new(generation.clone()))).await;
        let first = tokio::spawn(async move {
            call(proxy, Method::POST, &format!("{NS}/a"), &[], "body").await
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while generation.in_flight() == 0 {
            assert!(std::time::Instant::now() < deadline, "never admitted");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Let it reach the module before revoking.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = generation.revoke();
        assert_eq!(first.await.unwrap().status, StatusCode::SERVICE_UNAVAILABLE);
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("the upstream connection outlived its revoked request")
            .unwrap();
    }

    /// D4: a request goes to the address of the generation it was ADMITTED
    /// into, even when a restart has put another generation in the slot before
    /// the request is sent — here while the client is still sending its body.
    /// Reading the slot at send time would deliver it to the new process.
    #[tokio::test]
    async fn a_request_goes_to_the_generation_it_was_admitted_into() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let a = gated(Then::Answer).await;
        let b = gated(Then::Answer).await;
        let old = running_generation(a.addr.clone());
        let current = Current::new(old.clone());
        let proxy = serve(mount(Router::new(), NS, current.clone())).await;

        let mut client = tokio::net::TcpStream::connect(proxy).await.unwrap();
        client
            .write_all(
                format!("POST {NS}/a HTTP/1.1\r\nhost: x\r\nconnection: close\r\ncontent-length: 4\r\n\r\nab")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while old.in_flight() == 0 {
            assert!(std::time::Instant::now() < deadline, "never admitted");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // A restart, while the body is still coming.
        let _ = current.replace(running_generation(b.addr));
        client.write_all(b"cd").await.unwrap();
        a.wait_arrived().await;
        a.release.notify_waiters();
        let mut response = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut response)).await;
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(a.dials.load(Ordering::SeqCst), 1);
        assert_eq!(
            b.dials.load(Ordering::SeqCst),
            0,
            "sent to the generation that replaced it"
        );
    }

    // ── ME-3b-5: the proxy side of the two-phase stop ────────────────────

    /// What a gated module does once the test lets it go.
    #[derive(Clone, Copy)]
    enum Then {
        /// Answer `200 done`.
        Answer,
        /// Close the connection without answering — what the kernel sees when
        /// it kills the process under a request.
        Vanish,
    }

    /// A module that holds every request until the test releases it.
    ///
    /// `arrived` fires once per request that actually reached the module, so a
    /// test can wait for "it is in flight" as a fact, and can count dials.
    struct Gated {
        addr: PathBuf,
        arrived: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Notify>,
        dials: Arc<AtomicUsize>,
        /// The `x-a24-request-id` each request arrived with, in arrival order.
        ids: Arc<std::sync::Mutex<Vec<String>>>,
    }

    async fn gated(then: Then) -> Gated {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let addr = unique_sock("gated");
        let listener = tokio::net::UnixListener::bind(&addr).unwrap();
        let arrived = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let dials = Arc::new(AtomicUsize::new(0));
        let ids = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (a, r, d, i) = (arrived.clone(), release.clone(), dials.clone(), ids.clone());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                d.fetch_add(1, Ordering::SeqCst);
                let (a, r, i) = (a.clone(), r.clone(), i.clone());
                tokio::spawn(async move {
                    // Read the request head before announcing arrival, so
                    // "arrived" means the proxy really sent it.
                    let mut buf = vec![0u8; 4096];
                    let mut seen = Vec::new();
                    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        let Ok(n) = socket.read(&mut buf).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        seen.extend_from_slice(&buf[..n]);
                    }
                    let head = String::from_utf8_lossy(&seen).to_ascii_lowercase();
                    if let Some(id) = head
                        .lines()
                        .find_map(|l| l.strip_prefix("x-a24-request-id:"))
                    {
                        i.lock().unwrap().push(id.trim().to_owned());
                    }
                    // Register for the release BEFORE announcing arrival.
                    // `notify_waiters` only wakes futures already registered, so
                    // in the other order a test that releases right after
                    // `wait_arrived` can land in the gap and the request hangs.
                    let released = r.notified();
                    tokio::pin!(released);
                    released.as_mut().enable();
                    a.add_permits(1);
                    released.await;
                    if let Then::Answer = then {
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone")
                            .await;
                    }
                    // `Vanish`: dropped here without a byte.
                });
            }
        });
        Gated {
            addr,
            arrived,
            release,
            dials,
            ids,
        }
    }

    impl Gated {
        async fn wait_arrived(&self) {
            tokio::time::timeout(Duration::from_secs(10), self.arrived.acquire())
                .await
                .expect("the request never reached the module")
                .unwrap()
                .forget();
        }
    }

    fn running_generation(upstream: PathBuf) -> Arc<crate::drain::Generation> {
        let g = crate::drain::Generation::serving_at(upstream);
        assert!(g.ready());
        g
    }

    /// SPEC §8 ME-3b: *"DRAINING 期间新的被代理请求 503、在途请求的回调仍可用"* —
    /// the proxy half: the new request is refused with its own code and never
    /// dialled, and the one already in flight completes with the module's answer.
    #[tokio::test]
    async fn draining_refuses_a_new_request_while_the_one_in_flight_completes() {
        let module = gated(Then::Answer).await;
        let generation = running_generation(module.addr.clone());
        let proxy = serve(mount(Router::new(), NS, Current::new(generation.clone()))).await;

        let first =
            tokio::spawn(
                async move { call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await },
            );
        module.wait_arrived().await;

        assert!(generation.begin_drain(std::time::Instant::now(), Duration::from_secs(30)));
        let refused = call(proxy, Method::GET, &format!("{NS}/b"), &[], "").await;
        assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(refused.json()["error"]["code"], "module_draining");
        assert!(refused.body.contains("being stopped"), "{}", refused.body);
        assert_eq!(
            module.dials.load(Ordering::SeqCst),
            1,
            "a request refused for draining still reached the module"
        );

        // The other half of §8: the in-flight request's callbacks still work.
        // They carry the id the MODULE was given, so that is the id to test with
        // — not one taken from the kernel's side. If the proxy registered one id
        // and sent the module another, every in-flight callback during a drain
        // would be refused and nothing else here would notice.
        let seen = module.ids.lock().unwrap()[0].clone();
        assert_eq!(generation.admit_callback(Some(&seen)), Ok(()));
        assert_eq!(
            generation.admit_callback(Some("forged-id")),
            Err(crate::drain::CallbackRefused::DrainingUnknownRequest),
            "control: the check is against the live set, not 'any id passes'"
        );

        // The control: the request that was already in flight is not a casualty.
        module.release.notify_waiters();
        let first = first.await.unwrap();
        assert_eq!(first.status, StatusCode::OK);
        assert_eq!(first.body, "done");
    }

    /// SPEC §8: *"drain 超时的在途请求返回 503(不假装成功)"* — through the whole
    /// handler: the request is with the module when its generation is revoked,
    /// and the client gets 503 `request_abandoned` plus the log line. (Whether
    /// the module's 200 is ever read here depends on which side of the race wins;
    /// that a 200 arriving AFTER the revocation is not committed is pinned
    /// separately, by `a_200_that_arrives_after_the_revocation_is_not_committed`.)
    #[tokio::test]
    async fn a_request_whose_generation_is_revoked_gets_a_503_not_the_modules_200() {
        let (logs, _guard) = capture_logs();
        let module = gated(Then::Answer).await;
        let generation = running_generation(module.addr.clone());
        let proxy = serve(mount(Router::new(), NS, Current::new(generation.clone()))).await;

        let first =
            tokio::spawn(
                async move { call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await },
            );
        module.wait_arrived().await;

        let revocation = generation.revoke().unwrap();
        assert_eq!(revocation.abandoned.len(), 1);
        module.release.notify_waiters();

        let got = first.await.unwrap();
        assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(got.json()["error"]["code"], "request_abandoned");
        assert!(got.body.contains("unknown"), "{}", got.body);

        // SPEC §4: the abandonment is written to the log, not swallowed — and
        // the line names the request, or it cannot be matched to anything.
        let id = module.ids.lock().unwrap()[0].clone();
        let log = logs.text();
        assert!(
            log.contains("abandoned") && log.contains("unknown"),
            "{log}"
        );
        assert!(log.contains(&id), "the log line does not name {id}: {log}");
    }

    /// The same, when the kill lands before the module answers: the connection
    /// just dies. That is a 502 `upstream_unavailable` from `forward` — which
    /// would tell an operator the MODULE failed, when the kernel stopped it.
    #[tokio::test]
    async fn a_request_killed_under_a_revocation_is_abandoned_not_blamed_on_the_module() {
        let module = gated(Then::Vanish).await;
        let generation = running_generation(module.addr.clone());
        let proxy = serve(mount(Router::new(), NS, Current::new(generation.clone()))).await;

        let first =
            tokio::spawn(
                async move { call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await },
            );
        module.wait_arrived().await;
        let _ = generation.revoke();
        module.release.notify_waiters();

        let got = first.await.unwrap();
        assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(got.json()["error"]["code"], "request_abandoned");
    }

    /// The control for the two above: the same vanishing module, NOT revoked,
    /// is still the module's failure (502). Without this, a proxy that turned
    /// every upstream error into `request_abandoned` would pass them.
    ///
    /// A POST (not idempotent, not retried — FU-64 §A) rather than the GET
    /// this test used before FU-64: `gated`'s module reads the full request
    /// before vanishing, so this is unambiguously "may have been acted on",
    /// not "never reached it" — `upstream_connection_closed`, not the old
    /// blanket `upstream_unavailable` FU-64 replaced for exactly this case.
    #[tokio::test]
    async fn a_module_that_vanishes_on_its_own_is_still_a_502() {
        let module = gated(Then::Vanish).await;
        let proxy = serve(mount(
            Router::new(),
            NS,
            running_module(module.addr.clone()),
        ))
        .await;

        let first = tokio::spawn(async move {
            call(proxy, Method::POST, &format!("{NS}/a"), &[], "body").await
        });
        module.wait_arrived().await;
        module.release.notify_waiters();

        let got = first.await.unwrap();
        assert_eq!(got.status, StatusCode::BAD_GATEWAY);
        assert_eq!(got.json()["error"]["code"], "upstream_connection_closed");
    }

    /// Before `initialize` the namespace answers 503 `module_not_ready` and the
    /// module is never dialled. Control: the same generation, once ready, serves.
    #[tokio::test]
    async fn a_module_that_is_not_ready_is_never_dialled() {
        let module = gated(Then::Answer).await;
        let generation = crate::drain::Generation::serving_at(module.addr.clone());
        let proxy = serve(mount(Router::new(), NS, Current::new(generation.clone()))).await;

        let got = call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await;
        assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(got.json()["error"]["code"], "module_not_ready");
        assert!(got.body.contains("starting"), "{}", got.body);
        assert_eq!(module.dials.load(Ordering::SeqCst), 0);

        assert!(generation.ready());
        let second =
            tokio::spawn(
                async move { call(proxy, Method::GET, &format!("{NS}/b"), &[], "").await },
            );
        module.wait_arrived().await;
        module.release.notify_waiters();
        assert_eq!(second.await.unwrap().status, StatusCode::OK);
    }

    /// A restart swaps the slot the proxy reads, without remounting anything.
    #[tokio::test]
    async fn after_a_restart_the_proxy_admits_into_the_new_generation() {
        let module = gated(Then::Answer).await;
        let old = running_generation(module.addr.clone());
        let current = Current::new(old.clone());
        let proxy = serve(mount(Router::new(), NS, current.clone())).await;

        let _ = old.revoke();
        let refused = call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await;
        assert_eq!(refused.json()["error"]["code"], "module_stopping");
        assert!(
            refused.body.contains("has been stopped"),
            "{}",
            refused.body
        );

        let _ = current.replace(running_generation(module.addr.clone()));
        let next =
            tokio::spawn(
                async move { call(proxy, Method::GET, &format!("{NS}/b"), &[], "").await },
            );
        module.wait_arrived().await;
        module.release.notify_waiters();
        assert_eq!(next.await.unwrap().status, StatusCode::OK);
    }

    /// Captured `tracing` output for one test. `#[tokio::test]` is
    /// single-threaded, so a thread-local default subscriber sees the handler.
    #[derive(Clone, Default)]
    struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Logs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for Logs {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
        type Writer = Logs;
        fn make_writer(&'a self) -> Logs {
            self.clone()
        }
    }

    fn capture_logs() -> (Logs, tracing::subscriber::DefaultGuard) {
        let logs = Logs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish();
        (logs, tracing::subscriber::set_default(subscriber))
    }

    /// Revocation answers the client at once, not when the process dies. The
    /// module here never answers and is never killed: without the race against
    /// the revocation, the client waits for the 30s total deadline.
    #[tokio::test]
    async fn a_revoked_request_is_answered_at_once_not_when_the_process_dies() {
        let module = gated(Then::Answer).await;
        let generation = running_generation(module.addr.clone());
        let proxy = serve(mount(Router::new(), NS, Current::new(generation.clone()))).await;

        let first =
            tokio::spawn(
                async move { call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await },
            );
        module.wait_arrived().await;
        let _ = generation.revoke();

        let got = tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .expect("still waiting for the module 5s after its generation was revoked")
            .unwrap();
        assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(got.json()["error"]["code"], "request_abandoned");
    }

    /// A request revoked BEFORE it reached the module has a known outcome —
    /// nothing ran — and must not be reported, or logged, as "unknown".
    #[tokio::test]
    async fn a_request_revoked_before_it_reached_the_module_is_not_called_unknown() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (logs, _guard) = capture_logs();
        let module = gated(Then::Answer).await;
        let generation = running_generation(module.addr.clone());
        let proxy = serve(mount(Router::new(), NS, Current::new(generation.clone()))).await;

        // A client that promises ten bytes of body and sends one: the request is
        // admitted and stays in the body read, short of the module.
        let mut client = tokio::net::TcpStream::connect(proxy).await.unwrap();
        client
            .write_all(
                format!("POST {NS}/a HTTP/1.1\r\nhost: x\r\ncontent-length: 10\r\n\r\nx")
                    .as_bytes(),
            )
            .await
            .unwrap();
        // Establish "it is in flight" as a fact, through the generation itself.
        // (Not by starting a drain first: a drain that begins before the request
        // is admitted refuses it, and the test would be about something else.)
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while generation.in_flight() != 1 {
            assert!(
                std::time::Instant::now() < deadline,
                "the request was never admitted"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let _ = generation.revoke();

        let mut got = Vec::new();
        let mut buf = [0u8; 1024];
        let read_until = tokio::time::Instant::now() + Duration::from_secs(5);
        while !String::from_utf8_lossy(&got).contains('}') {
            match tokio::time::timeout_at(read_until, client.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => got.extend_from_slice(&buf[..n]),
                other => panic!(
                    "no complete response: {other:?} after {:?}",
                    String::from_utf8_lossy(&got)
                ),
            }
        }
        let got = String::from_utf8_lossy(&got);
        assert!(got.starts_with("HTTP/1.1 503"), "{got}");
        assert!(got.contains("module_stopping"), "{got}");
        assert!(!got.contains("request_abandoned"), "{got}");
        assert_eq!(module.dials.load(Ordering::SeqCst), 0);
        assert!(!logs.text().contains("unknown"), "{}", logs.text());
    }

    /// Admission comes before the concurrency ceiling. A module that is draining
    /// AND full must say "draining" — "overloaded" would tell the operator to
    /// wait for capacity on a module that is going away.
    #[tokio::test]
    async fn a_draining_module_that_is_also_full_says_draining_not_overloaded() {
        let module = gated(Then::Answer).await;
        let generation = running_generation(module.addr.clone());
        let (app, sem) =
            proxy_and_permits_for(Current::new(generation.clone()), Limits::default(), 1);
        let proxy = serve(app).await;

        let first =
            tokio::spawn(
                async move { call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await },
            );
        wait_for_permits(&sem, 0).await;
        assert!(generation.begin_drain(std::time::Instant::now(), Duration::from_secs(30)));

        let refused = call(proxy, Method::GET, &format!("{NS}/b"), &[], "").await;
        assert_eq!(refused.json()["error"]["code"], "module_draining");

        module.wait_arrived().await;
        module.release.notify_waiters();
        assert_eq!(first.await.unwrap().status, StatusCode::OK);
    }

    /// `forward` checks the revocation itself before sending, rather than
    /// relying on the `select!` in `proxy` to have dropped it. The `select!`
    /// alone does not close the window: when the client's last body byte and the
    /// revocation land in the same poll, `select!` may poll `forward` first, and
    /// it would dial the module after the revoke. So this calls `forward`
    /// directly on a request whose generation is already revoked.
    #[tokio::test]
    async fn forward_does_not_send_a_request_whose_generation_was_revoked() {
        let module = gated(Then::Answer).await;
        let generation = running_generation(module.addr.clone());
        let state = state_with(
            NS,
            Current::new(generation.clone()),
            Limits::default(),
            MAX_INFLIGHT_PER_MODULE,
        );
        let in_flight = generation
            .admit_request(
                "r-1".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let _ = generation.revoke();

        let uri: Uri = format!("{NS}/a").parse().unwrap();
        let request = Request::builder()
            .uri(uri.clone())
            .body(Body::empty())
            .unwrap();
        let got = forward(&state, &uri, request, "r-1", "test-token", &in_flight).await;

        assert_eq!(got.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = got.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("module_stopping"));
        assert_eq!(
            module.dials.load(Ordering::SeqCst),
            0,
            "sent after the revoke"
        );
    }

    /// A request body that yields nothing until the test opens `gate`, and says
    /// when it was first polled — so a test can establish "forward is inside the
    /// body read" as a fact rather than after a sleep.
    struct GatedBody {
        polled: Option<tokio::sync::oneshot::Sender<()>>,
        gate: Option<tokio::sync::oneshot::Receiver<()>>,
        sent: bool,
    }

    impl hyper::body::Body for GatedBody {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            use std::future::Future;
            use std::task::Poll;
            if let Some(tx) = self.polled.take() {
                let _ = tx.send(());
            }
            if self.sent {
                return Poll::Ready(None);
            }
            if let Some(gate) = self.gate.as_mut() {
                if std::pin::Pin::new(gate).poll(cx).is_pending() {
                    return Poll::Pending;
                }
                self.gate = None;
            }
            self.sent = true;
            Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from_static(
                b"late",
            )))))
        }
    }

    /// The window the `select!` in `proxy` cannot close alone: the client's last
    /// body byte and the revocation land together, and `forward` is polled first.
    /// Here `forward` runs on its own, inside the body read when the revocation
    /// lands; the body then completes. It must not dial the module.
    #[tokio::test]
    async fn a_revocation_during_the_body_read_stops_forward_from_sending() {
        let module = gated(Then::Answer).await;
        let generation = running_generation(module.addr.clone());
        let state = state_with(
            NS,
            Current::new(generation.clone()),
            Limits::default(),
            MAX_INFLIGHT_PER_MODULE,
        );
        let in_flight = generation
            .admit_request(
                "r-3".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let (polled_tx, polled_rx) = tokio::sync::oneshot::channel();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        let uri: Uri = format!("{NS}/a").parse().unwrap();
        let request = Request::builder()
            .method(Method::POST)
            .uri(uri.clone())
            .body(Body::new(GatedBody {
                polled: Some(polled_tx),
                gate: Some(gate_rx),
                sent: false,
            }))
            .unwrap();
        let task = tokio::spawn(async move {
            let got = forward(&state, &uri, request, "r-3", "test-token", &in_flight).await;
            (got, in_flight)
        });

        polled_rx
            .await
            .expect("forward never started reading the body");
        let _ = generation.revoke();
        gate_tx.send(()).unwrap();

        let (got, in_flight) = task.await.unwrap();
        assert_eq!(got.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = got.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("module_stopping"));
        assert_eq!(
            module.dials.load(Ordering::SeqCst),
            0,
            "sent after the revoke"
        );
        assert_eq!(in_flight.finish(), Err(Abandoned { dispatched: false }));
    }

    /// `finish` is the commit point. A 200 the module really wrote — after its
    /// generation was revoked — is not committed. Driven through `forward`
    /// directly so the race in `proxy` cannot cancel the read first: the
    /// precondition asserts the 200 actually arrived.
    #[tokio::test]
    async fn a_200_that_arrives_after_the_revocation_is_not_committed() {
        let module = gated(Then::Answer).await;
        let generation = running_generation(module.addr.clone());
        let state = state_with(
            NS,
            Current::new(generation.clone()),
            Limits::default(),
            MAX_INFLIGHT_PER_MODULE,
        );
        let in_flight = generation
            .admit_request(
                "r-4".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let uri: Uri = format!("{NS}/a").parse().unwrap();
        let request = Request::builder()
            .uri(uri.clone())
            .body(Body::empty())
            .unwrap();
        let task = tokio::spawn(async move {
            let got = forward(&state, &uri, request, "r-4", "test-token", &in_flight).await;
            (got, in_flight)
        });
        module.wait_arrived().await;
        let _ = generation.revoke();
        module.release.notify_waiters();

        let (got, in_flight) = task.await.unwrap();
        assert_eq!(
            got.status(),
            StatusCode::OK,
            "precondition: the module really answered 200"
        );
        assert_eq!(
            in_flight.finish(),
            Err(Abandoned { dispatched: true }),
            "a 200 that arrived after the revocation was committed"
        );
    }

    // ── FU-64 §A/B/C: the reuse race, its retry, and its structured 502 ──

    /// A module that answers every request, then closes the connection at
    /// once — no keep-alive. Judgement criteria 1/2's fixture; round 5 Low 2
    /// downgraded these to stress coverage, not the mutation-killing
    /// positive control (that is `window_b_*` above), since whether any
    /// given request actually lands in window (b) here depends on exact
    /// scheduling, not on anything this test controls.
    async fn closes_after_every_response() -> PathBuf {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let path = unique_sock("close-every");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let mut head = Vec::new();
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
                    if let Some(len) = text
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                    {
                        let header_end =
                            head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let mut have = head.len() - header_end;
                        while have < len {
                            match socket.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => have += n,
                            }
                        }
                    }
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await;
                    // Dropped here, at once — no keep-alive.
                });
            }
        });
        path
    }

    /// FU-64 judgement criterion 1 (stress coverage; see
    /// `closes_after_every_response`'s doc for why this is not the primary
    /// evidence). The design doc's own number (round 6 code review Low 2:
    /// the earlier 500 was an unjustified reduction) — every request here
    /// goes on its own fresh connection (nothing pooled long enough to
    /// matter to a module this aggressive), so N only affects how many
    /// independent trials of the same race this run happens to sample.
    #[tokio::test]
    async fn many_gets_against_a_module_that_always_closes_cost_no_502() {
        let upstream = closes_after_every_response().await;
        let proxy = serve(mount(Router::new(), NS, running_module(upstream))).await;
        for i in 0..2000 {
            let got = call(proxy, Method::GET, &format!("{NS}/{i}"), &[], "").await;
            assert_eq!(got.status, StatusCode::OK, "request {i}: {}", got.body);
        }
    }

    /// FU-64 judgement criterion 2 (stress coverage). Whichever of these
    /// happen to land in window (b) — possibly none, possibly all, it is not
    /// this test's to control — must come back with the structured fields,
    /// never the old blanket `upstream_unavailable`.
    #[tokio::test]
    async fn many_posts_against_a_module_that_always_closes_get_the_structured_502_when_they_do() {
        let upstream = closes_after_every_response().await;
        let proxy = serve(mount(Router::new(), NS, running_module(upstream))).await;
        for i in 0..200 {
            let got = call(proxy, Method::POST, &format!("{NS}/{i}"), &[], "body").await;
            if got.status == StatusCode::BAD_GATEWAY {
                assert_eq!(got.json()["error"]["code"], "upstream_connection_closed");
                assert!(
                    !got.json()["error"]["hint"]
                        .as_str()
                        .unwrap_or_default()
                        .is_empty()
                );
            } else {
                assert_eq!(got.status, StatusCode::OK, "request {i}: {}", got.body);
            }
        }
    }

    /// A module that closes the FIRST connection it ever gets, right after
    /// reading a complete request (head, and any declared body) — deep
    /// enough into window (b) that the write undeniably already landed, not
    /// window (a) (FU-64 judgement criterion 7, the deterministic positive
    /// control criteria 1/2 alone cannot be — round 5 Low 2). Every later
    /// connection is answered normally. `seen` counts connections that got a
    /// full request read from them, in arrival order.
    async fn vanishes_once_then_answers() -> (PathBuf, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let path = unique_sock("vanish-once");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let counted = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let seen = counted.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let mut head = Vec::new();
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
                    if let Some(len) = text
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                    {
                        let header_end =
                            head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let mut have = head.len() - header_end;
                        while have < len {
                            match socket.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => have += n,
                            }
                        }
                    }
                    if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                        drop(socket); // the first connection ever: vanish.
                        return;
                    }
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await;
                });
            }
        });
        (path, seen)
    }

    #[tokio::test]
    async fn window_b_get_is_saved_by_the_idempotent_retry() {
        let (upstream, seen) = vanishes_once_then_answers().await;
        let proxy = serve(mount(Router::new(), NS, running_module(upstream))).await;
        let got = call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await;
        assert_eq!(got.status, StatusCode::OK, "{}", got.body);
        assert_eq!(
            seen.load(Ordering::SeqCst),
            2,
            "expected one failed connection and one successful retry"
        );
    }

    /// Same fixture, a POST: not idempotent, so the design does not retry it
    /// — the client gets the structured `upstream_connection_closed` 502,
    /// and the module never sees a second copy of the request.
    #[tokio::test]
    async fn window_b_post_is_not_retried_and_gets_the_structured_502() {
        let (upstream, seen) = vanishes_once_then_answers().await;
        let proxy = serve(mount(Router::new(), NS, running_module(upstream))).await;
        let got = call(proxy, Method::POST, &format!("{NS}/a"), &[], "body").await;
        assert_eq!(got.status, StatusCode::BAD_GATEWAY, "{}", got.body);
        let j = got.json();
        assert_eq!(j["error"]["code"], "upstream_connection_closed");
        let hint = j["error"]["hint"].as_str().expect("hint present");
        assert!(hint.contains("side effect"), "{hint}");
        assert!(hint.contains("keep-alive"), "{hint}");
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "a non-idempotent request must not be sent twice"
        );
    }

    /// FU-64 §A, judgement criterion 8: the retry re-checks revocation
    /// immediately before its second physical send — a generation revoked
    /// while the first attempt is in flight must stop the retry from ever
    /// dialling a second connection. The client sees the pre-existing
    /// revocation path (`request_abandoned`), not a new
    /// `upstream_connection_closed` — `proxy()`'s `in_flight.finish()` check
    /// is authoritative once `dispatch()` has ever succeeded, which it did
    /// before the first attempt (design doc §A, round 2 Medium 1).
    #[tokio::test]
    async fn a_retry_is_not_sent_after_the_generation_is_revoked_mid_flight() {
        let module = gated(Then::Vanish).await;
        let generation = running_generation(module.addr.clone());
        let proxy = serve(mount(Router::new(), NS, Current::new(generation.clone()))).await;

        let task =
            tokio::spawn(
                async move { call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await },
            );
        module.wait_arrived().await;
        let _ = generation.revoke();
        module.release.notify_waiters();

        let got = task.await.unwrap();
        assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE, "{}", got.body);
        assert_eq!(got.json()["error"]["code"], Abandoned::CODE);
        assert_eq!(
            module.dials.load(Ordering::SeqCst),
            1,
            "the retry must not have dialled a second connection after revocation"
        );
    }

    /// The mechanism the test above relies on, in isolation and fully
    /// deterministic (code review round 1 Medium 2): the test above goes
    /// through the whole HTTP stack, where `proxy()`'s own
    /// `tokio::select!` against `in_flight.revoked()` can in principle win
    /// before `forward` ever reaches the retry's `send_guard` at all — a
    /// scheduling-dependent path to the same assertion, not a proof the
    /// guard itself works. This test calls `exchange`'s `force_fresh` tail
    /// directly with a guard that always says no, and proves the two things
    /// that matter about it without depending on any race: the connection is
    /// still dialled (`Upstream::connect` is not skipped — the guard runs
    /// AFTER it, narrowing the TOCTOU rather than avoiding the connect
    /// entirely, design doc §A code review round 1 High 1), but no bytes are
    /// ever written to it (the module only ever sees an accepted socket that
    /// goes silent, never a request head).
    #[tokio::test]
    async fn exchange_does_not_send_when_the_guard_withdraws_permission() {
        let path = unique_sock("send-guard-withheld");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        // Explicit signals the server side reports back, not counters the
        // test hopes have been updated by the time it looks (code review
        // round 2 Medium): `dial_rx` fires the instant `accept()` returns;
        // `read_rx` carries what the read actually observed — `true` if a
        // byte arrived, `false` if the peer (this test's `exchange` call)
        // closed the connection with nothing written, which is what happens
        // when `fresh` — and its socket — is dropped on `exchange`'s `Err`
        // return path below.
        let (dial_tx, dial_rx) = tokio::sync::oneshot::channel::<()>();
        let (read_tx, read_rx) = tokio::sync::oneshot::channel::<bool>();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let _ = dial_tx.send(());
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 1];
            let wrote = matches!(socket.read(&mut buf).await, Ok(n) if n > 0);
            let _ = read_tx.send(wrote);
        });

        let generation = running_generation(path.clone());
        let idle = IdleConnections::default();
        let req = Request::builder()
            .method(Method::GET)
            .uri("/a")
            .header(axum::http::header::HOST, UPSTREAM_HOST)
            .body(Full::new(Bytes::new()))
            .unwrap();

        // Bounded so a regression that hangs (rather than one that silently
        // sends) fails fast with a clear timeout, not a stuck test run.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            exchange(&idle, &generation, &path, req, true, None, Some(&|| false)),
        )
        .await
        .expect("exchange must not hang when the guard withholds permission");
        // `Upstream` (the `Ok` payload) has no `Debug` impl — describe by
        // hand rather than deriving one just for this assertion message.
        let describe = match &result {
            Ok(_) => "Ok(..)".to_owned(),
            Err(e) => format!("Err({e})"),
        };
        assert!(
            matches!(result, Err(ExchangeError::NotSent(_))),
            "expected NotSent when the guard withholds permission, got {describe}"
        );
        // `fresh` (and with it the client end of the socket) was already
        // dropped when `exchange` returned above, so the server side WILL
        // see a dial and then EOF — awaited as facts, not hoped for by
        // yielding and hoping the scheduler got to it.
        tokio::time::timeout(Duration::from_secs(5), dial_rx)
            .await
            .expect("the guard runs after connect, not instead of it — a dial must have happened")
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

    /// Window (a)'s deterministic test (FU-64 judgement criterion 3, closing
    /// the space FU-63/PR#184 left with only a comment for evidence): a
    /// close landing exactly between `take()` handing back a pooled
    /// connection and `exchange` trying to send on it — not before `take()`
    /// (already covered by `a_module_closing_an_idle_connection_costs_no_502`)
    /// — still costs the client nothing: `try_send_request` hands the
    /// request back unsent and `exchange` sends it once more on a fresh
    /// connection, transparently, without even reaching FU-64's own new
    /// retry logic in `forward`.
    #[tokio::test]
    async fn window_a_close_between_take_and_send_is_still_saved() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let path = unique_sock("window-a");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let close = Arc::new(tokio::sync::Notify::new());
        let (closed_tx, mut closed) = tokio::sync::mpsc::unbounded_channel::<()>();
        {
            let close = close.clone();
            tokio::spawn(async move {
                let mut first = true;
                while let Ok((mut socket, _)) = listener.accept().await {
                    let (close, closed_tx, is_first) = (close.clone(), closed_tx.clone(), first);
                    first = false;
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let mut head = Vec::new();
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match socket.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buf[..n]),
                            }
                        }
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                            .await;
                        if is_first {
                            close.notified().await;
                            drop(socket);
                            let _ = closed_tx.send(());
                        }
                        // The second (retry) connection: left open, unused.
                    });
                }
            });
        }

        let generation = running_generation(path.clone());
        let idle = IdleConnections::default();

        // Warm a connection into the pool exactly the way `forward` would.
        let warm_req = Request::builder()
            .method(Method::GET)
            .uri("/a")
            .header(axum::http::header::HOST, UPSTREAM_HOST)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let mut warm = Upstream::connect(generation.clone(), &path).await.unwrap();
        let resp = warm.sender.send_request(warm_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let _ = resp.into_body().collect().await;
        // Deterministically ready for a next request, the same thing
        // production waits for before pooling a connection (`forward`,
        // under `IDLE_SETTLE`) — not just "probably ready because the body
        // was drained" (code review round 1 Medium 3c).
        warm.sender.ready().await.unwrap();
        idle.put(warm);

        // The second request: gate `exchange` right after `take()`, close
        // the module's end of exactly that connection, confirm the OS-level
        // close, only then release `exchange` into `try_send_request`.
        let ready = tokio::sync::Notify::new();
        let proceed = tokio::sync::Notify::new();
        let gate = TakeSendGate {
            ready: &ready,
            proceed: &proceed,
        };
        let req2 = Request::builder()
            .method(Method::GET)
            .uri("/b")
            .header(axum::http::header::HOST, UPSTREAM_HOST)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let attempt = exchange(&idle, &generation, &path, req2, false, Some(&gate), None);
        tokio::pin!(attempt);
        tokio::select! {
            r = &mut attempt => panic!("exchange returned before the gate released it: {:?}", r.is_ok()),
            () = ready.notified() => {}
        }
        close.notify_one();
        tokio::time::timeout(Duration::from_secs(5), closed.recv())
            .await
            .expect("the module never closed the connection")
            .unwrap();
        // `closed.recv()` only confirms the SERVER's end is gone. Whether
        // hyper's client-side driver has processed the resulting EOF yet is
        // not observable from OUT HERE — by this point `take()` has already
        // moved the connection out of `idle` and into `exchange`'s own local
        // `reused`, so there is no external handle left to poll (code review
        // round 1 Medium 3b; round 2 Medium: a fixed yield count here was
        // still a race, not a proof). `exchange` itself polls its own
        // `reused.sender.is_closed()` before proceeding to
        // `try_send_request` when a gate is attached (see its own comment) —
        // this call just releases it to go do that.
        proceed.notify_one();

        let (response, _connection) = tokio::time::timeout(Duration::from_secs(5), attempt)
            .await
            .expect("exchange never returned")
            .expect("the transparent unsent-retry should have saved this request");
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// FU-64 §B, judgement criterion 4: a connection idle longer than
    /// `max_age` is treated as already dead and dropped on `take()` — even
    /// though it is not actually closed (`is_closed()` is still `false`).
    #[tokio::test]
    async fn an_idle_connection_older_than_max_age_is_not_taken() {
        let (upstream, _peers) = peer_counting_upstream().await;
        let generation = running_generation(upstream.clone());
        let idle = IdleConnections::with_max_age(Duration::from_millis(20));
        let warmed = Upstream::connect(generation.clone(), &upstream)
            .await
            .unwrap();
        idle.put(warmed);
        assert!(
            !idle.lock()[0].sender.is_closed(),
            "precondition: the connection is still alive, only old"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            idle.take(&generation).is_none(),
            "a connection older than max_age must not be handed out"
        );
        assert!(idle.lock().is_empty(), "it should have been dropped too");
    }

    // ── ERR-1 §D/§E: every "can't reach the module" code carries a hint ──

    /// A `Current` whose generation is revoked, with `status` attached at
    /// the given value — the minimal fixture `stopping_response` needs,
    /// without spinning up a real `supervise()` loop (`attach_status` is
    /// `pub(crate)`, reachable from this same crate's tests).
    fn revoked_current_with_status(status: crate::supervisor::Status) -> Arc<Current> {
        let generation = running_generation(unique_sock("d-status"));
        let current = Current::new(generation.clone());
        let (_tx, rx) = tokio::sync::watch::channel(status);
        current.attach_status(rx);
        let _ = generation.revoke();
        current
    }

    fn hint_of(r: Response) -> (StatusCode, String, String) {
        let status = r.status();
        // `error_response_with_hint` always serializes `hint` when it was
        // given one — these tests only ever call it that way.
        let body = futures_body_to_json(r);
        (
            status,
            body["error"]["code"].as_str().unwrap().to_owned(),
            body["error"]["hint"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    }

    /// Companion to `hint_of` for the tests that need `message` too (code
    /// review round 2 Low: proving a control-plane phrase does NOT leak into
    /// the proxy-path `message` needs to actually look at `message`).
    fn message_of(r: Response) -> String {
        futures_body_to_json(r)["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn futures_body_to_json(r: Response) -> serde_json::Value {
        // Small enough (an error envelope) to collect synchronously via a
        // throwaway runtime — every caller here is itself inside a
        // `#[tokio::test]`, so `block_in_place`/nesting is not an option;
        // `futures::executor` is not a dependency, so drive it by hand.
        let (_, body) = r.into_parts();
        let bytes = futures_lite_collect(body);
        serde_json::from_slice(&bytes).unwrap()
    }

    fn futures_lite_collect(body: Body) -> Bytes {
        // A response built by `error_response`/`error_response_with_hint` is
        // always a single already-buffered `Json` body — collecting it never
        // actually awaits I/O, so blocking the current (tokio) thread on it
        // with a tiny dedicated runtime is safe and avoids making every
        // caller `async` just to call `.collect()`.
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async { body.collect().await.unwrap().to_bytes() })
    }

    use crate::supervisor::Status;

    #[test]
    fn stopping_status_does_not_promise_an_outcome() {
        let current = revoked_current_with_status(Status::Stopping);
        let (status, code, hint) = hint_of(stopping_response(&current));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "module_stopping");
        assert!(!hint.is_empty());
        // Deliberately uncommitted — must not read as either "will restart"
        // or "gone for good" (round 2 High 2's whole point).
        assert!(hint.contains("may"), "{hint}");
    }

    #[test]
    fn gave_up_reports_the_breaker_and_never_suggests_os_enable() {
        let current = revoked_current_with_status(Status::GaveUp {
            failures: 5,
            within: Duration::from_secs(60),
            last: crate::failure::RunFailure::new(
                crate::failure::FailureKind::Exited,
                "exited with code 1",
            ),
        });
        let (status, code, hint) = hint_of(stopping_response(&current));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "circuit_breaker_tripped");
        assert!(
            !hint.contains("agent24 os enable"),
            "a GaveUp module cannot be revived by `os enable` alone (round 1 High 5): {hint}"
        );
        assert!(hint.contains("agent24 service status"), "{hint}");
    }

    #[test]
    fn package_changed_names_the_reason_and_does_not_copy_control_plane_text() {
        let current = revoked_current_with_status(Status::PackageChanged {
            reason: "manifest digest mismatch".to_owned(),
        });
        let (status, code, hint) = hint_of(stopping_response(&current));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "package_changed");
        assert!(hint.contains("agent24 service status"), "{hint}");
        // The name says "does not copy control-plane text" — code review
        // round 2 Low pointed out this test never actually looked at
        // `message` to prove it. It must name the reason (the one thing a
        // proxy-path caller should be told) and must NOT carry
        // `os_routes.rs`'s control-plane phrasing, which is either
        // inseparable from the old unsafe restart command (`PackageChanged`)
        // or false for a passively-hit proxied request (`this request
        // disabled ... in os.json` — nobody sent a disable request here).
        let message = message_of(stopping_response(&current));
        assert!(message.contains("manifest digest mismatch"), "{message}");
        assert!(!message.contains("os.json"), "{message}");
        assert!(!message.contains("this request"), "{message}");
    }

    /// The regression FU-61 判据 6 actually names: a real request proxied
    /// through the mounted router at a `PackageChanged` module, not just a
    /// direct call to `stopping_response` (code review round 1 Low 2b — the
    /// unit test above exercises the classification, this one exercises the
    /// wiring that gets a request there at all: `admit_request` refusing on
    /// the revoked generation, `refused_response` reading `Current`'s
    /// attached status, all through one HTTP round trip).
    #[tokio::test]
    async fn a_proxied_request_to_a_package_changed_module_gets_the_real_response() {
        let current = revoked_current_with_status(Status::PackageChanged {
            reason: "manifest digest mismatch".to_owned(),
        });
        let proxy = serve(mount(Router::new(), NS, current)).await;
        let got = call(proxy, Method::GET, &format!("{NS}/a"), &[], "").await;
        assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE, "{}", got.body);
        let j = got.json();
        assert_eq!(j["error"]["code"], "package_changed");
        assert!(
            j["error"]["hint"]
                .as_str()
                .unwrap_or_default()
                .contains("agent24 service status"),
            "{}",
            got.body
        );
    }

    #[test]
    fn stop_failed_message_is_proxy_specific_not_the_patch_endpoints_text() {
        let current = revoked_current_with_status(Status::StopFailed {
            error: "group not confirmed gone".to_owned(),
        });
        let r = stopping_response(&current);
        let status = r.status();
        let body = futures_body_to_json(r);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "stop_failed");
        let message = body["error"]["message"].as_str().unwrap();
        // Round 5 Medium 3: the PATCH endpoint's own text says "this request
        // disabled ... in os.json" — false for a passively-hit proxy request.
        assert!(!message.contains("os.json"), "{message}");
        assert!(!message.contains("this request"), "{message}");
        let hint = body["error"]["hint"].as_str().unwrap();
        assert!(hint.contains("agent24 service status"), "{hint}");
    }

    #[test]
    fn panicked_and_killed_each_get_their_own_code() {
        let (status, code, hint) = hint_of(stopping_response(&revoked_current_with_status(
            Status::Panicked,
        )));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "module_panicked");
        assert!(!hint.is_empty());

        let (status, code, hint) = hint_of(stopping_response(&revoked_current_with_status(
            Status::Killed,
        )));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "module_killed");
        assert!(!hint.is_empty());
    }

    #[test]
    fn stopped_hint_covers_both_enable_and_restart() {
        let current = revoked_current_with_status(Status::Stopped);
        let (status, code, hint) = hint_of(stopping_response(&current));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        // FU-61 判据 6 / round 2 High 1's regression: this must no longer be
        // an undifferentiated `module_stopping` with no hint.
        assert_eq!(code, "module_stopping");
        assert!(
            hint.contains("agent24 os enable"),
            "round 2 High 3: enabling alone is not suffient, but must be mentioned: {hint}"
        );
        assert!(hint.contains("agent24 service status"), "{hint}");
    }

    /// The narrow fallback (round 3 High 2): a `Status` this classification
    /// does not recognize — paired with an already-revoked generation, the
    /// way `PackageChanged`'s own `retire()`-then-`send_replace()` briefly
    /// can — reports today's plain `module_stopping`, with a non-empty,
    /// honest hint, never a panic and never a guess.
    #[test]
    fn an_unrecognized_status_falls_back_without_guessing() {
        let current = revoked_current_with_status(Status::Running);
        let (status, code, hint) = hint_of(stopping_response(&current));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "module_stopping");
        assert!(!hint.is_empty());
    }

    #[test]
    fn not_ready_and_draining_carry_hints_too() {
        let current = Current::new(Generation::starting());
        let (status, code, hint) = hint_of(refused_response(RequestRefused::NotReady, &current));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "module_not_ready");
        assert!(!hint.is_empty());

        let (status, code, hint) = hint_of(refused_response(RequestRefused::Draining, &current));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "module_draining");
        assert!(!hint.is_empty());
        // Round 3 High 1: must not promise a next generation that a
        // deliberate stop (the only real path into `Draining`) never has.
        assert!(
            !hint.contains("next generation"),
            "must not promise an automatic restart: {hint}"
        );
    }

    /// Judgement criterion 11: the six responses that tell an operator to
    /// restart the daemon all share the exact same instruction fragment —
    /// not six hand-copied near-duplicates (round 4/5 Medium findings).
    #[test]
    fn restart_advice_shares_one_instruction_across_every_d_section_code() {
        for status in [
            Status::GaveUp {
                failures: 1,
                within: Duration::from_secs(1),
                last: crate::failure::RunFailure::new(crate::failure::FailureKind::Exited, "x"),
            },
            Status::PackageChanged {
                reason: "x".to_owned(),
            },
            Status::StopFailed {
                error: "x".to_owned(),
            },
            Status::Stopped,
        ] {
            let hint = hint_of(stopping_response(&revoked_current_with_status(status))).2;
            assert!(
                hint.contains(RESTART_DAEMON_INSTRUCTION),
                "hint does not contain the shared instruction verbatim: {hint}"
            );
        }
        assert!(RESTART_DAEMON_INSTRUCTION.contains("agent24 service status"));
        // Round 5 Medium 2 fixed the placeholder `<label>`; round 6 Low 3a:
        // assert the complete, actually-runnable command — the real service
        // label (`service.rs`'s `LABEL`), not just the `gui/$(id -u)/` prefix
        // a hand-typed near-duplicate could also satisfy.
        assert!(
            RESTART_DAEMON_INSTRUCTION
                .contains("launchctl kickstart -k gui/$(id -u)/ai.auraai.agent24")
        );
        assert!(RESTART_DAEMON_INSTRUCTION.contains("loaded: no"));
    }

    // ── ME4-1.3.2 — the reserved path judgement (design §7, `judge`) ─────
    //
    // §7.1: `judge` runs at the very front of `proxy()`, before minting a
    // request id or calling `admit_request` — a path judged `Reserved` or
    // `Rejected` never touches the module. The pure-function tests below
    // port the design's reference implementation verbatim
    // (`docs/design/ME4-S1-scheduler-callback.md` §7.2, scratch
    // `reserved_path.rs`) together with its C3.1/C3.2/C3.3 variant matrices
    // (design §11); the wired tests after them prove the same claims
    // end-to-end through the real router: the mock upstream sees zero
    // requests for a reserved/rejected path, and the untouched raw path for
    // a forwarded one.
    mod reserved_path {
        use super::*;

        // ---- C3.1: reserved variants — 404 `not_found`, never forwarded --
        const RESERVED: &[&str] = &[
            "/_a24/scheduler/fired",
            "/_A24/scheduler/fired",
            "//_a24/scheduler/fired",
            "/%5fa24/scheduler/fired",
            "/%5Fa24/scheduler/fired",
            "/%5F%61%32%34/scheduler/fired",
            "/%255fa24/scheduler/fired",
            "/%25255fa24/scheduler/fired",
            "/_a24;x=1/scheduler/fired",
            "/_a24%3Fq/scheduler/fired",
            "/_a24%23f/scheduler/fired",
            "/_a24",
            "/_a24/",
            // design v2, H2
            "/_a24./scheduler/fired",
            "/_a24%20/scheduler/fired",
            "/_a24.%20./scheduler/fired",
        ];

        // ---- C3.2: cannot be canonicalised — 400 `invalid_request_path`,
        // never forwarded -----------------------------------------------
        const REJECTED: &[&str] = &[
            "/x%2F..%2F_a24/scheduler/fired", // encoded slash
            "/x%2f..%2f_a24/scheduler/fired",
            "/x%252F..%252F_a24/scheduler/fired", // double-encoded slash
            "/x%5C..%5C_a24/scheduler/fired",     // encoded backslash
            "/x\\..\\_a24/scheduler/fired",       // literal backslash
            "/_a24%",                             // illegal %
            "/_a24%zz/x",                         // illegal %
            "/%C0%AF_a24",                        // overlong UTF-8
            "/%00_a24",                           // NUL
            "/../x/_a24",                         // escapes the namespace root
            "/%2e%2e/admin",
            // design v3 (H-B): every dot segment is 400, wherever it sits
            "/./_a24/scheduler/fired",
            "/x/../_a24/scheduler/fired",
            "/x/%2e%2e/_a24/scheduler/fired",
            "/x/.%2E/_a24/scheduler/fired",
            "/x/..;/_a24/scheduler/fired",
            "/x/..%3B/_a24/scheduler/fired",
            "/x/..;a=b/_a24/scheduler/fired",
            "/.;/_a24/scheduler/fired",
            "/_a24/scheduler/fired/../../..",
            "/_a24/..",
            "/_a24/../x",
            "/_a24/%2e%2e",
            "/_a24;/..",
            "/x/..%20/_a24",
            "/x/.../_a24",
            "/routines/./today",
            // design v3.1, L1: the dot-segment cut point matches the
            // first-segment cut point — `;`, `?`, `#` alike.
            "/x/..%3F/_a24",
            "/x/..%23frag/_a24",
            // design v3.1, L2: a non-empty all-space segment is a dot
            // segment too — an IIS-style server strips it to empty, same
            // meaning as `//`.
            "/x/%20%20/y",
            // Pathological nesting (§7.2's "4 轮后仍在变 → 拒"): rejected NOT
            // because it settles to `..` — traced by hand, it never does.
            // `%252525252e` needs a 5th decode round to finish unwrapping
            // (`%2525252e` → `%25252e` → `%252e` → `%2e`, still `%2e` — not
            // `.` — after the 3 extra rounds `canonical_segment` allows); one
            // more decode of `%2e` still changes it (to `.`), so the fixed-
            // point check fails and the WHOLE segment is refused on that
            // basis alone, independent of whatever it might eventually
            // decode to.
            "/x/%252525252e/_a24",
        ];

        // ---- C3.3: positive controls — forwarded, path untouched --------
        const FORWARD: &[&str] = &[
            "",
            "/",
            "/anything",
            "/a24x",
            "/_a24x/y",
            "/_a25/scheduler/fired",
            "/x/_a24/scheduler/fired", // `_a24` is not the FIRST segment
            "/routines/%E4%BD%A0%E5%A5%BD",
            "/a%20b",
            "/x/..a/y",  // `..a` is a name, not `..`
            "/x/a.b/c.", // dots inside / at the end of a name
            "/_a24x./y",
        ];

        #[test]
        fn c3_1_reserved_variants_are_all_404() {
            for p in RESERVED {
                let full = format!("{NS}{p}");
                assert_eq!(judge(NS, &full), PathVerdict::Reserved, "{full}");
            }
        }

        #[test]
        fn c3_2_not_canonicalisable_variants_are_all_400() {
            for p in REJECTED {
                let full = format!("{NS}{p}");
                assert_eq!(judge(NS, &full), PathVerdict::Rejected, "{full}");
            }
        }

        #[test]
        fn c3_3_positive_controls_forward() {
            for p in FORWARD {
                let full = format!("{NS}{p}");
                assert_eq!(judge(NS, &full), PathVerdict::Forward, "{full}");
            }
        }

        /// Rule 1 (§7.2): the path must start with `namespace`, and what
        /// follows must be empty or start with `/`. Unreachable through
        /// axum's own `nest` in production (it only ever calls `proxy` for
        /// paths that already match byte-exactly), so `judge` is a general
        /// function that has to fail closed here on its own — this is the
        /// only place that branch is exercised directly.
        #[test]
        fn rule1_path_must_start_with_namespace_then_be_empty_or_slash() {
            // `namespace` is a byte-exact PREFIX of the raw path, but the very
            // next byte is neither end-of-string nor `/` — e.g. a sibling
            // namespace `/api/v1/zzmockx` that happens to start with ours.
            assert_eq!(
                judge(NS, &format!("{NS}x")),
                PathVerdict::Rejected,
                "namespace prefix not followed by '/' or end of string"
            );
            // Does not start with `namespace` at all.
            assert_eq!(
                judge(NS, "/other"),
                PathVerdict::Rejected,
                "raw path outside the namespace entirely"
            );
        }

        // ---- C3.4 / C3.5: reproducible mutation testing ------------------
        //
        // This repo has no mutation-testing harness wired into `cargo test`,
        // so each mutation below is a small, LOCAL reimplementation of
        // `judge` with exactly one behaviour changed — never the real
        // `judge` itself — checked against the SAME `RESERVED`/`REJECTED`
        // tables the real matrices above use. Each mutant function is
        // deliberately dumb: it does not reuse the piece of `judge` it is
        // mutating, only the pieces it is not (e.g. `canonical_segment` for
        // decoding, when the mutation is only about how a dot segment is
        // judged afterward).

        /// C3.4a mutant: judge the RAW, still percent-encoded path with a
        /// naive `starts_with` — the check an implementer might reach for
        /// before realising why `judge` decodes and canonicalises at all.
        fn judge_naive_starts_with(namespace: &str, raw_path: &str) -> PathVerdict {
            let Some(rest) = raw_path.strip_prefix(namespace) else {
                return PathVerdict::Rejected;
            };
            if rest == "/_a24" || rest.starts_with("/_a24/") {
                PathVerdict::Reserved
            } else {
                PathVerdict::Forward
            }
        }

        /// C3.4b mutant's segment canonicaliser: like `canonical_segment`,
        /// but never calls `reject_structural` — a slash unwrapped at any
        /// decode depth (`%252F` → `%2F` → `/`) survives into the
        /// "canonical" segment instead of being refused.
        fn canonical_segment_no_slash_check(raw: &str) -> Option<String> {
            if raw.bytes().any(|b| b == b'\\') {
                return None;
            }
            let mut cur = decode_percent(raw, true)?;
            for _ in 0..EXTRA_DECODE_ROUNDS {
                let next = decode_percent(&cur, false)?;
                if next == cur {
                    return Some(cur);
                }
                cur = next;
            }
            (decode_percent(&cur, false)? == cur).then_some(cur)
        }

        /// C3.4b mutant: `judge`, with every per-round `/`/`\`/control-char
        /// check dropped.
        fn judge_no_slash_check(namespace: &str, raw_path: &str) -> PathVerdict {
            let Some(rest) = raw_path.strip_prefix(namespace) else {
                return PathVerdict::Rejected;
            };
            if !(rest.is_empty() || rest.starts_with('/')) {
                return PathVerdict::Rejected;
            }
            let mut stack: Vec<String> = Vec::new();
            for raw in rest.split('/') {
                let Some(seg) = canonical_segment_no_slash_check(raw) else {
                    return PathVerdict::Rejected;
                };
                let base = seg.split([';', '?', '#']).next().unwrap_or("");
                if !base.is_empty() && base.chars().all(|c| c == '.' || c == ' ') {
                    return PathVerdict::Rejected;
                }
                if base.is_empty() {
                    continue;
                }
                stack.push(seg);
            }
            match stack.first() {
                Some(first) if is_reserved(first) => PathVerdict::Reserved,
                _ => PathVerdict::Forward,
            }
        }

        /// C3.5a mutant: RESOLVE a dot segment (pop `..`, drop `.`) instead
        /// of rejecting the whole path — the exact v2 bug (design H-B): the
        /// kernel forwards the RAW path, so resolving `..` only in the
        /// kernel's own view lets `_a24/scheduler/fired/../../..` (first
        /// canonical segment after resolving: none) or `_a24/..` through to
        /// a module that prefix-matches `/_a24/…`.
        fn judge_resolves_dots_in_kernel_view(namespace: &str, raw_path: &str) -> PathVerdict {
            let Some(rest) = raw_path.strip_prefix(namespace) else {
                return PathVerdict::Rejected;
            };
            if !(rest.is_empty() || rest.starts_with('/')) {
                return PathVerdict::Rejected;
            }
            let mut stack: Vec<String> = Vec::new();
            for raw in rest.split('/') {
                let Some(seg) = canonical_segment(raw) else {
                    return PathVerdict::Rejected;
                };
                let base = seg.split([';', '?', '#']).next().unwrap_or("");
                if base == "." {
                    continue;
                }
                if base == ".." {
                    stack.pop();
                    continue;
                }
                if base.is_empty() {
                    continue;
                }
                stack.push(seg);
            }
            match stack.first() {
                Some(first) if is_reserved(first) => PathVerdict::Reserved,
                _ => PathVerdict::Forward,
            }
        }

        /// C3.5b mutant: the dot-segment check cuts only at `;`, not at
        /// `;`/`?`/`#` — reintroduces the pre-v3.1 inconsistency (design
        /// L1) between this check and the first-segment reserved check.
        fn judge_dotseg_cuts_only_semicolon(namespace: &str, raw_path: &str) -> PathVerdict {
            let Some(rest) = raw_path.strip_prefix(namespace) else {
                return PathVerdict::Rejected;
            };
            if !(rest.is_empty() || rest.starts_with('/')) {
                return PathVerdict::Rejected;
            }
            let mut stack: Vec<String> = Vec::new();
            for raw in rest.split('/') {
                let Some(seg) = canonical_segment(raw) else {
                    return PathVerdict::Rejected;
                };
                let base = seg.split(';').next().unwrap_or("");
                if !base.is_empty() && base.chars().all(|c| c == '.' || c == ' ') {
                    return PathVerdict::Rejected;
                }
                if base.is_empty() {
                    continue;
                }
                stack.push(seg);
            }
            match stack.first() {
                Some(first) if is_reserved(first) => PathVerdict::Reserved,
                _ => PathVerdict::Forward,
            }
        }

        /// C3.5c mutant's first-segment check: like `is_reserved`, but never
        /// trims a trailing `.`/space — reintroduces the pre-v2 bug (design
        /// H2): `_a24.` and `_a24%20` stop matching `_a24`.
        fn is_reserved_no_trim(segment: &str) -> bool {
            let head = segment.split([';', '?', '#']).next().unwrap_or("");
            head.eq_ignore_ascii_case(RESERVED_SEGMENT)
        }

        /// C3.5c mutant: `judge`, with the first segment's trailing-dot/space
        /// trim dropped.
        fn judge_first_segment_not_trimmed(namespace: &str, raw_path: &str) -> PathVerdict {
            let Some(rest) = raw_path.strip_prefix(namespace) else {
                return PathVerdict::Rejected;
            };
            if !(rest.is_empty() || rest.starts_with('/')) {
                return PathVerdict::Rejected;
            }
            let mut stack: Vec<String> = Vec::new();
            for raw in rest.split('/') {
                let Some(seg) = canonical_segment(raw) else {
                    return PathVerdict::Rejected;
                };
                let base = seg.split([';', '?', '#']).next().unwrap_or("");
                if !base.is_empty() && base.chars().all(|c| c == '.' || c == ' ') {
                    return PathVerdict::Rejected;
                }
                if base.is_empty() {
                    continue;
                }
                stack.push(seg);
            }
            match stack.first() {
                Some(first) if is_reserved_no_trim(first) => PathVerdict::Reserved,
                _ => PathVerdict::Forward,
            }
        }

        /// A mutant `judge`, by name.
        type MutantJudge = fn(&str, &str) -> PathVerdict;

        /// For every mutant above: somewhere in `RESERVED ∪ REJECTED` there
        /// must be a case where the mutant disagrees with the real `judge`
        /// AND the mutant's (wrong) answer is `Forward` — i.e. the mutation
        /// would let something reach the module that must not. A future
        /// refactor that reintroduces any of these five bugs turns this test
        /// red, not just the doc comment that used to be here.
        #[test]
        fn c3_4_and_c3_5_mutations_are_caught_by_the_matrix() {
            let all_cases: Vec<String> = RESERVED
                .iter()
                .chain(REJECTED.iter())
                .map(|p| format!("{NS}{p}"))
                .collect();
            let mutants: &[(&str, MutantJudge)] = &[
                ("C3.4a naive starts_with", judge_naive_starts_with),
                ("C3.4b no per-round / check", judge_no_slash_check),
                (
                    "C3.5a resolve dots in kernel view",
                    judge_resolves_dots_in_kernel_view,
                ),
                (
                    "C3.5b dot segment cuts only at ;",
                    judge_dotseg_cuts_only_semicolon,
                ),
                (
                    "C3.5c first segment not trimmed",
                    judge_first_segment_not_trimmed,
                ),
            ];
            for (name, mutant) in mutants {
                let killed = all_cases.iter().any(|full| {
                    let real = judge(NS, full);
                    let got = mutant(NS, full);
                    got != real && got == PathVerdict::Forward
                });
                assert!(
                    killed,
                    "mutation `{name}` survived the RESERVED/REJECTED matrix — \
                     no case in it flips to Forward under this mutant"
                );
            }
        }

        /// The r3-review table (`scratchpad/r3-review/src/rp.rs`, `requested`
        /// test) — extra adversarial variants a second reviewer proposed on
        /// top of C3.1–C3.3. The original only printed verdicts for manual
        /// inspection; the expected verdicts below were computed by running
        /// this exact `judge` against each case (not guessed) and hand-
        /// checked against §7.2's rules before being pinned here.
        #[test]
        fn review_r3_requested_variants() {
            let cases: &[(&str, PathVerdict)] = &[
                // Any dot segment, in every encoded form — Rejected.
                ("/_a24/scheduler/fired/../../..", PathVerdict::Rejected),
                ("/_a24/..", PathVerdict::Rejected),
                ("/_a24/../x", PathVerdict::Rejected),
                ("/_a24/%2e%2e", PathVerdict::Rejected),
                ("/_a24;/..", PathVerdict::Rejected),
                ("/x/..%20/_a24/scheduler/fired", PathVerdict::Rejected),
                ("/x/.../_a24/scheduler/fired", PathVerdict::Rejected),
                ("/x/.%2e/_a24/scheduler/fired", PathVerdict::Rejected),
                ("/x/%2e./_a24/scheduler/fired", PathVerdict::Rejected),
                ("/x/..;a=b/_a24/scheduler/fired", PathVerdict::Rejected),
                // Nested `%25`-encodings that DO settle to the fixed point
                // `..` within the 3 extra decode rounds `canonical_segment`
                // allows (traced by hand: `%252e%252e` → `%2e%2e` → `..`,
                // two rounds; `%25252e%25252e` needs three) — genuine dot
                // segments, rejected on that basis.
                ("/x/%252e%252e/_a24/scheduler/fired", PathVerdict::Rejected),
                (
                    "/x/%25252e%25252e/_a24/scheduler/fired",
                    PathVerdict::Rejected,
                ),
                (
                    "/x/%2525252e%2525252e/_a24/scheduler/fired",
                    PathVerdict::Rejected,
                ),
                // NOT in the "settles to `..`" group above: `%252525252e`
                // needs a 5th round to finish unwrapping and is rejected as
                // pathological nesting instead — see the dedicated, labelled
                // case in `REJECTED` (C3.2) for the traced-by-hand reasoning.
                ("/x/%252525252e/_a24", PathVerdict::Rejected),
                // `//` and a segment that is only `;params` are skipped, not
                // pushed — so `_a24` right after one is still the effective
                // first segment.
                ("//_a24/scheduler/fired", PathVerdict::Reserved),
                ("/_a24/scheduler/fired/", PathVerdict::Reserved),
                ("/;x/_a24/scheduler/fired", PathVerdict::Reserved),
                ("/%3B/_a24/scheduler/fired", PathVerdict::Reserved),
                // More `;`/`?`/`#` cut-point and trailing-space/dot forms of
                // a dot segment.
                ("/x/..%3F/_a24", PathVerdict::Rejected),
                ("/x/..%23/_a24", PathVerdict::Rejected),
                ("/x/..%3f/../_a24", PathVerdict::Rejected),
                ("/x/%2e%2e%3b/_a24", PathVerdict::Rejected),
                ("/x/.%20./_a24", PathVerdict::Rejected),
                ("/x/..%09/_a24", PathVerdict::Rejected),
                ("/ /_a24", PathVerdict::Rejected),
                ("/x/.. /_a24", PathVerdict::Rejected),
                ("/x/%2e%2e%2f_a24", PathVerdict::Rejected),
                ("/x/..%00/_a24", PathVerdict::Rejected),
                // IIS's legacy `%uXXXX` form is not `%XX` — `%u0` is `%`
                // followed by `u`, and `u` is not a hex digit, so the
                // STRICT round-1 decode rejects the whole segment outright
                // (rule 2's "非法 % 序列即拒") before there is any question
                // of what `%u002e` might otherwise mean.
                ("/x/%u002e%u002e/_a24", PathVerdict::Rejected),
                // `_a24` with trailing dots from `%2e%2e` — a NAME (not a
                // dot segment, since it also has letters), still recognised
                // as reserved after the trailing-dot trim (rule 4).
                ("/_a24%2e%2e/x", PathVerdict::Reserved),
                // An encoded slash inside the segment — rejected regardless
                // of what surrounds it (rule 2).
                ("/_a24%3B%2F../x", PathVerdict::Rejected),
                // `..%2525` DOES reach a fixed point within budget — traced
                // by hand: `..%2525` → `..%25` → `..%`, and decoding `..%`
                // again is a no-op, so `canonical_segment` stops there. `..%`
                // is not a dot segment (it has a non-dot, non-space byte,
                // the literal `%`), so `judge` pushes it as an ordinary
                // segment name and `_a24` is not first. That is NOT the
                // safety argument, though: this crate never resolves a dot
                // segment (rule 3 rejects the whole path instead of
                // popping), so nothing here could have promoted `_a24` to
                // first position regardless of how `..%` canonicalises —
                // the `x` ahead of it is itself an ordinary, permanently
                // stack-resident segment. What `Forward` here actually
                // rests on is the residual risk this design accepts and
                // names (R10): `judge` does not know whether some module's
                // OWN framework would decode `..%2525` further (to `..%25`,
                // `..%`, or beyond) and treat THAT as `..` once the raw path
                // reaches it unchanged — `review_r3_fuzz_reduced` below
                // checks this Forward verdict against 80 plausible
                // module-side normalisers and finds none that do, but a
                // model outside that 80 is exactly R10's disclosed gap, not
                // something this test can rule out.
                ("/x/..%2525/_a24", PathVerdict::Forward),
                // Ideographic full stop (U+3002, `。`), not ASCII `.` or
                // space — `judge` never treats it as dot-segment material,
                // by design (R2: non-ASCII lookalikes are explicitly out of
                // scope, not silently assumed safe). As above, `_a24` not
                // being first here is a consequence of `judge` never
                // resolving segments at all, not of this segment being
                // "harmless" in some absolute sense — a module that itself
                // folds `。` to `.` before routing would see something this
                // judgement did not predict, which is exactly what R2 says
                // out loud rather than papering over.
                ("/x/%E3%80%82%E3%80%82/_a24", PathVerdict::Forward),
            ];
            for (p, want) in cases {
                let full = format!("{NS}{p}");
                assert_eq!(judge(NS, &full), *want, "{full}");
            }
        }

        // ---- reduced fuzz (r3-review's `fuzz`, scaled down per task: the
        // full 28-token set kept, sequence length cut from 4 to 3, so this
        // runs in a few seconds rather than r3-review's own multi-minute
        // 659,373-path run) -------------------------------------------
        //
        // Cross-checks `judge`'s `Forward` verdicts against 160 different
        // plausible MODULE-side path normalizers (decode rounds ×
        // strip-`;params` × trim-trailing × collapse-empty × re-parse at
        // `?`/`#` × resolve-dot-segments-at-all, all on/off) — not just
        // against itself. Two claims are checked per `Forward`-verdicted raw
        // path, both halves of design §7.4's guarantee:
        //  1. under none of the 160 models does the result begin with
        //     `_a24` as its first segment (the guarantee's first half —
        //     `_a24` unreachable);
        //  2. under the `resolve_dots = false` models (a module that
        //     decodes for its own purposes but does not itself special-case
        //     `.`/`..`, e.g. an exact-match static-file router), the result
        //     never CONTAINS a literal `.` or `..` segment at all — the
        //     guarantee's other half, "任何含点段的路径到达不了模块", checked
        //     directly rather than only inferred from (1).
        fn dec_one(s: &str) -> String {
            let b = s.as_bytes();
            let mut o = Vec::new();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%'
                    && let (Some(h), Some(l)) = (
                        b.get(i + 1).and_then(|c| (*c as char).to_digit(16)),
                        b.get(i + 2).and_then(|c| (*c as char).to_digit(16)),
                    )
                {
                    o.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                o.push(b[i]);
                i += 1;
            }
            String::from_utf8_lossy(&o).into_owned()
        }

        /// One of the 160 hypothetical module-side canonicalisers this fuzz
        /// checks `judge`'s `Forward` verdicts against. `resolve_dots`:
        /// `true` models a router that pops `..` and drops `.`, same as
        /// `judge`'s peers usually do; `false` models one that decodes but
        /// never special-cases a `.`/`..` segment (an exact-match router,
        /// say) — those segments survive into the returned string literally,
        /// which is what lets the caller check for them directly.
        #[allow(clippy::too_many_arguments)]
        fn module_view(
            raw: &str,
            rounds: usize,
            strip_params: bool,
            trim: bool,
            collapse: bool,
            reparse: bool,
            resolve_dots: bool,
        ) -> String {
            let mut p = raw.to_string();
            for _ in 0..rounds {
                p = dec_one(&p);
            }
            if reparse {
                p = p.split(['?', '#']).next().unwrap_or("").to_string();
            }
            let mut st: Vec<String> = Vec::new();
            for seg in p.split('/') {
                let mut s = seg.to_string();
                if strip_params {
                    s = s.split(';').next().unwrap_or("").to_string();
                }
                if trim {
                    s = s.trim_end_matches(['.', ' ']).to_string();
                    if seg.chars().all(|c| c == '.') && !seg.is_empty() {
                        s = seg.to_string();
                    }
                }
                if resolve_dots && s == "." {
                    continue;
                }
                if resolve_dots && s == ".." {
                    st.pop();
                    continue;
                }
                if s.is_empty() && collapse {
                    continue;
                }
                st.push(s);
            }
            format!("/{}", st.join("/")).to_ascii_lowercase()
        }

        #[test]
        fn review_r3_fuzz_reduced() {
            let toks = [
                "_a24", "_A24", "%5fa24", "%255fa24", "x", "..", ".", "%2e", "%2e%2e", "%252e",
                "..;", "..;a", "..%20", "...", ";p", "", "%3b", "..%3F", "%3F", "%23", "_a24.",
                "_a24%20", "y", "..a", "%2F", "%20", ".%2e", "%2e.",
            ];
            let mut n = 0u64;
            let mut fwd = 0u64;
            let mut bad = Vec::new();
            fn rec(
                pre: &mut Vec<&'static str>,
                toks: &[&'static str],
                d: usize,
                f: &mut dyn FnMut(&[&'static str]),
            ) {
                f(pre);
                if d == 0 {
                    return;
                }
                for t in toks {
                    pre.push(t);
                    rec(pre, toks, d - 1, f);
                    pre.pop();
                }
            }
            let toks_s: Vec<&'static str> = toks.to_vec();
            let reserved_prefix = format!("{NS}/_a24/");
            let reserved_bare = format!("{NS}/_a24");
            let mut check = |segs: &[&'static str]| {
                let raw = format!("{NS}/{}", segs.join("/"));
                n += 1;
                if judge(NS, &raw) != PathVerdict::Forward {
                    return;
                }
                fwd += 1;
                if raw.starts_with(&reserved_prefix) || raw.eq_ignore_ascii_case(&reserved_bare) {
                    bad.push(format!("RAW {raw} reaches _a24 directly"));
                }
                for rounds in 0..=4 {
                    for sp in [false, true] {
                        for tr in [false, true] {
                            for co in [false, true] {
                                for rp in [false, true] {
                                    for rd in [false, true] {
                                        let mv = module_view(&raw, rounds, sp, tr, co, rp, rd);
                                        let tag =
                                            format!("r{rounds} sp{sp} tr{tr} co{co} rp{rp} rd{rd}");
                                        // Half 1 of §7.4: `_a24` unreachable.
                                        if mv.starts_with(&format!("{NS}/_a24/"))
                                            || mv == format!("{NS}/_a24")
                                        {
                                            bad.push(format!("{raw} {tag} -> {mv} (reaches _a24)"));
                                        }
                                        // Half 2 of §7.4: no dot segment
                                        // survives into the module's own
                                        // view either — checked only where
                                        // this model would not itself have
                                        // resolved one away (`rd == false`),
                                        // since a resolving model can never
                                        // emit one by construction.
                                        if !rd && mv.split('/').any(|s| s == "." || s == "..") {
                                            bad.push(format!(
                                                "{raw} {tag} -> {mv} (dot segment survives)"
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            };
            // Depth 3 (not r3-review's 4) for the plain sequence recursion,
            // PLUS r3-review's "two tokens concatenated into one segment"
            // form (`{a}{b}/c}`), which the recursion alone cannot produce.
            // Together: 44,717 raw paths, 5,185 of them `Forward` and so run
            // through the 160 module models (≈830k `module_view` calls
            // total) — a few seconds in a debug build (measured: ~4.3s).
            rec(&mut Vec::new(), &toks_s, 3, &mut check);
            for a in &toks_s {
                for b in &toks_s {
                    let concat: &'static str = Box::leak(format!("{a}{b}").into_boxed_str());
                    for c in &toks_s {
                        check(&[concat, c]);
                    }
                }
            }
            assert!(n > 40_000, "sanity: the fuzz should have run, n={n}");
            assert!(fwd > 0, "sanity: some paths should have forwarded");
            assert!(
                bad.is_empty(),
                "§7.4 violated under some module model: {:#?}",
                &bad[..bad.len().min(20)]
            );
        }

        // ── wired: the same claims, end-to-end through the real router ──

        #[tokio::test]
        async fn reserved_and_rejected_paths_never_reach_the_module() {
            let (proxy, hits) = proxied().await;
            for p in RESERVED {
                let full = format!("{NS}{p}");
                let got = call(proxy, Method::GET, &full, &[], "").await;
                assert_eq!(got.status, StatusCode::NOT_FOUND, "{full}");
                assert_eq!(got.json()["error"]["code"], "not_found", "{full}");
            }
            for p in REJECTED {
                let full = format!("{NS}{p}");
                let got = call(proxy, Method::GET, &full, &[], "").await;
                assert_eq!(got.status, StatusCode::BAD_REQUEST, "{full}");
                assert_eq!(
                    got.json()["error"]["code"],
                    "invalid_request_path",
                    "{full}"
                );
            }
            assert_eq!(
                hits.0.load(Ordering::SeqCst),
                0,
                "the mock upstream must see zero requests for a reserved or rejected path"
            );
        }

        #[tokio::test]
        async fn positive_controls_still_forward_the_untouched_raw_path() {
            let (proxy, hits) = proxied().await;
            for (i, p) in FORWARD.iter().enumerate() {
                let full = format!("{NS}{p}");
                let got = call(proxy, Method::GET, &full, &[], "").await;
                assert_eq!(got.status, StatusCode::OK, "{full}");
                assert_eq!(
                    got.json()["path"],
                    full,
                    "the module must see the ORIGINAL raw path, not a canonicalised one"
                );
                assert_eq!(
                    hits.0.load(Ordering::SeqCst),
                    i + 1,
                    "each positive control must reach the module exactly once: {full}"
                );
            }
        }

        /// C3.1's query-string variant: `judge` only ever sees
        /// `OriginalUri::path()`, never the query — so a query string
        /// cannot turn a reserved path into a forwarded one, and does not
        /// need to be stripped anywhere else for this judgement to hold.
        #[tokio::test]
        async fn query_string_does_not_affect_the_judgement() {
            let (proxy, hits) = proxied().await;
            let got = call(
                proxy,
                Method::GET,
                &format!("{NS}/_a24/scheduler/fired?x=1"),
                &[],
                "",
            )
            .await;
            assert_eq!(got.status, StatusCode::NOT_FOUND);
            assert_eq!(got.json()["error"]["code"], "not_found");
            assert_eq!(hits.0.load(Ordering::SeqCst), 0);
        }

        /// L3: the judgement does not depend on the HTTP method — a HEAD or
        /// a POST to a reserved path must be refused exactly like a GET.
        /// `judge` never looks at the method at all, but that is exactly
        /// the kind of invariant that is easy to break by accident (e.g. by
        /// moving the judgement into a GET-only branch during a refactor),
        /// so it is worth pinning through the real router rather than only
        /// trusting the pure function's signature.
        #[tokio::test]
        async fn reserved_paths_are_judged_regardless_of_method() {
            let (proxy, hits) = proxied().await;

            let head = call(
                proxy,
                Method::HEAD,
                &format!("{NS}/_a24/scheduler/fired"),
                &[],
                "",
            )
            .await;
            assert_eq!(head.status, StatusCode::NOT_FOUND);

            let post = call(
                proxy,
                Method::POST,
                &format!("{NS}/_a24/scheduler/fired"),
                &[],
                "{}",
            )
            .await;
            assert_eq!(post.status, StatusCode::NOT_FOUND);
            assert_eq!(post.json()["error"]["code"], "not_found");

            assert_eq!(
                hits.0.load(Ordering::SeqCst),
                0,
                "neither HEAD nor POST to a reserved path may reach the module"
            );
        }

        /// L2: the judgement precedes EVERY side effect of admission, not
        /// only "no request reaches the module". Checked two ways at once:
        /// while a genuine request is held open, a reserved/rejected call
        /// sneaking into `Generation::in_flight` even briefly would show up
        /// as a count of 2; and the request-id counter (`RequestIds`, a
        /// monotonic `AtomicU64` — `proxy.rs`) must advance by exactly one
        /// across the whole batch of reserved/rejected calls fired between
        /// two legitimate ones, proving none of them minted an id.
        #[tokio::test]
        async fn rejection_happens_before_any_admission_or_id_mint() {
            let module = gated(Then::Answer).await;
            let generation = running_generation(module.addr.clone());
            let proxy = serve(mount(Router::new(), NS, Current::new(generation.clone()))).await;
            assert_eq!(generation.in_flight(), 0);

            let held = tokio::spawn(async move {
                call(proxy, Method::GET, &format!("{NS}/held"), &[], "").await
            });
            module.wait_arrived().await;
            assert_eq!(generation.in_flight(), 1);

            for p in RESERVED.iter().chain(REJECTED.iter()) {
                let full = format!("{NS}{p}");
                let got = call(proxy, Method::GET, &full, &[], "").await;
                assert_ne!(got.status, StatusCode::OK, "{full}");
                assert_eq!(
                    generation.in_flight(),
                    1,
                    "a reserved/rejected request must never be admitted \
                     while another is genuinely in flight: {full}"
                );
            }
            assert_eq!(
                module.dials.load(Ordering::SeqCst),
                1,
                "only the held request should ever have reached the module"
            );

            module.release.notify_waiters();
            let held = held.await.unwrap();
            assert_eq!(held.status, StatusCode::OK);
            assert_eq!(generation.in_flight(), 0);

            let next = tokio::spawn(async move {
                call(proxy, Method::GET, &format!("{NS}/next"), &[], "").await
            });
            module.wait_arrived().await;
            module.release.notify_waiters();
            let next = next.await.unwrap();
            assert_eq!(next.status, StatusCode::OK);

            // The ids the MODULE saw, in arrival order — exactly two, back
            // to back, with none of the reserved/rejected calls above ever
            // having minted one in between.
            let ids = module.ids.lock().unwrap().clone();
            assert_eq!(ids.len(), 2, "{ids:?}");
            let n = |id: &str| -> u64 {
                id.rsplit('-')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or_else(|| panic!("not a `<prefix>-<n>` id: {id:?}"))
            };
            assert_eq!(
                n(&ids[1]),
                n(&ids[0]) + 1,
                "a reserved/rejected request must never mint a request id: {ids:?}"
            );
        }
    }
}
