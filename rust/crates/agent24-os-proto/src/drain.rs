//! ME-3b-5 — stopping a module in two phases (SPEC-ME3-OUT-OF-PROCESS §4).
//!
//! One [`Generation`] is one run of one module process: minted when the process
//! is spawned, and revoked exactly once before that process is killed. A restart
//! is a new generation, so nothing a dead run was allowed to do carries over.
//!
//! ```text
//! Starting ──ready──▶ Running ──begin_drain──▶ Draining ──revoke──▶ Revoked
//!     │                  │                                    ▲
//!     └──────────────────┴──────────────── revoke ────────────┘
//! ```
//!
//! # What each state admits
//!
//! | | new proxied request | callback with a live `request_id` | callback without one |
//! |---|---|---|---|
//! | `Starting` | 503 `module_not_ready` | refused | refused |
//! | `Running`  | admitted | admitted | admitted |
//! | `Draining` | 503 `module_draining` | **admitted** | **refused** |
//! | `Revoked`  | 503 `module_stopping` | refused | refused |
//!
//! The `Draining` row is the reason this module exists. SPEC §4 settled two
//! drafts that contradicted each other: requests already in flight keep running
//! AND keep their callbacks — otherwise a handler cannot fetch the memory or the
//! approval it needs to finish, and "a grace period" means "a period in which
//! everything fails" — while background work (callbacks that carry no request)
//! is cut off at once.
//!
//! **This guarantee is narrower than it may sound, and says so.** A `request_id`
//! is not a secret: the module sees every id it is currently serving, so a
//! background task CAN attach a live one and be admitted while draining. What
//! `Draining` refuses is a callback that carries no live request — not "any
//! callback that is not really on behalf of that request". Telling those apart
//! needs a per-handler channel, which SPEC §4 and §6 leave to a later decision.
//!
//! # Revocation before the kill — by type, not by call order
//!
//! SPEC §4: *"撤 generation 必须早于杀进程（否则宽限期里它还能写）"*. That is a
//! safety property, and a property that holds because every caller remembered
//! the order holds until the first caller who did not. So killing needs a
//! [`KillPermit`]; the only way to obtain one is [`Generation::revoke`]; and
//! **both `revoke` and the kill live inside this crate**, behind the one type
//! that holds a module process — [`crate::supervise::ModuleProcess`]. From
//! outside, the only way to kill a module is `ModuleProcess::stop`, which
//! revokes that process's own generation first (FU-46).
//!
//! ```compile_fail
//! // There is no way to build a permit by hand ...
//! let permit = agent24_os_proto::drain::KillPermit { _private: () };
//! ```
//!
//! ```compile_fail
//! // ... or to copy one ...
//! fn f(p: agent24_os_proto::drain::KillPermit) -> (agent24_os_proto::drain::KillPermit, agent24_os_proto::drain::KillPermit) {
//!     (p.clone(), p)
//! }
//! ```
//!
//! ```compile_fail
//! // ... or to default one into existence ...
//! let permit = agent24_os_proto::drain::KillPermit::default();
//! ```
//!
//! ```compile_fail
//! // ... or to revoke a generation from outside the crate and take its permit ...
//! let revocation = agent24_os_proto::drain::Generation::starting().revoke();
//! ```
//!
//! ```compile_fail
//! // ... or to reach the child around `stop`.
//! # async fn f(p: agent24_os_proto::supervise::ModuleProcess) {
//! // (A borrow, not a move: moving out of a type with `Drop` is refused even
//! // for a public field, which would keep this block red for the wrong reason.)
//! let child = &p.child;
//! # }
//! ```
//!
//! The control — the one legal way, which must compile:
//!
//! ```no_run
//! # async fn f(p: agent24_os_proto::supervise::ModuleProcess) {
//! let report = p.stop(std::time::Duration::from_secs(1)).await;
//! # }
//! ```
//!
//! **Why the control is there.** Stable rustdoc does not check the error code of
//! a `compile_fail` block — measured: `compile_fail,E0999` passes. So each of
//! the blocks above passes for ANY compile error, a typo included, and only
//! the control shows that the same shape compiles when the path is the legal
//! one. Each is mutation-checked: making `_private` public, deriving `Clone`,
//! adding `impl Default`, making `revoke` public, and making the process's
//! field public each turn exactly its block red.
//!
//! **What this still does not guarantee.** Inside this crate, `revoke` and the
//! kill are ordinary functions; the ordering there rests on review of one small
//! type, not on the compiler. And the guarantee is about the process
//! `ModuleProcess` started — a module that hands work to a process outside its
//! own group (a daemon of its own, say) is outside every kill path by design.
//!
//! Every kill path goes through `ModuleProcess` — a disable, a crash (helpers
//! outlive a dead leader and may still hold the callback connection), a startup
//! timeout (nothing was ever admitted, so revoking costs nothing), and dropping
//! it without stopping it (which revokes, then kills the group without grace).
//! None may be exempt, because an exemption is a constructor.
//!
//! # No clock, no waiting
//!
//! Like [`crate::supervise::RestartPolicy`], this holds no clock: the drain
//! deadline is computed from the `now` the caller passes. How the caller waits
//! for "in flight reached zero or the grace ran out" is its business; this
//! answers "which of those is true at `now`".

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// Where one generation is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainState {
    /// Spawned; `initialize` has not completed.
    Starting,
    /// Ready: admits requests and callbacks.
    Running,
    /// Refuses new requests; lets in-flight ones finish, with their callbacks.
    Draining,
    /// Nothing is admitted any more. The process may now be killed.
    Revoked,
}

/// Why a new proxied request was not admitted. Each maps to its own 503 `code`
/// (SPEC §2.1: 503 means "this namespace cannot take this request right now",
/// and the operator-facing reason lives in `code`, not in the status).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestRefused {
    NotReady,
    Draining,
    Stopping,
    /// The kernel minted an id that is already in flight. The kernel mints
    /// these ids itself, so this is a kernel bug — refused rather than allowed
    /// to alias, because two requests sharing one id would make the `Draining`
    /// check answer for the wrong one.
    DuplicateId,
}

impl RequestRefused {
    /// The `error.code` a 503 carries.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::NotReady => "module_not_ready",
            Self::Draining => "module_draining",
            Self::Stopping => "module_stopping",
            Self::DuplicateId => "duplicate_request_id",
        }
    }
}

/// Why a callback was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackRefused {
    /// Before `initialize` completed nothing is authorised.
    NotReady,
    /// Draining, and the callback carries no request id — background work.
    DrainingWithoutRequest,
    /// Draining, and the id it carries is not in flight (never was, or has
    /// already finished).
    DrainingUnknownRequest,
    /// The generation is revoked: nothing is admitted, in-flight or not.
    Revoked,
}

/// Why an approval-submitting callback (`_a24/approval/gate`/`advise`) was
/// refused (T7b/ME-3e design doc, decision 2). A DELIBERATELY coarser set than
/// [`CallbackRefused`]: rows 3 and 4 of the decision's error matrix — an id
/// that was never in flight, one that already finished, one from an old
/// generation, and one whose token is wrong or already used — all fold into
/// the same [`Self::TokenInvalid`], so an unauthorized caller cannot tell "bad
/// id" from "bad token" apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalCallbackRefused {
    /// Before `initialize` completed nothing is authorised.
    NotReady,
    /// The generation is revoked: nothing is admitted.
    Revoked,
    /// `request_id` is not in flight, or the token does not match / was
    /// already used. Deliberately one outcome for all of those (see above).
    TokenInvalid,
}

/// The one value that allows killing a module's process group.
///
/// Not `Clone`, not `Default`, no public constructor: [`Generation::revoke`] is
/// the only source, and only [`crate::supervise::ModuleProcess`] calls it for a
/// live process — on its own generation. See the module docs.
#[derive(Debug)]
pub struct KillPermit {
    _private: (),
}

/// What [`Generation::revoke`] hands back.
#[derive(Debug)]
pub struct Revocation {
    /// Allows the kill in [`crate::supervise::ModuleProcess::stop`].
    pub permit: KillPermit,
    /// Requests in flight AND already sent to the module at the moment of
    /// revocation. Their outcome is **unknown** — the module may or may not have
    /// acted on them — and the caller must say so (a 503, and a log line),
    /// never report success.
    pub abandoned: Vec<String>,
    /// Requests in flight but never sent (still reading the client's body, say).
    /// Their outcome is known: nothing ran, and after this point nothing will.
    pub never_sent: Vec<String>,
}

/// Where a drain stands at a given `now`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainProgress {
    /// Not draining (never started, or already revoked).
    NotDraining,
    /// Nothing in flight: revoke now.
    Idle,
    /// Still waiting for `in_flight` requests, at most `remaining` longer.
    Waiting {
        in_flight: usize,
        remaining: Duration,
    },
    /// The grace ran out with requests still in flight: revoke now, and those
    /// requests are abandoned.
    Expired { in_flight: usize },
}

/// One in-flight request's approval-callback token (T7b/ME-3e design doc,
/// decision 2): the SHA-256 of the plaintext `X-A24-Approval-Token` minted
/// alongside this request's id, never the plaintext itself, and whether it
/// has already been spent. Minted and registered in the SAME
/// [`Generation::admit_request`] call as the id — never a second, later
/// write — so there is no window where the id is admitted but has no token
/// yet.
#[derive(Debug, Clone, Copy)]
struct ApprovalToken {
    hash: [u8; 32],
    used: bool,
}

/// One in-flight request's approval token AND its lifecycle signal (T8.5a
/// design doc, decision 1) — `token`/`deadline`/`ended` are inserted together
/// by the SAME [`Generation::admit_request`] call and removed together by
/// `finish`/`Drop`. No tombstone, no second table: once an entry leaves
/// `in_flight` it is gone for good, exactly the discipline `ApprovalToken`
/// alone already had.
#[derive(Debug)]
struct InFlightEntry {
    /// Original field, unchanged.
    token: ApprovalToken,
    /// This request's own absolute deadline (decision 5): `now.checked_add(budget)`
    /// at `admit_request` time.
    deadline: Instant,
    /// Whether this request has ended — a queryable current value, not an
    /// event stream (decision 2). `watch::Sender` + `send_replace`, the same
    /// primitive [`Generation::revoked`] already uses: a `subscribe()` that
    /// happens after the `send_replace` still sees the current value, so a
    /// late query cannot miss it.
    ended: watch::Sender<bool>,
}

#[derive(Debug)]
struct Inner {
    state: DrainState,
    /// Keyed by request id; each entry also carries that request's approval
    /// token and lifecycle signal (T7b/T8.5a). A plain `HashSet<String>` was
    /// enough before either existed — the value only needs to say "is this id
    /// live", now it also has to say "and if so, with which secret, until
    /// when, and has it ended".
    in_flight: HashMap<String, InFlightEntry>,
    /// The subset of `in_flight` that has been sent to the module. Marked under
    /// the same lock `revoke` takes, so "sent" and "revoked" are ordered: a
    /// request is either sent before the revocation (its outcome is then
    /// unknown) or never sent at all (known: nothing ran).
    dispatched: HashSet<String>,
    drain_deadline: Option<Instant>,
}

/// One run of one module process. Shared (`Arc`) between the proxy, the callback
/// channel and whoever stops the module.
#[derive(Debug)]
pub struct Generation {
    inner: Mutex<Inner>,
    /// Flips to `true` once, on revocation, so a request in flight can stop
    /// waiting for its module the moment its generation is revoked rather than
    /// when the process finally dies (see [`InFlight::revoked`]).
    revoked: tokio::sync::watch::Sender<bool>,
    /// Where this run's process listens for proxied requests: its own Unix
    /// domain socket path (D4, FU-60), so a request admitted into this
    /// generation reaches this process and no other. `None` for a
    /// placeholder, which never becomes `Running`.
    upstream: Option<std::path::PathBuf>,
}

/// A request admitted into a generation. Leaves the in-flight set when dropped,
/// so a handler that returns early, panics or is cancelled cannot leave an id
/// behind that would keep a drain waiting for the whole grace period.
#[derive(Debug)]
pub struct InFlight {
    generation: Arc<Generation>,
    id: String,
    finished: bool,
}

impl Generation {
    /// A placeholder: a slot's generation while no process is serving it —
    /// before the first run, between runs. It refuses everything
    /// (`503 module_not_ready`) and can never become `Running`: it has no
    /// process to send a request to.
    #[must_use]
    pub fn starting() -> Arc<Self> {
        Self::new(None)
    }

    /// A freshly spawned module listening at `upstream`, before `initialize`.
    /// Requests admitted into it are sent there and nowhere else (SUP-3b).
    #[must_use]
    pub fn serving_at(upstream: std::path::PathBuf) -> Arc<Self> {
        Self::new(Some(upstream))
    }

    fn new(upstream: Option<std::path::PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                state: DrainState::Starting,
                in_flight: HashMap::new(),
                dispatched: HashSet::new(),
                drain_deadline: None,
            }),
            revoked: tokio::sync::watch::Sender::new(false),
            upstream,
        })
    }

    /// Where this run's process listens; `None` for a placeholder. Every
    /// `Running` generation has one (see [`Generation::ready`]). Borrowed, not
    /// `Copy` like the `SocketAddr` this replaced (FU-60) — a caller that
    /// needs to own it clones explicitly.
    #[must_use]
    pub fn upstream(&self) -> Option<&std::path::Path> {
        self.upstream.as_deref()
    }

    // A poisoned lock means another thread panicked while holding it. The state
    // it guards is a set and an enum — every write is a single assignment or a
    // single insert/remove — so there is no half-written state to protect
    // against, and refusing to proceed would turn one panic into a module that
    // can never be stopped.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn state(&self) -> DrainState {
        self.lock().state
    }

    /// How many admitted requests have not finished. For logs, and for a test to
    /// establish "it is in flight" as a fact rather than after a sleep.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.lock().in_flight.len()
    }

    /// `initialize` succeeded. Only from `Starting`, and only for a generation
    /// with a process behind it; returns `false` otherwise (a generation that
    /// was revoked while starting stays revoked, and a placeholder stays a
    /// placeholder). So a `Running` generation always has an
    /// [`upstream`](Generation::upstream).
    #[must_use]
    pub fn ready(&self) -> bool {
        if self.upstream.is_none() {
            return false;
        }
        let mut inner = self.lock();
        if inner.state == DrainState::Starting {
            inner.state = DrainState::Running;
            true
        } else {
            false
        }
    }

    /// Admit a new proxied request carrying the kernel-minted `id`, minting
    /// and registering its approval-callback token in the SAME step
    /// (T7b/ME-3e design doc, decision 2 — "铸造与登记必须在同一次
    /// `admit_request` 调用里完成"): there is deliberately no separate,
    /// later call that attaches a token to an id already admitted, which
    /// would leave a window where the id exists with no token to check.
    /// `token_hash` is the SHA-256 of the plaintext token minted alongside
    /// `id` — never the plaintext itself.
    ///
    /// `now`/`budget` (T8.5a design doc, decision 5) fix this request's own
    /// absolute deadline at admission time — this module holds no clock of
    /// its own (see the module doc, "No clock, no waiting"): `now` is always
    /// the caller's, and `budget` is the caller's own ceiling for this call
    /// (`proxy.rs` passes `state.limits.total`, a test passes any fixed
    /// value). `now.checked_add(budget)` overflowing is refused toward the
    /// SAFER extreme for a hot admission path, unlike [`Self::begin_drain`]'s
    /// refusal of the whole operation: the deadline becomes `now` itself
    /// (`remaining` is then always `Duration::ZERO`), and the request is
    /// still admitted rather than rejected over a case that, in production,
    /// only an exhausted monotonic clock could ever trigger.
    ///
    /// # Errors
    ///
    /// Anything but `Running` refuses, each state with its own reason.
    pub fn admit_request(
        self: &Arc<Self>,
        id: String,
        token_hash: [u8; 32],
        now: Instant,
        budget: Duration,
    ) -> Result<InFlight, RequestRefused> {
        let mut inner = self.lock();
        match inner.state {
            DrainState::Starting => Err(RequestRefused::NotReady),
            DrainState::Draining => Err(RequestRefused::Draining),
            DrainState::Revoked => Err(RequestRefused::Stopping),
            DrainState::Running => {
                if inner.in_flight.contains_key(&id) {
                    return Err(RequestRefused::DuplicateId);
                }
                let deadline = now.checked_add(budget).unwrap_or(now);
                inner.in_flight.insert(
                    id.clone(),
                    InFlightEntry {
                        token: ApprovalToken {
                            hash: token_hash,
                            used: false,
                        },
                        deadline,
                        ended: watch::Sender::new(false),
                    },
                );
                Ok(InFlight {
                    generation: Arc::clone(self),
                    id,
                    finished: false,
                })
            }
        }
    }

    /// May a callback carrying `request_id` (or none) proceed?
    ///
    /// # Errors
    ///
    /// See the table in the module docs.
    pub fn admit_callback(&self, request_id: Option<&str>) -> Result<(), CallbackRefused> {
        let inner = self.lock();
        match inner.state {
            DrainState::Starting => Err(CallbackRefused::NotReady),
            DrainState::Running => Ok(()),
            DrainState::Revoked => Err(CallbackRefused::Revoked),
            DrainState::Draining => match request_id {
                None => Err(CallbackRefused::DrainingWithoutRequest),
                Some(id) if inner.in_flight.contains_key(id) => Ok(()),
                Some(_) => Err(CallbackRefused::DrainingUnknownRequest),
            },
        }
    }

    /// May a `_a24/approval/gate`/`advise` SUBMIT proceed, and — if so — mark
    /// its token used? (T7b/ME-3e design doc, decision 2.) Verification and
    /// consumption are ONE atomic step under one lock: there is no window
    /// between "checked valid" and "marked used" in which a second,
    /// concurrent submit for the same `request_id` could also pass.
    ///
    /// Unlike [`Self::admit_callback`], `Running` and `Draining` are treated
    /// identically here — an approval submission always carries a
    /// `request_id`, so there is no "background work, no id" case to tell
    /// apart, and a still-in-flight request's approval submission is exactly
    /// the case `Draining` must keep admitting (SPEC §4).
    ///
    /// # Errors
    ///
    /// See [`ApprovalCallbackRefused`] and decision 2's error matrix: an id
    /// that was never in flight, one that already finished, one from an old
    /// generation, a wrong token, and an already-used token all collapse into
    /// [`ApprovalCallbackRefused::TokenInvalid`] — deliberately, so an
    /// unauthorized caller cannot tell those apart (judgement 3-6).
    pub fn admit_approval_callback(
        &self,
        request_id: &str,
        token: &str,
    ) -> Result<(), ApprovalCallbackRefused> {
        let mut inner = self.lock();
        match inner.state {
            DrainState::Starting => return Err(ApprovalCallbackRefused::NotReady),
            DrainState::Revoked => return Err(ApprovalCallbackRefused::Revoked),
            DrainState::Running | DrainState::Draining => {}
        }
        let Some(entry) = inner.in_flight.get_mut(request_id) else {
            return Err(ApprovalCallbackRefused::TokenInvalid);
        };
        let presented = sha256(token.as_bytes());
        if entry.token.used || !constant_time_eq(&entry.token.hash, &presented) {
            return Err(ApprovalCallbackRefused::TokenInvalid);
        }
        entry.token.used = true;
        Ok(())
    }

    /// This request's lifecycle signal, if `id` is currently in flight in
    /// this generation — its absolute deadline and a queryable/awaitable
    /// "has it ended" flag (T8.5a design doc, decisions 1/2).
    ///
    /// `None` for BOTH "this id was never admitted into this generation" AND
    /// "it was, but has already `finish`ed or been dropped" — decision 2's
    /// deliberate range boundary: once an entry leaves `in_flight` there is no
    /// remaining time left to report, and a caller in either case falls back
    /// to the same default budget (decision 3). Only a caller that already
    /// holds a `RequestLifecycle` from BEFORE the request ended keeps
    /// learning about it — that `watch::Receiver` is independent of this
    /// map entry once handed out.
    #[must_use]
    pub fn request_lifecycle(&self, id: &str) -> Option<RequestLifecycle> {
        let inner = self.lock();
        inner.in_flight.get(id).map(|entry| RequestLifecycle {
            deadline: entry.deadline,
            ended: entry.ended.subscribe(),
        })
    }

    /// Stop admitting new requests; let in-flight ones run for at most `grace`.
    ///
    /// Only from `Running` — returns `false` otherwise. A module that never
    /// became ready has nothing to drain and is revoked directly; one already
    /// draining keeps its first deadline (a second `disable` does not extend
    /// the grace).
    #[must_use]
    pub fn begin_drain(&self, now: Instant, grace: Duration) -> bool {
        // Computed BEFORE touching the state: `now + grace` can overflow and
        // panic, and a panic after `state = Draining` would leave a draining
        // generation with no deadline behind a poisoned (and here, recovered)
        // lock. An unrepresentable grace is refused instead.
        let Some(deadline) = now.checked_add(grace) else {
            return false;
        };
        let mut inner = self.lock();
        if inner.state == DrainState::Running {
            inner.state = DrainState::Draining;
            inner.drain_deadline = Some(deadline);
            true
        } else {
            false
        }
    }

    /// Where the drain stands at `now`.
    #[must_use]
    pub fn progress(&self, now: Instant) -> DrainProgress {
        let inner = self.lock();
        let (DrainState::Draining, Some(deadline)) = (inner.state, inner.drain_deadline) else {
            return DrainProgress::NotDraining;
        };
        let in_flight = inner.in_flight.len();
        if in_flight == 0 {
            DrainProgress::Idle
        } else if now >= deadline {
            DrainProgress::Expired { in_flight }
        } else {
            DrainProgress::Waiting {
                in_flight,
                remaining: deadline - now,
            }
        }
    }

    /// Revoke this generation: from here on nothing is admitted — no request, no
    /// callback, in flight or not, and no in-flight request may still be sent
    /// to the module. Legal from every state, because every kill path needs it
    /// (see the module docs).
    ///
    /// **One-shot**: the first call returns the [`Revocation`] (and with it the
    /// only [`KillPermit`] this generation will ever yield); every later call
    /// returns `None`. Two paths that race to stop the same run — a crash and a
    /// disable — therefore cannot both kill.
    ///
    /// The in-flight set is **kept**: an [`InFlight`] finishing after this
    /// point learns that it was abandoned from [`InFlight::finish`].
    ///
    /// Crate-private: outside this crate a generation is revoked only by
    /// stopping the [`crate::supervise::ModuleProcess`] it belongs to, which is
    /// what binds the permit to that process (FU-46).
    #[must_use]
    pub(crate) fn revoke(&self) -> Option<Revocation> {
        let (mut abandoned, mut never_sent) = {
            let mut inner = self.lock();
            if inner.state == DrainState::Revoked {
                return None;
            }
            inner.state = DrainState::Revoked;
            let (sent, unsent): (Vec<String>, Vec<String>) = inner
                .in_flight
                .keys()
                .cloned()
                .partition(|id| inner.dispatched.contains(id));
            (sent, unsent)
        };
        abandoned.sort();
        never_sent.sort();
        // After the state is Revoked, so a woken request that re-checks the
        // state sees the revocation.
        self.revoked.send_replace(true);
        Some(Revocation {
            permit: KillPermit { _private: () },
            abandoned,
            never_sent,
        })
    }
}

impl Generation {
    /// Resolves once this generation is revoked — immediately if it already
    /// has been. For whoever serves this generation's callback connection: it
    /// stops when the generation does (FU-49 — the connection is bound to the
    /// generation that completed its handshake). `watch`, so a revocation that
    /// lands before anyone waits is not a lost wakeup.
    pub async fn revoked(&self) {
        let mut rx = self.revoked.subscribe();
        // The sender lives in `self`; an error here cannot happen while `self`
        // is borrowed, and is read as "never revoked" rather than as revoked.
        let _ = rx.wait_for(|revoked| *revoked).await;
    }
}

/// A request's lifecycle signal, as of the moment
/// [`Generation::request_lifecycle`] returned it: its absolute deadline, and
/// an awaitable/queryable "has this request ended" flag (T8.5a design doc,
/// decision 2). Holds no clock of its own (decision 5) — [`Self::remaining`]
/// takes `now` from its caller, the same discipline [`Generation::begin_drain`]
/// and [`Generation::progress`] already follow. `Clone`: `watch::Receiver` is
/// `Clone` unconditionally (it shares the same underlying state), so cloning
/// this just hands out another handle to the same request's signal — useful
/// for a caller that needs to observe the same request from more than one
/// place, and for tests that re-run `bind_to_lifecycle` against one fixed
/// lifecycle many times.
#[derive(Debug, Clone)]
pub struct RequestLifecycle {
    deadline: Instant,
    ended: watch::Receiver<bool>,
}

impl RequestLifecycle {
    /// How much of this request's budget is left, as of `now`. Saturates at
    /// zero rather than going negative once `now` reaches or passes the
    /// deadline.
    #[must_use]
    pub fn remaining(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    /// Resolves once this request has ended — immediately if it already had
    /// by the time this was called. Backed by `watch::Sender::send_replace`
    /// (decision 2): a `subscribe()` that happens after the request already
    /// ended still observes the current value, so a late waiter cannot miss
    /// it the way a plain `send` would let it.
    pub async fn ended(&mut self) {
        let _ = self.ended.wait_for(|ended| *ended).await;
    }
}

/// Why [`bind_to_lifecycle`] gave up on `work` instead of returning its
/// result — two different facts, kept distinct rather than folded into one
/// "the request has already ended" message (T8.5a design doc's frozen-design
/// note M-new2, which flagged that folding as inaccurate on the first
/// branch):
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleTimeout {
    /// This request's own time budget (`remaining` at the time
    /// [`bind_to_lifecycle`] started) ran out before `work` completed. The
    /// bound request may or may not still be in flight — this says nothing
    /// about whether it has ended.
    BudgetExhausted,
    /// The bound request ended (`finish`/`Drop`, decision 4) before `work`
    /// completed.
    RequestEnded,
}

/// Bind a business future to a (possibly absent) request lifecycle (T8.5a
/// design doc, decision 3): with a lifecycle, race `work` against
/// `min(remaining, natural completion)` AND against the bound request ending,
/// whichever comes first; with none, `await` `work` unchanged — the caller's
/// own outer deadline (e.g. `rpc.rs`'s `Limits::call_timeout`) is the only
/// thing bounding it, exactly as today.
///
/// Independent of any specific business handler by design — testable with a
/// stub future a test controls directly (judgement 9a), rather than only
/// through a real operation that happens to be asynchronous. Not preemption:
/// `work` (or whatever it drives, e.g. `sink.emit`) may have already
/// committed a side effect by the time this returns `Err` — dropping the
/// future does not undo it (decision 4), the same disclaimer `$/cancelRequest`
/// already carries. And it can only take effect at `work`'s own next
/// `.await` point: a `work` that does not yield does not get interrupted
/// mid-poll.
///
/// # Errors
///
/// [`LifecycleTimeout`] — see its variants for which of the two happened.
pub async fn bind_to_lifecycle<T>(
    lifecycle: Option<RequestLifecycle>,
    work: impl Future<Output = T>,
) -> Result<T, LifecycleTimeout> {
    match lifecycle {
        Some(mut lifecycle) => {
            let remaining = lifecycle.remaining(Instant::now());
            // `biased`, work arm first (Codex review round 1 Medium 1): a
            // `work` that is already resolved by the time this is first
            // polled (`std::future::ready(sink.emit(...))` in
            // `events_emit.rs` always is — `sink.emit` runs eagerly, before
            // `bind_to_lifecycle` is ever called) and a lifecycle that has
            // ALSO already ended by then are both immediately ready on the
            // first poll. Plain `tokio::select!` picks a ready branch at
            // random, which could report `RequestEnded` for a call whose
            // effect had already landed — a caller seeing that error has no
            // way to know the side effect happened and may retry, duplicating
            // it. Polling the work arm first makes an already-produced result
            // win deterministically; this cannot starve `ended()`, because
            // there is exactly one `select!`, not a loop — a pending `work`
            // still lets `ended()` be polled on every subsequent wakeup.
            tokio::select! {
                biased;
                r = tokio::time::timeout(remaining, work) => {
                    r.map_err(|_| LifecycleTimeout::BudgetExhausted)
                }
                () = lifecycle.ended() => Err(LifecycleTimeout::RequestEnded),
            }
        }
        // No binding (no `request_id`, or one `request_lifecycle` could not
        // find — decision 2's range boundary): unchanged behaviour, bounded
        // only by whatever the caller's own outer deadline already is.
        None => Ok(work.await),
    }
}

/// Returned by [`InFlight::finish`] when the generation was revoked while the
/// request was in flight: whatever the module answered, the kernel does not
/// pass it on as a success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abandoned {
    /// Whether it had been sent to the module. `true`: its outcome is unknown.
    /// `false`: nothing ran.
    pub dispatched: bool,
}

impl Abandoned {
    /// The `error.code` of the 503 an abandoned request gets.
    pub const CODE: &'static str = "request_abandoned";
}

/// The generation a namespace is served by right now.
///
/// The proxy is mounted once, when the router is built, but a module that
/// crashes and restarts is a NEW generation. So the proxy holds this slot rather
/// than a generation, and reads it once per request. A request keeps the
/// generation it was admitted into (its [`InFlight`] holds it), so replacing the
/// slot never moves a request in flight to the next run — revoking the old run
/// still abandons it.
#[derive(Debug)]
pub struct Current {
    slot: Mutex<Arc<Generation>>,
    /// Whether a supervisor holds this slot (see `claim`).
    held: std::sync::atomic::AtomicBool,
    /// The status feed of whichever supervisor currently (or most recently)
    /// claimed this slot (ERR-1/D) — attached from inside `supervise()`
    /// itself, not by a caller, so it can never be forgotten (design doc,
    /// Codex round 1 High 7). `None` until a supervisor has ever attached one.
    status: Mutex<Option<watch::Receiver<crate::supervisor::Status>>>,
}

impl Current {
    #[must_use]
    pub fn new(generation: Arc<Generation>) -> Arc<Self> {
        Arc::new(Self {
            slot: Mutex::new(generation),
            held: std::sync::atomic::AtomicBool::new(false),
            status: Mutex::new(None),
        })
    }

    /// Attach the status feed for the supervisor that just claimed this slot.
    /// Called once from inside `supervisor::supervise()`, right after it
    /// builds its own `watch::channel` — see the module doc on why this is
    /// not a step callers opt into.
    pub(crate) fn attach_status(&self, status: watch::Receiver<crate::supervisor::Status>) {
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status);
    }

    /// The most recently observed `Status` of whichever supervisor has ever
    /// claimed this slot. `None` only for a `Current` no supervisor has ever
    /// been attached to (e.g. a bare test fixture built with `new` alone).
    #[must_use]
    pub fn status(&self) -> Option<crate::supervisor::Status> {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|rx| rx.borrow().clone())
    }

    /// Become the one supervisor of this slot and install `placeholder`, or
    /// `false` if a supervisor holds it already. One at a time, by
    /// construction: a second supervisor started while the first winds down
    /// would run a second process on the same data directory — for as long as
    /// the first failed to die (review of ME3-SUP slice 3a, rounds 2–5, which
    /// tried a takeover protocol instead and kept finding its gaps).
    pub(crate) fn claim(&self, placeholder: Arc<Generation>) -> bool {
        if self
            .held
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return false;
        }
        // Out goes the caller's initial generation, or the last one of the
        // previous supervisor, which stopped (and revoked it) before releasing.
        let _previous = self.replace(placeholder);
        true
    }

    /// Give the slot up: the supervisor holds no process that is not
    /// confirmed gone (see `supervisor::Exit`).
    pub(crate) fn release(&self) {
        self.held.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    #[must_use]
    pub fn get(&self) -> Arc<Generation> {
        Arc::clone(
            &self
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Install the next run's generation and hand back the one it replaced.
    ///
    /// Does NOT revoke the old one: whoever stops a run revokes it, through the
    /// one path that yields a [`KillPermit`]. Revoking here as well would be a
    /// second place that decides when a module may be killed.
    ///
    /// Crate-private: the slot's supervisor (see `claim`) is the only writer,
    /// and a public `replace` let anyone put a generation there behind its
    /// back (review of ME3-SUP slice 3a, round 6).
    #[must_use]
    pub(crate) fn replace(&self, next: Arc<Generation>) -> Arc<Generation> {
        std::mem::replace(
            &mut *self
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            next,
        )
    }
}

impl InFlight {
    /// Where this request goes: the address of the generation it was admitted
    /// into — not of whatever the slot holds by the time it is sent (D4).
    /// Always `Some` for an admitted request: only a `Running` generation
    /// admits, and every `Running` one has an address.
    #[must_use]
    pub fn upstream(&self) -> Option<&std::path::Path> {
        self.generation.upstream()
    }

    /// The generation this request was admitted into.
    pub(crate) fn generation(&self) -> &Arc<Generation> {
        &self.generation
    }

    /// The kernel-minted request id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Mark the request as about to be sent to the module. `false` if the
    /// generation is already revoked — the caller must then NOT send it.
    ///
    /// Taken under the lock `revoke` takes, so the two are ordered: without it,
    /// a request whose client was still sending its body when the drain expired
    /// would finish reading and dial the module afterwards — the old process in
    /// its SIGTERM grace, or a restarted one on the same address.
    #[must_use]
    pub fn dispatch(&self) -> bool {
        let mut inner = self.generation.lock();
        if inner.state == DrainState::Revoked {
            return false;
        }
        inner.dispatched.insert(self.id.clone());
        true
    }

    /// Resolves once this request's generation is revoked — immediately if it
    /// already has been. `watch` rather than `Notify`: a subscriber sees the
    /// current value, so a revocation that lands before anyone waits is not a
    /// lost wakeup.
    pub async fn revoked(&self) {
        let mut rx = self.generation.revoked.subscribe();
        // Err means the sender is gone, which cannot happen while `self` holds
        // the generation; treat it as "never revoked" rather than as revoked.
        let _ = rx.wait_for(|revoked| *revoked).await;
    }

    /// The request is done — **this is the commit point.** `Ok` means the
    /// kernel passes the module's answer on; a revocation after this point does
    /// not reach back into a response already on its way to the client (its
    /// bytes may still be queued for a slow reader, and are still counted by the
    /// concurrency ceiling, but they are no longer this generation's business).
    /// `Err(Abandoned)` if the generation was revoked first.
    ///
    /// # Errors
    ///
    /// [`Abandoned`], as above.
    pub fn finish(mut self) -> Result<(), Abandoned> {
        // Leave the set and read the state under ONE lock. With two, a revoke
        // landing between them lists this request in `Revocation::abandoned`
        // while this returns `Ok` — the kernel would log "outcome unknown" and
        // report success for the same request.
        let (revoked, dispatched, entry) = {
            let mut inner = self.generation.lock();
            let entry = inner.in_flight.remove(&self.id);
            let dispatched = inner.dispatched.remove(&self.id);
            (inner.state == DrainState::Revoked, dispatched, entry)
        };
        // T8.5a decision 4: `send_replace` OUTSIDE the lock, same reason
        // `revoke` flips `self.revoked` after releasing it — never holding
        // `Mutex<Inner>` and `watch::Sender`'s own internal lock at once, so
        // the two never nest and cannot form a new lock order.
        if let Some(entry) = entry {
            entry.ended.send_replace(true);
        }
        self.finished = true;
        if revoked {
            Err(Abandoned { dispatched })
        } else {
            Ok(())
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if !self.finished {
            let entry = {
                let mut inner = self.generation.lock();
                inner.dispatched.remove(&self.id);
                inner.in_flight.remove(&self.id)
            };
            // See the comment in `finish`: outside the lock, same reason.
            if let Some(entry) = entry {
                entry.ended.send_replace(true);
            }
        }
    }
}

/// SHA-256 of `bytes` (T7b/ME-3e design doc, decision 2). Used to hash the
/// plaintext `X-A24-Approval-Token` presented in a `gate`/`advise` submission
/// against the hash [`Generation::admit_request`] stored — the plaintext
/// itself is never kept. `pub(crate)`: `proxy.rs` hashes the token it mints
/// with this same function, so minting and verifying can never disagree on
/// the hash.
pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

/// Constant-time comparison — a copy of `agent24d::server::constant_time_eq`
/// (T7b/ME-3e design doc, decision 2: the two crates do not depend on each
/// other, so the one function is duplicated rather than the dependency
/// added). Length is checked first — a length mismatch is not the secret
/// being timed, so this branch not being constant-time gives nothing away.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A generation with a process behind it — the address is never dialled
    /// by these tests.
    fn spawned() -> Arc<Generation> {
        Generation::serving_at("/tmp/a24-test.sock".into())
    }

    fn running() -> Arc<Generation> {
        let g = spawned();
        assert!(g.ready());
        g
    }

    const GRACE: Duration = Duration::from_secs(10);

    /// SPEC §8 ME-3b: *"DRAINING 期间新的被代理请求 503、在途请求的回调仍可用"*.
    /// Both halves in one test, because each is only meaningful next to the
    /// other: a generation that refused everything would pass the first half.
    #[test]
    fn draining_refuses_new_requests_but_not_the_ones_in_flight() {
        let g = running();
        let a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(g.begin_drain(Instant::now(), GRACE));

        assert_eq!(
            g.admit_request(
                "b".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30)
            )
            .unwrap_err(),
            RequestRefused::Draining
        );
        assert_eq!(RequestRefused::Draining.code(), "module_draining");
        // The in-flight one still has its callbacks ...
        assert_eq!(g.admit_callback(Some("a")), Ok(()));
        // ... and finishes normally.
        assert_eq!(a.finish(), Ok(()));
    }

    /// SPEC §8: *"不带 `request_id` 的回调被拒"* — and the two shapes next to it
    /// that must NOT be confused with it: an id that is not in flight is refused
    /// for its own reason, and in `Running` all three are admitted (the control:
    /// without it, a generation that refuses every callback passes).
    #[test]
    fn draining_admits_only_callbacks_that_name_a_live_request() {
        let g = running();
        let a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let done = g
            .admit_request(
                "done".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        done.finish().unwrap();

        // Control: Running admits all three shapes.
        assert_eq!(g.admit_callback(Some("a")), Ok(()));
        assert_eq!(g.admit_callback(None), Ok(()));
        assert_eq!(g.admit_callback(Some("done")), Ok(()));

        assert!(g.begin_drain(Instant::now(), GRACE));
        assert_eq!(g.admit_callback(Some("a")), Ok(()));
        assert_eq!(
            g.admit_callback(None),
            Err(CallbackRefused::DrainingWithoutRequest)
        );
        assert_eq!(
            g.admit_callback(Some("done")),
            Err(CallbackRefused::DrainingUnknownRequest),
            "an id that has finished is not a pass for background work"
        );
        drop(a);
    }

    /// SPEC §4 REVOKING: *"此后**一切**回调拒绝，包括在途的"*.
    #[test]
    fn revoking_refuses_every_callback_including_in_flight_ones() {
        let g = running();
        let a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(g.begin_drain(Instant::now(), GRACE));
        assert_eq!(
            g.admit_callback(Some("a")),
            Ok(()),
            "control: before revoke"
        );

        let r = g.revoke().unwrap();
        // Admitted but never sent: its outcome is known.
        assert!(r.abandoned.is_empty());
        assert_eq!(r.never_sent, vec!["a".to_owned()]);
        assert_eq!(g.admit_callback(Some("a")), Err(CallbackRefused::Revoked));
        assert_eq!(g.admit_callback(None), Err(CallbackRefused::Revoked));
        assert_eq!(
            g.admit_request(
                "b".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30)
            )
            .unwrap_err(),
            RequestRefused::Stopping
        );
        // SPEC §8: *"drain 超时的在途请求返回 503(不假装成功)"*.
        assert_eq!(a.finish(), Err(Abandoned { dispatched: false }));
    }

    /// The drain ends at whichever comes first: in-flight reaching zero, or the
    /// grace running out. Both endings, plus the waiting state between them.
    #[test]
    fn a_drain_ends_when_idle_or_when_the_grace_runs_out() {
        let t = Instant::now();

        let g = running();
        let a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(g.begin_drain(t, GRACE));
        assert_eq!(
            g.progress(t + Duration::from_secs(3)),
            DrainProgress::Waiting {
                in_flight: 1,
                remaining: Duration::from_secs(7)
            }
        );
        a.finish().unwrap();
        assert_eq!(g.progress(t + Duration::from_secs(3)), DrainProgress::Idle);

        let g = running();
        let _b = g
            .admit_request(
                "b".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(g.begin_drain(t, GRACE));
        assert_eq!(
            g.progress(t + GRACE),
            DrainProgress::Expired { in_flight: 1 },
            "at the deadline, not one tick after it"
        );
    }

    /// A handler that is cancelled or panics drops its `InFlight` without
    /// calling `finish`. If that left the id behind, every drain after it would
    /// wait out the full grace for a request that no longer exists.
    #[test]
    fn a_dropped_request_leaves_the_in_flight_set() {
        let g = running();
        let a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        drop(a);
        assert!(g.begin_drain(Instant::now(), GRACE));
        assert_eq!(g.progress(Instant::now()), DrainProgress::Idle);
    }

    /// `progress` only answers while draining; before and after, it says so
    /// rather than reporting an idle drain that is not happening.
    #[test]
    fn progress_outside_a_drain_is_not_draining() {
        let g = running();
        let _a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(g.progress(Instant::now()), DrainProgress::NotDraining);
        // Revoked AFTER a drain began, so a deadline exists: `NotDraining` here
        // must come from the state, not from the deadline being absent.
        assert!(g.begin_drain(Instant::now(), GRACE));
        let _ = g.revoke();
        assert_eq!(g.progress(Instant::now()), DrainProgress::NotDraining);
    }

    /// `ready` only moves Starting → Running. A late or repeated `initialize`
    /// must not reopen a module that is draining.
    #[test]
    fn a_late_handshake_cannot_reopen_a_draining_module() {
        let g = running();
        assert!(!g.ready(), "a second initialize on a running module");
        assert!(g.begin_drain(Instant::now(), GRACE));
        assert!(!g.ready());
        assert_eq!(g.state(), DrainState::Draining);
        assert_eq!(
            g.admit_request(
                "b".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30)
            )
            .unwrap_err(),
            RequestRefused::Draining
        );
    }

    #[test]
    fn a_second_drain_does_not_extend_the_grace() {
        let t = Instant::now();
        let g = running();
        let _a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(g.begin_drain(t, GRACE));
        assert!(!g.begin_drain(t + Duration::from_secs(5), GRACE));
        assert_eq!(
            g.progress(t + GRACE),
            DrainProgress::Expired { in_flight: 1 }
        );
    }

    /// A placeholder has no process behind it, so it can never become
    /// `Running` — and so a `Running` generation always has an address to send
    /// a request to (SUP-3b). Control: one with an address can.
    #[test]
    fn a_placeholder_can_never_become_running() {
        let g = Generation::starting();
        assert!(!g.ready(), "a placeholder became Running");
        assert_eq!(g.state(), DrainState::Starting);
        assert_eq!(g.upstream(), None);
        let s = spawned();
        assert!(s.ready());
        assert!(s.upstream().is_some());
    }

    /// Before `initialize` nothing is authorised, and there is nothing to drain:
    /// the only way out is revocation.
    #[test]
    fn a_module_that_is_not_ready_admits_nothing_and_goes_straight_to_revoked() {
        let g = Generation::starting();
        assert_eq!(
            g.admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30)
            )
            .unwrap_err(),
            RequestRefused::NotReady
        );
        assert_eq!(g.admit_callback(None), Err(CallbackRefused::NotReady));
        assert!(!g.begin_drain(Instant::now(), GRACE));
        let r = g.revoke().unwrap();
        assert!(r.abandoned.is_empty() && r.never_sent.is_empty());
        // A revoked generation cannot be revived by a late handshake.
        assert!(!g.ready());
        assert_eq!(g.state(), DrainState::Revoked);
    }

    /// A restart swaps the slot. The request admitted by the old run stays with
    /// the old run: revoking it abandons that request, while the new run serves
    /// new ones. The control is the new run admitting at all — a slot that
    /// never moved would pass the first half.
    #[test]
    fn a_restart_does_not_carry_a_request_in_flight_into_the_next_run() {
        let current = Current::new(running());
        let old = current.get();
        let a = old
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();

        let next = running();
        let replaced = current.replace(Arc::clone(&next));
        assert!(Arc::ptr_eq(&replaced, &old));
        // `replace` does not revoke: whoever stops a run does, through the one
        // path that yields a permit.
        assert_eq!(replaced.state(), DrainState::Running);
        let _ = replaced.revoke();

        assert_eq!(a.finish(), Err(Abandoned { dispatched: false }));
        assert!(
            current
                .get()
                .admit_request(
                    "b".into(),
                    [0u8; 32],
                    Instant::now(),
                    Duration::from_secs(30)
                )
                .is_ok()
        );
        assert!(Arc::ptr_eq(&current.get(), &next));
    }

    /// Revocation and "about to send" are ordered: after the revoke, a request
    /// that has not been sent never will be; one that was sent is reported as
    /// such. The control is the dispatch that happens BEFORE the revoke.
    #[test]
    fn after_revocation_nothing_more_is_sent_and_what_was_sent_is_reported() {
        let g = running();
        let sent = g
            .admit_request(
                "sent".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let unsent = g
            .admit_request(
                "unsent".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(sent.dispatch(), "control: a running generation may send");

        let r = g.revoke().unwrap();
        assert_eq!(r.abandoned, vec!["sent".to_owned()]);
        assert_eq!(r.never_sent, vec!["unsent".to_owned()]);
        assert!(
            !unsent.dispatch(),
            "a revoked generation's request was sent"
        );

        assert_eq!(sent.finish(), Err(Abandoned { dispatched: true }));
        assert_eq!(unsent.finish(), Err(Abandoned { dispatched: false }));
    }

    /// One revocation, one permit: a crash and a disable racing to stop the same
    /// run cannot both kill it.
    #[test]
    fn revoking_twice_yields_one_permit() {
        let g = running();
        assert!(g.revoke().is_some());
        assert!(g.revoke().is_none());
        assert_eq!(g.state(), DrainState::Revoked);
    }

    /// A grace that cannot be added to `now` is refused, and leaves the
    /// generation running — not draining with no deadline.
    #[test]
    fn an_unrepresentable_grace_is_refused_without_half_starting_a_drain() {
        let g = running();
        assert!(!g.begin_drain(Instant::now(), Duration::MAX));
        assert_eq!(g.state(), DrainState::Running);
        assert!(
            g.begin_drain(Instant::now(), GRACE),
            "control: a normal grace works"
        );
    }

    #[test]
    fn a_duplicate_id_is_refused_rather_than_aliased() {
        let g = running();
        let _a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(
            g.admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30)
            )
            .unwrap_err(),
            RequestRefused::DuplicateId
        );
    }

    /// The four 503 codes are distinct — an operator reads them to tell "it is
    /// stopping" from "it is not up yet" (SPEC §2.1).
    #[test]
    fn every_refusal_has_its_own_code() {
        let codes: HashSet<_> = [
            RequestRefused::NotReady,
            RequestRefused::Draining,
            RequestRefused::Stopping,
            RequestRefused::DuplicateId,
        ]
        .map(RequestRefused::code)
        .into_iter()
        .collect();
        assert_eq!(codes.len(), 4);
    }

    /// `Generation::revoked` waits until the generation is revoked — not
    /// before (the control: still pending) — and returns at once after, even
    /// for a waiter that arrives late.
    #[tokio::test]
    async fn waiting_for_a_revocation_ends_when_it_happens() {
        let g = Generation::starting();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), g.revoked())
                .await
                .is_err(),
            "resolved before any revocation"
        );
        let _ = g.revoke();
        tokio::time::timeout(std::time::Duration::from_secs(1), g.revoked())
            .await
            .expect("a late waiter was not told");
    }

    // ── T7b/ME-3e: `admit_approval_callback` (design doc decision 2) ──────
    //
    // These test the raw primitive directly — the state-machine fact that a
    // token is checked-and-consumed as one atomic step. The higher-level
    // policy of a wire resubmission being idempotent (design doc judgement
    // 16a) is a DIFFERENT layer (`ModuleApprovalBroker`'s idempotency
    // lookup, which runs BEFORE this primitive is ever called again for the
    // same `(module, request_id, kind)`); it is tested in `agent24d`, not
    // here.

    fn admit(g: &Arc<Generation>, id: &str, token: &str) -> InFlight {
        g.admit_request(
            id.to_owned(),
            sha256(token.as_bytes()),
            Instant::now(),
            Duration::from_secs(30),
        )
        .unwrap()
    }

    #[test]
    fn a_fresh_token_admits_exactly_once_then_is_invalid() {
        let g = running();
        let _in_flight = admit(&g, "r1", "secret-1");
        // Judgement 3, control: the first use succeeds.
        assert_eq!(g.admit_approval_callback("r1", "secret-1"), Ok(()));
        // Judgement 3: the SAME token, used again for the SAME request,
        // is rejected — `admit_approval_callback` marks it used atomically
        // with the check, so a second raw call can never pass.
        assert_eq!(
            g.admit_approval_callback("r1", "secret-1"),
            Err(ApprovalCallbackRefused::TokenInvalid)
        );
    }

    #[test]
    fn mismatched_id_and_token_pairs_are_both_refused() {
        let g = running();
        let _a = admit(&g, "a", "token-a");
        let _b = admit(&g, "b", "token-b");
        // Judgement 4: A's id with B's token, and the reverse.
        assert_eq!(
            g.admit_approval_callback("a", "token-b"),
            Err(ApprovalCallbackRefused::TokenInvalid)
        );
        assert_eq!(
            g.admit_approval_callback("b", "token-a"),
            Err(ApprovalCallbackRefused::TokenInvalid)
        );
        // Control: the correct pairing on either side still works.
        assert_eq!(g.admit_approval_callback("a", "token-a"), Ok(()));
        assert_eq!(g.admit_approval_callback("b", "token-b"), Ok(()));
    }

    #[test]
    fn a_token_from_a_different_generation_is_refused() {
        // Judgement 5: a module that restarted (a fresh `Generation`) cannot
        // use a token minted for its previous run — the two `in_flight`
        // maps are entirely separate objects.
        let old_gen = running();
        let _a = admit(&old_gen, "r1", "secret-1");
        assert_eq!(old_gen.admit_approval_callback("r1", "secret-1"), Ok(()));

        let new_gen = running();
        assert_eq!(
            new_gen.admit_approval_callback("r1", "secret-1"),
            Err(ApprovalCallbackRefused::TokenInvalid)
        );
    }

    #[test]
    fn a_finished_requests_token_is_refused() {
        // Judgement 6: once a request has `finish()`ed, its id leaves
        // `in_flight` — its token (used or not) admits nothing afterwards.
        let g = running();
        let in_flight = admit(&g, "r1", "secret-1");
        in_flight.finish().unwrap();
        assert_eq!(
            g.admit_approval_callback("r1", "secret-1"),
            Err(ApprovalCallbackRefused::TokenInvalid)
        );
    }

    #[test]
    fn admit_approval_callback_follows_the_designed_error_matrix() {
        // Row 1: Starting → not_ready, regardless of the pair.
        let starting = Generation::starting();
        assert_eq!(
            starting.admit_approval_callback("r1", "secret-1"),
            Err(ApprovalCallbackRefused::NotReady)
        );

        // Row 2: Revoked → revoked, regardless of the pair (even one that
        // was live moments before).
        let g = running();
        let _a = admit(&g, "r1", "secret-1");
        let _ = g.revoke();
        assert_eq!(
            g.admit_approval_callback("r1", "secret-1"),
            Err(ApprovalCallbackRefused::Revoked)
        );

        // Rows 3/4 (never in flight; wrong/used token) are covered by the
        // other tests above. Row 5 (success) is the control throughout.
    }

    #[test]
    fn draining_still_admits_a_still_in_flight_requests_token() {
        // Design doc "现状" 2 / decision 2: Running and Draining are treated
        // identically for approval submission — an approval submission
        // always carries a live `request_id`, unlike a background
        // `_a24/events/emit` callback with none.
        let g = running();
        let in_flight = admit(&g, "r1", "secret-1");
        assert!(g.begin_drain(Instant::now(), GRACE));
        assert_eq!(g.admit_approval_callback("r1", "secret-1"), Ok(()));
        drop(in_flight);
    }

    // ── T8.5a: request lifecycle signal (design doc judgements 1/3/4/5/7/8/9a) ──

    /// Judgement 1: `remaining` is computed from the query time, not fixed at
    /// admission — using fixed `Instant` values, no real sleep. Positive
    /// control: queried earlier, more is left, proving the value tracks the
    /// query time rather than being pinned to a short constant.
    #[test]
    fn remaining_reflects_the_query_time_not_a_fixed_value() {
        let t0 = Instant::now();
        let g = running();
        let _in_flight = g
            .admit_request("r1".into(), [0u8; 32], t0, Duration::from_secs(30))
            .unwrap();
        let lifecycle = g.request_lifecycle("r1").unwrap();
        assert_eq!(
            lifecycle.remaining(t0 + Duration::from_secs(29)),
            Duration::from_secs(1)
        );
        assert_eq!(
            lifecycle.remaining(t0 + Duration::from_secs(1)),
            Duration::from_secs(29),
            "positive control: queried earlier, more time is left"
        );
    }

    /// Judgement 3: an id that was never admitted (a stranger, or one from
    /// another generation) reports `None` — and querying it does not perturb
    /// a DIFFERENT, live request in the same generation that is close to its
    /// own deadline (no cross-request interference).
    #[test]
    fn an_unknown_id_reports_none_without_cross_request_interference() {
        let t0 = Instant::now();
        let g = running();
        let _about_to_expire = g
            .admit_request(
                "about-to-expire".into(),
                [0u8; 32],
                t0,
                Duration::from_millis(1),
            )
            .unwrap();
        assert!(g.request_lifecycle("ghost").is_none());
        let another_gen = running();
        assert!(another_gen.request_lifecycle("about-to-expire").is_none());

        let lifecycle = g.request_lifecycle("about-to-expire").unwrap();
        assert_eq!(
            lifecycle.remaining(t0),
            Duration::from_millis(1),
            "querying an unrelated id must not perturb this one"
        );
    }

    /// Judgement 4: a callback waiting on `ended()` is woken promptly when
    /// its bound request `finish()`es — well inside a short test timeout,
    /// not the request's real (long) remaining budget. Positive control: a
    /// DIFFERENT, still in-flight request's lifecycle is unaffected.
    #[tokio::test]
    async fn ended_resolves_promptly_after_finish() {
        let g = running();
        let a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let b = g
            .admit_request(
                "b".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let mut lifecycle_a = g.request_lifecycle("a").unwrap();
        let mut lifecycle_b = g.request_lifecycle("b").unwrap();

        a.finish().unwrap();
        tokio::time::timeout(Duration::from_millis(200), lifecycle_a.ended())
            .await
            .expect("ended() must resolve promptly after finish()");

        assert!(
            tokio::time::timeout(Duration::from_millis(50), lifecycle_b.ended())
                .await
                .is_err(),
            "positive control: an unrelated, still in-flight request must not be woken"
        );
        drop(b);
    }

    /// Judgement 5: same as above, but triggered by `Drop` (a handler
    /// panicking, being cancelled, or the client disconnecting) rather than
    /// `finish()` — the two paths must be equivalent to a waiting callback.
    #[tokio::test]
    async fn ended_resolves_promptly_after_drop() {
        let g = running();
        let a = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let mut lifecycle = g.request_lifecycle("a").unwrap();
        drop(a);
        tokio::time::timeout(Duration::from_millis(200), lifecycle.ended())
            .await
            .expect("ended() must resolve promptly after Drop");
    }

    /// Judgement 7: a request that is `revoke()`d but not yet `finish()`ed
    /// still reports `Some` from `request_lifecycle` (its pre-revoke
    /// deadline) — revoke alone does not end it. Only its own `finish()`
    /// (which now returns `Err(Abandoned)`) flips `ended`.
    #[tokio::test]
    async fn revoke_does_not_prematurely_end_a_still_in_flight_request() {
        let t0 = Instant::now();
        let g = running();
        let a = g
            .admit_request("a".into(), [0u8; 32], t0, Duration::from_secs(30))
            .unwrap();
        let mut lifecycle = g.request_lifecycle("a").unwrap();
        let _ = g.revoke();

        assert!(
            g.request_lifecycle("a").is_some(),
            "revoke alone must not remove the entry"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), lifecycle.ended())
                .await
                .is_err(),
            "ended() must not resolve from revoke alone"
        );

        assert_eq!(a.finish(), Err(Abandoned { dispatched: false }));
        tokio::time::timeout(Duration::from_millis(200), lifecycle.ended())
            .await
            .expect("finish() after revoke must still flip ended");
    }

    /// Judgement 8: an unrepresentable deadline (`now.checked_add(budget)`
    /// overflows) fails toward the SAFER extreme for this hot admission
    /// path — the request is still admitted, and its deadline collapses to
    /// `now` (so `remaining` is always zero) — unlike `begin_drain`, which
    /// refuses the whole operation on overflow.
    #[test]
    fn an_overflowing_deadline_admits_with_zero_remaining_rather_than_refusing() {
        let g = running();
        let now = Instant::now();
        let in_flight = g
            .admit_request("a".into(), [0u8; 32], now, Duration::MAX)
            .expect("admission must still succeed despite the overflow");
        let lifecycle = g.request_lifecycle("a").unwrap();
        assert_eq!(lifecycle.remaining(now), Duration::ZERO);
        assert_eq!(
            lifecycle.remaining(now + Duration::from_secs(1)),
            Duration::ZERO,
            "remaining stays saturated at zero, never wraps"
        );
        drop(in_flight);
    }

    // ── T8.5a judgement 9a: `bind_to_lifecycle` itself, via stub futures ──
    // (C1 fix — independent of any real business handler; `events_emit.rs`'s
    // own tests, judgement 9b, only check that it is WIRED correctly.)

    /// A `work` that never completes, bound to a request with a very short
    /// remaining budget: `bind_to_lifecycle` returns `BudgetExhausted` once
    /// that budget elapses. Real, short wall-clock wait (L-new1: accepted —
    /// `admit_request` reads no injectable clock; `proxy.rs`'s own 504 tests
    /// use the same shape).
    #[tokio::test]
    async fn bind_to_lifecycle_times_out_when_the_budget_is_exhausted() {
        let g = running();
        let in_flight = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_millis(50),
            )
            .unwrap();
        let lifecycle = g.request_lifecycle("a").unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            bind_to_lifecycle(Some(lifecycle), std::future::pending::<()>()),
        )
        .await
        .expect("bind_to_lifecycle must resolve well inside the 1s outer timeout");
        assert_eq!(result, Err(LifecycleTimeout::BudgetExhausted));
        drop(in_flight);
    }

    /// A `work` that never completes, bound to a request with a generous
    /// (30s) remaining budget: `finish()`ing that request resolves
    /// `bind_to_lifecycle` with `RequestEnded` almost immediately — far
    /// short of the 30s budget, proving the `ended()` arm won the race, not
    /// the timeout arm.
    #[tokio::test]
    async fn bind_to_lifecycle_ends_promptly_when_its_request_finishes() {
        let g = running();
        let in_flight = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let lifecycle = g.request_lifecycle("a").unwrap();
        let bound = tokio::spawn(bind_to_lifecycle(
            Some(lifecycle),
            std::future::pending::<()>(),
        ));
        in_flight.finish().unwrap();
        let result = tokio::time::timeout(Duration::from_millis(500), bound)
            .await
            .expect("bind_to_lifecycle must resolve well inside the remaining 30s budget")
            .unwrap();
        assert_eq!(result, Err(LifecycleTimeout::RequestEnded));
    }

    /// Same as above, but triggered by `Drop` (mirrors judgement 5, this
    /// time observed through `bind_to_lifecycle` rather than
    /// `RequestLifecycle::ended` directly).
    #[tokio::test]
    async fn bind_to_lifecycle_ends_promptly_when_its_request_is_dropped() {
        let g = running();
        let in_flight = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let lifecycle = g.request_lifecycle("a").unwrap();
        let bound = tokio::spawn(bind_to_lifecycle(
            Some(lifecycle),
            std::future::pending::<()>(),
        ));
        drop(in_flight);
        let result = tokio::time::timeout(Duration::from_millis(500), bound)
            .await
            .expect("bind_to_lifecycle must resolve well inside the remaining 30s budget")
            .unwrap();
        assert_eq!(result, Err(LifecycleTimeout::RequestEnded));
    }

    /// Positive control for both of the above: `work` finishing first (and
    /// promptly) returns `Ok`, unaffected by an unexpired deadline or an
    /// untriggered `ended()` — the FU-54 acceptance text's other half.
    #[tokio::test]
    async fn bind_to_lifecycle_returns_ok_when_work_finishes_first() {
        let g = running();
        let in_flight = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let lifecycle = g.request_lifecycle("a").unwrap();
        let result = bind_to_lifecycle(Some(lifecycle), std::future::ready(42)).await;
        assert_eq!(result, Ok(42));
        drop(in_flight);
    }

    /// Codex review round 1, Medium 1: `work` that has ALREADY produced a
    /// value by the time `bind_to_lifecycle` is first polled — exactly what
    /// `events_emit.rs` passes (`std::future::ready(sink.emit(...))`,
    /// evaluated eagerly before the call) — must win over a lifecycle that
    /// has ALSO already ended by then, deterministically, not by the luck of
    /// `tokio::select!`'s (otherwise random) tie-break: a caller that saw
    /// `Err(RequestEnded)` for a call whose side effect already landed has no
    /// way to know that and may retry, duplicating it. Run many trials: a
    /// non-`biased` `select!` would occasionally return `Err(RequestEnded)`
    /// here.
    #[tokio::test]
    async fn already_completed_work_wins_over_an_already_ended_lifecycle() {
        let g = running();
        let in_flight = g
            .admit_request(
                "a".into(),
                [0u8; 32],
                Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        let lifecycle = g.request_lifecycle("a").unwrap();
        in_flight.finish().unwrap();
        for _ in 0..200 {
            let result = bind_to_lifecycle(Some(lifecycle.clone()), std::future::ready(42)).await;
            assert_eq!(
                result,
                Ok(42),
                "already-completed work must win over an already-ended lifecycle"
            );
        }
    }

    /// Positive control, no lifecycle at all (no `request_id`, or one that
    /// query to `None`): behaves exactly like a plain `.await` — nothing
    /// `bind_to_lifecycle` itself imposes bounds it.
    #[tokio::test]
    async fn bind_to_lifecycle_with_no_lifecycle_just_awaits() {
        let result = bind_to_lifecycle(None, std::future::ready(42)).await;
        assert_eq!(result, Ok(42));
    }
}
