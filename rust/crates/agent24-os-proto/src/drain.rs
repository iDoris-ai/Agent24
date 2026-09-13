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

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

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

#[derive(Debug)]
struct Inner {
    state: DrainState,
    in_flight: HashSet<String>,
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
    /// Where this run's process listens for proxied requests: its own port
    /// (D4), so a request admitted into this generation reaches this process
    /// and no other. `None` for a placeholder, which never becomes `Running`.
    upstream: Option<std::net::SocketAddr>,
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
    pub fn serving_at(upstream: std::net::SocketAddr) -> Arc<Self> {
        Self::new(Some(upstream))
    }

    fn new(upstream: Option<std::net::SocketAddr>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                state: DrainState::Starting,
                in_flight: HashSet::new(),
                dispatched: HashSet::new(),
                drain_deadline: None,
            }),
            revoked: tokio::sync::watch::Sender::new(false),
            upstream,
        })
    }

    /// Where this run's process listens; `None` for a placeholder. Every
    /// `Running` generation has one (see [`Generation::ready`]).
    #[must_use]
    pub fn upstream(&self) -> Option<std::net::SocketAddr> {
        self.upstream
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

    /// Admit a new proxied request carrying the kernel-minted `id`.
    ///
    /// # Errors
    ///
    /// Anything but `Running` refuses, each state with its own reason.
    pub fn admit_request(self: &Arc<Self>, id: String) -> Result<InFlight, RequestRefused> {
        let mut inner = self.lock();
        match inner.state {
            DrainState::Starting => Err(RequestRefused::NotReady),
            DrainState::Draining => Err(RequestRefused::Draining),
            DrainState::Revoked => Err(RequestRefused::Stopping),
            DrainState::Running => {
                if !inner.in_flight.insert(id.clone()) {
                    return Err(RequestRefused::DuplicateId);
                }
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
                Some(id) if inner.in_flight.contains(id) => Ok(()),
                Some(_) => Err(CallbackRefused::DrainingUnknownRequest),
            },
        }
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
                .iter()
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
}

impl Current {
    #[must_use]
    pub fn new(generation: Arc<Generation>) -> Arc<Self> {
        Arc::new(Self {
            slot: Mutex::new(generation),
            held: std::sync::atomic::AtomicBool::new(false),
        })
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
    pub fn upstream(&self) -> Option<std::net::SocketAddr> {
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
        let (revoked, dispatched) = {
            let mut inner = self.generation.lock();
            inner.in_flight.remove(&self.id);
            let dispatched = inner.dispatched.remove(&self.id);
            (inner.state == DrainState::Revoked, dispatched)
        };
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
            let mut inner = self.generation.lock();
            inner.in_flight.remove(&self.id);
            inner.dispatched.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A generation with a process behind it — the address is never dialled
    /// by these tests.
    fn spawned() -> Arc<Generation> {
        Generation::serving_at("127.0.0.1:9".parse().unwrap())
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
        let a = g.admit_request("a".into()).unwrap();
        assert!(g.begin_drain(Instant::now(), GRACE));

        assert_eq!(
            g.admit_request("b".into()).unwrap_err(),
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
        let a = g.admit_request("a".into()).unwrap();
        let done = g.admit_request("done".into()).unwrap();
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
        let a = g.admit_request("a".into()).unwrap();
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
            g.admit_request("b".into()).unwrap_err(),
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
        let a = g.admit_request("a".into()).unwrap();
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
        let _b = g.admit_request("b".into()).unwrap();
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
        let a = g.admit_request("a".into()).unwrap();
        drop(a);
        assert!(g.begin_drain(Instant::now(), GRACE));
        assert_eq!(g.progress(Instant::now()), DrainProgress::Idle);
    }

    /// `progress` only answers while draining; before and after, it says so
    /// rather than reporting an idle drain that is not happening.
    #[test]
    fn progress_outside_a_drain_is_not_draining() {
        let g = running();
        let _a = g.admit_request("a".into()).unwrap();
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
            g.admit_request("b".into()).unwrap_err(),
            RequestRefused::Draining
        );
    }

    #[test]
    fn a_second_drain_does_not_extend_the_grace() {
        let t = Instant::now();
        let g = running();
        let _a = g.admit_request("a".into()).unwrap();
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
            g.admit_request("a".into()).unwrap_err(),
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
        let a = old.admit_request("a".into()).unwrap();

        let next = running();
        let replaced = current.replace(Arc::clone(&next));
        assert!(Arc::ptr_eq(&replaced, &old));
        // `replace` does not revoke: whoever stops a run does, through the one
        // path that yields a permit.
        assert_eq!(replaced.state(), DrainState::Running);
        let _ = replaced.revoke();

        assert_eq!(a.finish(), Err(Abandoned { dispatched: false }));
        assert!(current.get().admit_request("b".into()).is_ok());
        assert!(Arc::ptr_eq(&current.get(), &next));
    }

    /// Revocation and "about to send" are ordered: after the revoke, a request
    /// that has not been sent never will be; one that was sent is reported as
    /// such. The control is the dispatch that happens BEFORE the revoke.
    #[test]
    fn after_revocation_nothing_more_is_sent_and_what_was_sent_is_reported() {
        let g = running();
        let sent = g.admit_request("sent".into()).unwrap();
        let unsent = g.admit_request("unsent".into()).unwrap();
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
        let _a = g.admit_request("a".into()).unwrap();
        assert_eq!(
            g.admit_request("a".into()).unwrap_err(),
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
}
