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
//! the order holds until the first caller who did not. So
//! [`crate::supervise::terminate_group`] takes a [`KillPermit`], and the only
//! way to obtain one is [`Generation::revoke`]:
//!
//! ```compile_fail
//! // There is no way to build a permit by hand ...
//! let permit = agent24_os_proto::drain::KillPermit { _private: () };
//! ```
//!
//! ```compile_fail
//! # fn f(child: &mut std::process::Child) {
//! // ... and no way to kill without one.
//! agent24_os_proto::supervise::terminate_group(child, std::time::Duration::from_secs(1));
//! # }
//! ```
//!
//! The control for both — the one legal order, which must compile:
//!
//! ```no_run
//! # fn f(child: &mut std::process::Child) {
//! use agent24_os_proto::drain::Generation;
//! let generation = Generation::starting();
//! let revocation = generation.revoke();
//! agent24_os_proto::supervise::terminate_group(revocation.permit, child, std::time::Duration::from_secs(1));
//! # }
//! ```
//!
//! **Why the control is there.** Stable rustdoc does not check the error code of
//! a `compile_fail` block — measured: `compile_fail,E0999` passes. So each of
//! the two blocks above passes for ANY compile error, a typo included, and only
//! the control shows that the same shape compiles when the permit is
//! legitimate. The mutation that proves the first one bears weight is making
//! `_private` public: it must turn that block red.
//!
//! Every kill path goes through here — a disable, a crash (helpers outlive a
//! dead leader and may still hold the callback connection), a startup timeout
//! (nothing was ever admitted, so revoking costs nothing). None is exempt,
//! because an exemption is a constructor.
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
/// Not `Clone`, no public constructor: [`Generation::revoke`] is the only source,
/// so holding one proves the generation it came from was revoked first.
#[derive(Debug)]
pub struct KillPermit {
    _private: (),
}

/// What [`Generation::revoke`] hands back.
#[derive(Debug)]
pub struct Revocation {
    /// Allows [`crate::supervise::terminate_group`].
    pub permit: KillPermit,
    /// Requests still in flight at the moment of revocation. Their outcome is
    /// **unknown** — the module may or may not have acted on them — and the
    /// caller must say so (a 503, and a log line), never report success.
    pub abandoned: Vec<String>,
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
    drain_deadline: Option<Instant>,
}

/// One run of one module process. Shared (`Arc`) between the proxy, the callback
/// channel and whoever stops the module.
#[derive(Debug)]
pub struct Generation {
    inner: Mutex<Inner>,
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
    /// A freshly spawned module, before `initialize`.
    #[must_use]
    pub fn starting() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                state: DrainState::Starting,
                in_flight: HashSet::new(),
                drain_deadline: None,
            }),
        })
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

    /// `initialize` succeeded. Only from `Starting`; returns `false` otherwise
    /// (a generation that was revoked while starting stays revoked).
    #[must_use]
    pub fn ready(&self) -> bool {
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
        let mut inner = self.lock();
        if inner.state == DrainState::Running {
            inner.state = DrainState::Draining;
            inner.drain_deadline = Some(now + grace);
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
    /// callback, in flight or not. Legal from every state, because every kill
    /// path needs it (see the module docs); calling it twice yields a second
    /// permit for a process that is already being killed, which is harmless.
    ///
    /// The in-flight set is **kept**: an [`InFlight`] finishing after this
    /// point learns that it was abandoned from [`InFlight::finish`].
    #[must_use]
    pub fn revoke(&self) -> Revocation {
        let mut inner = self.lock();
        inner.state = DrainState::Revoked;
        let mut abandoned: Vec<String> = inner.in_flight.iter().cloned().collect();
        abandoned.sort();
        Revocation {
            permit: KillPermit { _private: () },
            abandoned,
        }
    }
}

/// Returned by [`InFlight::finish`] when the generation was revoked while the
/// request was in flight: whatever the module answered, the kernel does not
/// pass it on as a success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abandoned;

impl InFlight {
    /// The kernel-minted request id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The request is done. `Err(Abandoned)` if its generation was revoked in
    /// the meantime — the caller answers 503 and logs the outcome as unknown.
    ///
    /// # Errors
    ///
    /// [`Abandoned`], as above.
    pub fn finish(mut self) -> Result<(), Abandoned> {
        // Leave the set and read the state under ONE lock. With two, a revoke
        // landing between them lists this request in `Revocation::abandoned`
        // while this returns `Ok` — the kernel would log "outcome unknown" and
        // report success for the same request.
        let revoked = {
            let mut inner = self.generation.lock();
            inner.in_flight.remove(&self.id);
            inner.state == DrainState::Revoked
        };
        self.finished = true;
        if revoked { Err(Abandoned) } else { Ok(()) }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if !self.finished {
            self.generation.lock().in_flight.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn running() -> Arc<Generation> {
        let g = Generation::starting();
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

        let r = g.revoke();
        assert_eq!(r.abandoned, vec!["a".to_owned()]);
        assert_eq!(g.admit_callback(Some("a")), Err(CallbackRefused::Revoked));
        assert_eq!(g.admit_callback(None), Err(CallbackRefused::Revoked));
        assert_eq!(
            g.admit_request("b".into()).unwrap_err(),
            RequestRefused::Stopping
        );
        // SPEC §8: *"drain 超时的在途请求返回 503(不假装成功)"*.
        assert_eq!(a.finish(), Err(Abandoned));
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
        let r = g.revoke();
        assert!(r.abandoned.is_empty());
        // A revoked generation cannot be revived by a late handshake.
        assert!(!g.ready());
        assert_eq!(g.state(), DrainState::Revoked);
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
}
