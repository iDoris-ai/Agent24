//! A3-2b (`docs/design/A3-ATTACHED-MODULE.md` §4.3, §5) — the live,
//! in-memory counterpart to [`crate::attached`]'s on-disk store: one
//! [`agent24_os_proto::attach::AttachSlot`] per registered module, guarded by
//! ONE lock, exactly the shape that crate's own module doc calls for ("the
//! caller's registry lock is the only synchronisation").
//!
//! Two things read this registry, and neither takes the lock across an
//! `await`:
//!
//! - [`AttachRegistry::expectation`] is the pure-check `lookup` callback
//!   [`agent24_os_proto::initialize::accept_attached`] takes (§4.3 ①) — a
//!   quick clone of what a handshake needs to verify, taken and released
//!   before any secret comparison happens.
//! - [`AttachRegistry::commit`] is the locked re-check-and-install step (§4.3
//!   ②): re-confirm the record is still the one `expectation` saw (by
//!   `token_id`, in case of a rotation racing the handshake), not disabled,
//!   and install a fresh [`agent24_os_proto::drain::Generation`] via
//!   [`AttachSlot::install`] — all inside the one lock, so "check then
//!   install" cannot be split by a concurrent register/revoke/disable.
//!
//! [`crate::attached::register`]/[`crate::attached::revoke`]/
//! [`crate::attached::set_disabled`] call back into this registry's
//! [`AttachRegistry::on_change`] from inside their OWN file-lock critical
//! section (`Change`'s own doc comment) — so a registration change and a
//! concurrent handshake commit can never observe each other half-applied:
//! whichever lock (the file lock around `on_change`, or this registry's own
//! lock inside `commit`) gets there first is the one that decides.

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use agent24_domain::{Capability, DomainOsManifest, EventBroadcast, EventSink, Grants};
use agent24_os_proto::attach::AttachSlot;
use agent24_os_proto::attach_mux::{KernelCalls, MAX_KERNEL_CALLS_IN_FLIGHT};
use agent24_os_proto::drain::{DrainState, Generation};
use agent24_os_proto::initialize::{AttachedAccepted, AttachedExpectation, HandshakeError, Offer};
use agent24_os_proto::rpc::Methods;
use agent24_os_proto::supervisor::MethodsFor;

use crate::attached::Change;

/// A3-3 (`docs/design/A3-ATTACHED-MODULE.md` §6.1): why a reverse command
/// could not be sent once it has already passed the cheap step ①/② pre-check
/// ([`CommandLookupError`]) — returned by [`AttachRegistry::reserve_ready`].
///
/// Review M1: `NotDeclared` exists because the pre-check and the actual
/// reserve-and-send are two separate lock acquisitions, with the request
/// body read (and, in production, real time) in between — long enough for a
/// rotation to swap in a manifest that no longer declares this command.
/// `reserve_ready` re-checks `host_commands` itself, atomically with the
/// readiness/busy check, so THIS is the check that actually closes the gap;
/// the REST handler's earlier `declared_command` call is purely a cheap
/// fail-fast (avoid reading/validating a body for a command that was never
/// going anywhere), not the enforcement point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRefusal {
    /// Re-checked at reserve time and no longer declared (§6.1 step ②,
    /// re-verified) — REST `403`, zero frames sent. Also covers `name` having
    /// disappeared from the registry entirely between the pre-check and here
    /// (`Revoked` removes the whole entry) — indistinguishable from "not
    /// declared" from the caller's point of view, and the same status either
    /// way.
    NotDeclared,
    /// No live generation for this module right now (never attached, the
    /// connection ended, or it was just closed out from under a reservation
    /// that already happened — review M2) — §6.1 step ④, REST
    /// `503 module_not_ready`.
    NotReady,
    /// §6.1's in-flight cap ([`MAX_KERNEL_CALLS_IN_FLIGHT`]) is already
    /// reached on this connection — REST `429 busy`, zero frames sent.
    Busy,
}

/// Whether `name`/`command` even gets as far as being SENT — §6.1 steps ①/②,
/// checked by the REST handler before it reads/validates the request body
/// (so an unknown module or an undeclared command never pays for a body
/// read). This is a fail-fast convenience only: [`AttachRegistry::reserve_ready`]
/// re-checks the declaration atomically with readiness (review M1) and is
/// the check that actually matters for correctness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLookupError {
    /// §6.1 step ①: this name is not a registered attached module at all —
    /// REST `404`.
    UnknownModule,
    /// §6.1 step ②: registered, but this manifest's `host_commands` does not
    /// list `command` — REST `403`, zero frames sent.
    CommandNotDeclared,
}

/// A reservation against the in-flight cap for one command call, holding the
/// [`KernelCalls`] handle to actually send it. Dropping this (on any path —
/// success, error, or the caller's future itself being cancelled) releases
/// the reservation, so the REST handler needs no explicit cleanup code for
/// any of its many exit points.
pub struct CommandSlot {
    calls: KernelCalls,
    in_flight: Arc<AtomicUsize>,
}

impl CommandSlot {
    #[must_use]
    pub fn calls(&self) -> &KernelCalls {
        &self.calls
    }
}

// `KernelCalls` itself is not `Debug` (attach_mux.rs never needed it to be),
// so this is written by hand rather than derived — needed only so tests can
// `unwrap_err()` a `Result<CommandSlot, _>`.
impl std::fmt::Debug for CommandSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandSlot").finish_non_exhaustive()
    }
}

impl Drop for CommandSlot {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The live [`KernelCalls`] handle for one module's CURRENT generation, plus
/// the per-connection in-flight counter §6.1's `429 busy` cap is measured
/// against. Reset (a fresh counter) every time a new generation installs its
/// calls handle ([`AttachRegistry::attach_kernel_calls`]) — an in-flight
/// count from a connection that has already ended is meaningless.
///
/// Review M2: every place this registry stops considering a generation live
/// (rotation, disable, `release`, `revoke_all`) calls `calls.close()` WHILE
/// STILL HOLDING the registry lock, before or as it drops this — see
/// [`KernelCalls::close`]'s own doc for why that ordering is the actual fix,
/// not merely tidy-up: it closes the window between "the registry decided
/// this connection is gone" and "the wire itself finishes tearing down",
/// during which a `CommandSlot` reserved just before the decision could
/// otherwise still get its frame onto the wire.
struct LiveCalls {
    generation: Arc<Generation>,
    calls: KernelCalls,
    in_flight: Arc<AtomicUsize>,
}

/// A3 P2 (design §1 "本期只用 `events`、`models`"): the only two capabilities
/// an attached module may be granted. Deliberately narrower than
/// `crate::domain::KERNEL_OOP_GRANTS` — `memory`/`scheduler`/`approval` have
/// no attach-side story yet (no partition-allocation pass runs for an
/// attached module, and `host_commands` — A3's own reverse channel — is not
/// one of `agent24_domain::Capability`'s variants), so granting them here
/// would be scope no judgement or design section asks for. A manifest MAY
/// still declare them (`kernel_capabilities` is an open list at the manifest
/// layer); `Grants::granting`'s intersection with this list is what refuses
/// them, the same "asking gains nothing" rule every other grant list in this
/// daemon already documents.
const ATTACH_OOP_GRANTS: &[Capability] = &[Capability::Events, Capability::Models];

/// Daemon-level handles an attach registration needs to build a module's
/// `Methods`/`ModelGrant`/`EventSink` — a small, `Clone`-friendly MIRROR of
/// `crate::domain::CallbackDeps`, which is deliberately NOT `Clone` (see its
/// own doc comment: `mount_all` relies on being the only place that struct is
/// ever dropped). This is what design §5.5 (M2) calls "AppState 持有
/// AttachRegistry（含 CallbackDeps 克隆）" — the clone lives here, one level
/// down, rather than on `AppState` directly.
#[derive(Clone)]
pub struct AttachDeps {
    pub scheduler: Arc<agent24_scheduler::Scheduler>,
    /// `None` when this daemon has no model deps to hand out — same meaning,
    /// same reason, as `CallbackDeps.models` (design §2.4/§10.1): an attach
    /// registration requesting `models` while this is `None` gets no grant,
    /// same outcome as not holding the capability.
    pub models: Option<crate::model_callback::ModelCallbackDeps>,
    pub approval_broker: Arc<crate::module_approval_broker::ModuleApprovalBroker>,
    pub events: crate::events::EventsHub,
}

/// Everything built once per manifest ADMISSION (a first-time registration,
/// or a full re-registration whose digest changed, §3.4) and reused across
/// every reconnect that keeps the same digest (a token-only rotation, or a
/// plain reconnect after a dropped connection): the `Offer` a handshake
/// answers with, and the `MethodsFor` closure that turns a fresh
/// `Generation` into that generation's `Methods` — the same `MethodsFor`
/// shape `crate::domain::build_methods_for` already gives a mounted
/// package's supervisor, called here once per commit instead of once per
/// process restart.
struct Grant {
    offer: Offer,
    methods_for: MethodsFor,
}

fn build_grant(name: &str, manifest: &DomainOsManifest, deps: &AttachDeps) -> Grant {
    let granted = Grants::granting(manifest.kernel_capabilities(), ATTACH_OOP_GRANTS);
    let broadcast: Arc<dyn EventBroadcast> =
        Arc::new(crate::domain::HubBroadcast(deps.events.clone()));
    let event_sink = granted
        .has(Capability::Events)
        .then(|| Arc::new(EventSink::new(manifest, broadcast)));
    // §4.3 C2's positive control: `_a24/model/` is listed in `provides` only
    // when `model_grant` actually built (deps present AND the capability was
    // granted) — never merely because the manifest asked for `models`.
    let model_grant = crate::model_callback::model_grant(
        name,
        manifest.model_access(),
        &granted,
        deps.models.as_ref(),
    );
    let mut provides = Vec::new();
    if granted.has(Capability::Events) {
        provides.push("_a24/events/".to_owned());
    }
    if model_grant.is_some() {
        provides.push("_a24/model/".to_owned());
    }
    let methods_for = crate::domain::build_methods_for(
        name.to_owned(),
        granted,
        event_sink,
        deps.approval_broker.clone(),
        // A3 P2 has no memory story (see `ATTACH_OOP_GRANTS`'s doc) — every
        // attached module gets `MemoryEntitlement::NONE`, same as a mounted
        // package that was never granted `Memory` or whose lend failed.
        crate::os_memory::MemoryEntitlement::NONE,
        deps.scheduler.clone(),
        model_grant,
    );
    Grant {
        offer: Offer { provides },
        methods_for,
    }
}

/// One registered module's live state: the record facts a handshake
/// re-checks (§4.3 ②), whether it is disabled (§5.3), its
/// [`AttachSlot`], and the [`Grant`] §3.4 says to reuse or rebuild.
struct Entry {
    manifest_digest: String,
    token_sha256: [u8; 32],
    token_id: String,
    disabled: bool,
    slot: AttachSlot,
    grant: Arc<Grant>,
    /// A3-3 (§3.1, §6.1 step ②): the manifest's declared `host_commands`,
    /// kept alongside the other per-registration facts so
    /// [`AttachRegistry::declared_command`]/[`AttachRegistry::reserve_ready`]
    /// need no second lookup into storage. Rebuilt from the manifest on
    /// every register/rotate/hydrate, same lifetime as `grant`/`manifest_digest`.
    host_commands: Vec<String>,
    /// The most recent generation this slot ever installed — kept after it
    /// ends (unlike `AttachSlot`, which clears its own `current` on
    /// `release`/`revoke`) purely so `GET /api/v1/attached` can report a
    /// `generation` number. Read through [`Generation::state`] to tell "was
    /// live" from "is live": an attached generation never re-enters
    /// `Running` once it leaves it (§5.3: no drain, no resume), so
    /// `DrainState::Running` is exactly "this is the current connection".
    last_generation: Option<(u64, Arc<Generation>)>,
    /// A3-3: the current connection's [`KernelCalls`] handle, installed by
    /// [`AttachRegistry::attach_kernel_calls`] once `serve_attached` has
    /// actually built one (strictly after `commit` installs `last_generation`
    /// — see that method's own doc for why the two cannot be one step).
    /// `None` whenever there is no live connection, OR a live connection
    /// exists but has not yet reached the point of installing its handle (a
    /// vanishingly short window right after `commit` returns) — either way
    /// §6.1 step ④ ("现役一代且 Ready") is unmet and a command gets
    /// `503 module_not_ready`, never a panic or a send into nothing.
    live_calls: Option<LiveCalls>,
}

/// Review M1: everything the lock protects. `deps` moved IN HERE (from a
/// plain field on [`AttachRegistry`]) because a plain field lives exactly as
/// long as the `Arc<AttachRegistry>` itself does — i.e. until the whole
/// process exits, since `AppState`/the router keep a clone. That meant an
/// `AttachRegistry` with ZERO attached modules still held a live
/// `ModelCallbackDeps` (via `deps.models`) for the rest of the process's
/// life, and `stop_usage_writer` waits for every clone of that struct's
/// `usage` sender to drop before it will finish — so every shutdown, even
/// with nothing ever attached, blocked on the writer's hard-stop deadline
/// (measured: ~1.5s, with `the model usage writer did not finish by the
/// modules deadline` in the log). Putting `deps` behind the SAME lock as the
/// entries lets [`AttachRegistry::revoke_all`] `take()` it at the exact
/// moment it revokes everything, so the registry's own clone drops on
/// schedule regardless of how many modules were ever attached.
struct RegistryState {
    entries: HashMap<String, Entry>,
    /// `None` once [`AttachRegistry::revoke_all`] has run — see this
    /// struct's own doc. Also the M2 ① signal: [`AttachRegistry::commit`]
    /// refuses with [`CommitRefused::Closed`] rather than touching `entries`
    /// once this is `None`, and [`AttachRegistry::on_change`] no-ops
    /// entirely (the daemon is exiting; nothing further done to `entries`
    /// would ever be observed).
    deps: Option<AttachDeps>,
}

/// The live registry: one [`Entry`] per name in `attached.json`, guarded by
/// one lock — see the module doc for why that lock is never held across an
/// `await`.
pub struct AttachRegistry {
    inner: Mutex<RegistryState>,
}

fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    // `is_ascii()` first: `str` indexing below is BYTE indexing (`hex[i*2..
    // i*2+2]`), which panics if it lands inside a multi-byte UTF-8 sequence.
    // A malformed `token_sha256_hex` is untrusted input to this function (it
    // comes from `attached.json`, which this process wrote, but hydration
    // also reads it back after a possible manual edit or a future format
    // change) — the length check alone does not rule out non-ASCII bytes
    // inside a 64-BYTE-but-not-64-ASCII-char string.
    if !hex.is_ascii() || hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Why [`AttachRegistry::commit`] refused. Kept separate from
/// [`HandshakeError`] (review M2 ①): a registry closed for shutdown is NOT a
/// credential problem, and must not be answered like one.
#[derive(Debug)]
pub enum CommitRefused {
    /// An ordinary handshake-level refusal — the caller writes the matching
    /// wire error frame (`crate::attach_listener`).
    Handshake(HandshakeError),
    /// The registry is shutting down (`revoke_all` already ran, or is
    /// running concurrently and won the lock first). The caller must NOT
    /// write an error frame for this: `auth_failed` tells a module "the
    /// credential is bad, stop retrying, re-pair" (§5.6's own table), which
    /// is false here — the token is fine, the daemon is just going away. The
    /// correct wire behaviour is silence: close the connection with no frame
    /// at all, so the module sees a plain EOF and backs off/retries per its
    /// OTHER §5.6 row (`connect` failure / EOF), exactly as it should when
    /// it reconnects to the daemon that comes back up.
    Closed,
}

impl AttachRegistry {
    #[must_use]
    pub fn new(deps: AttachDeps) -> Self {
        Self {
            inner: Mutex::new(RegistryState {
                entries: HashMap::new(),
                deps: Some(deps),
            }),
        }
    }

    /// Hydrate from `attached.json` at daemon startup (§5.6: "daemon
    /// 重启：注册记录从 `attached.json` 读回...所有模块回到 `Detached`").
    /// Must run BEFORE the attach listener takes its first connection —
    /// otherwise a handshake could race a name that has not been loaded yet
    /// and see a spurious `auth_failed`. Per-record best effort is
    /// `crate::attached::load_all`'s job (review M2 ②); this only handles the
    /// hex-decode failure `load_all` cannot see (it is a
    /// `crate::attach_registry`-local representation).
    pub fn hydrate(&self, path: &std::path::Path) -> Result<(), String> {
        let stored = crate::attached::load_all(path)?;
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let RegistryState { entries, deps } = &mut *state;
        let Some(deps) = deps.as_ref() else {
            // Hydration only ever runs once, at startup, strictly before
            // `revoke_all` can — this is unreachable in practice, and a loud
            // log beats a silent no-op if it somehow is.
            tracing::error!("attach registry hydrate() called after the registry was closed");
            return Ok(());
        };
        for (name, entry) in stored {
            let Some(token_sha256) = decode_hex32(&entry.token_sha256_hex) else {
                tracing::error!(
                    "attached.json record {name:?} has a malformed token_sha256; skipping \
                     hydration for it (it will not be reachable until re-registered)"
                );
                continue;
            };
            let grant = build_grant(&name, &entry.manifest, deps);
            let host_commands = entry.manifest.host_commands().to_vec();
            entries.insert(
                name,
                Entry {
                    manifest_digest: entry.manifest_digest,
                    token_sha256,
                    token_id: entry.token_id,
                    disabled: entry.disabled,
                    slot: AttachSlot::new(),
                    grant: Arc::new(grant),
                    host_commands,
                    last_generation: None,
                    live_calls: None,
                },
            );
        }
        Ok(())
    }

    /// Review (Codex A3 follow-up, §5.5): has [`Self::revoke_all`] run (or
    /// started running — the two share the same lock, so this can never
    /// observe a half-applied close)? `crate::attach_listener` checks this
    /// BEFORE calling [`agent24_os_proto::initialize::accept_attached`] at
    /// all: once this is `true`, [`Self::expectation`] returns `None` for
    /// EVERY name (its backing map was drained), which — fed through
    /// `accept_attached`'s own lookup-miss handling — would otherwise be
    /// indistinguishable from "no such module" and get answered
    /// `auth_failed`. That is the wrong answer for a legitimately registered
    /// module caught by a shutdown mid-handshake: `auth_failed` tells it the
    /// credential is bad, so it stops reconnecting and demands a fresh
    /// pairing, when the token was fine all along and it only needed to
    /// retry once the daemon comes back. See
    /// `agent24_os_proto::initialize::HandshakeError::ShuttingDown`'s own doc
    /// for the answer `attach_listener` sends instead.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        let state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        state.deps.is_none()
    }

    /// The pure-check `lookup` callback for
    /// [`agent24_os_proto::initialize::accept_attached`] (§4.3 ①) — a quick
    /// lock-and-clone, released before any token comparison. Returns `Some`
    /// for ANY registered name, disabled or not: whether it is disabled is a
    /// [`Self::commit`]-time question (§4.3's own note on why `Busy`/
    /// `Forbidden` are produced by the locked step, not the pure one).
    ///
    /// Returns `None` for every name once [`Self::revoke_all`] has run (its
    /// backing map is drained) — a caller who needs to tell that apart from
    /// "no such module" must check [`Self::is_closed`] FIRST, before relying
    /// on this at all; see that method's own doc.
    #[must_use]
    pub fn expectation(&self, name: &str) -> Option<AttachedExpectation> {
        let state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        state.entries.get(name).map(|e| AttachedExpectation {
            manifest_digest: e.manifest_digest.clone(),
            token_sha256: e.token_sha256,
            token_id: e.token_id.clone(),
            kernel_versions: agent24_os_proto::version::kernel_range(),
            offer: e.grant.offer.clone(),
        })
    }

    /// §4.3 ②, under the one lock: re-confirm the record `claim` was checked
    /// against is still current (by `token_id` — catches a rotation/revoke
    /// that landed between the pure check and this call), not disabled, then
    /// install a fresh generation. Every failure here closes the connection
    /// (same contract as [`agent24_os_proto::initialize::accept`]) — see
    /// [`CommitRefused::Closed`]'s own doc for the one case that closes
    /// WITHOUT a wire error frame.
    ///
    /// # Errors
    ///
    /// See [`CommitRefused`].
    pub fn commit(
        &self,
        claim: &AttachedAccepted,
    ) -> Result<(u64, Arc<Generation>, Methods), CommitRefused> {
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if state.deps.is_none() {
            return Err(CommitRefused::Closed);
        }
        let entry = state
            .entries
            .get_mut(&claim.module)
            .ok_or(CommitRefused::Handshake(HandshakeError::AuthFailed))?;
        if entry.token_id != claim.token_id {
            return Err(CommitRefused::Handshake(HandshakeError::AuthFailed));
        }
        if entry.disabled {
            return Err(CommitRefused::Handshake(HandshakeError::Forbidden));
        }
        let (number, generation) = entry
            .slot
            .install()
            .map_err(|_slot_busy| CommitRefused::Handshake(HandshakeError::Busy))?;
        entry.last_generation = Some((number, generation.clone()));
        let methods = (entry.grant.methods_for)(&generation);
        Ok((number, generation, methods))
    }

    /// Called by the connection task once its OWN generation has ended (EOF,
    /// a write failure, or `stop` firing — see `crate::attach_listener`).
    /// §5.3's "断连立即撤销": the ended generation must be revoked, but ONLY
    /// if it is still this entry's live one — a stale call from a connection
    /// already superseded by a rotation/re-registration (which revoked and
    /// replaced it under this same lock) must not touch the NEWER
    /// generation now installed. `last_generation` (compared by `Arc`
    /// identity, the same rule `AttachSlot::release` itself documents) is
    /// what tells the two apart; revoking twice is safe either way
    /// (`Generation::revoke` is idempotent), so this is correct whether or
    /// not `AttachSlot::release` itself already revokes (A3-1 is finalizing
    /// that as this lands — see this file's own top-of-module note).
    pub fn release(&self, name: &str, generation: &Arc<Generation>) {
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = state.entries.get_mut(name) {
            if entry
                .last_generation
                .as_ref()
                .is_some_and(|(_, live)| Arc::ptr_eq(live, generation))
            {
                entry.slot.revoke();
            }
            // A3-3 (review M2): a `live_calls` handle is only ever meaningful
            // for the generation it was installed against
            // (`attach_kernel_calls` checks the same identity going in) —
            // clear it here on the SAME identity check, and `close()` it
            // FIRST, still under this lock, so a `CommandSlot` reserved just
            // before this call (holding a clone of the same `KernelCalls`)
            // can never enqueue a frame after this point — see
            // `KernelCalls::close`'s own doc.
            if let Some(live) = &entry.live_calls
                && Arc::ptr_eq(&live.generation, generation)
            {
                live.calls.close();
                entry.live_calls = None;
            }
            entry.slot.release(generation);
        }
    }

    /// A3-3 (review M2): install this connection's [`KernelCalls`] handle
    /// once `crate::attach_listener` has one (strictly after [`Self::commit`]
    /// returns — `serve_attached` is what MANUFACTURES a `KernelCalls`, and
    /// it needs the `Methods` `commit` just built, so the two cannot be one
    /// step). Guarded by the same `Arc::ptr_eq(&last_generation.1,
    /// generation)` identity check [`Self::release`] uses: if a
    /// rotation/revoke/disable landed on this name between `commit`
    /// returning and this call (replacing or clearing `last_generation`
    /// under this same lock), this is a no-op — the caller's connection is
    /// already being torn down by whatever won that race, and installing a
    /// calls handle for a generation nothing points at any more would only
    /// leak it (or, worse, clobber a NEWER connection's already-installed
    /// handle) until the connection's own `release` call arrives and finds
    /// no match to clear either.
    pub fn attach_kernel_calls(
        &self,
        name: &str,
        generation: &Arc<Generation>,
        calls: KernelCalls,
    ) {
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = state.entries.get_mut(name)
            && entry
                .last_generation
                .as_ref()
                .is_some_and(|(_, live)| Arc::ptr_eq(live, generation))
        {
            entry.live_calls = Some(LiveCalls {
                generation: Arc::clone(generation),
                calls,
                in_flight: Arc::new(AtomicUsize::new(0)),
            });
        }
    }

    /// A3-3 (§6.1 steps ①/②): does `name` exist at all, and if so, does its
    /// manifest declare `command`? This is a cheap, FAIL-FAST pre-check the
    /// REST handler runs before it reads/validates the request body — see
    /// [`CommandLookupError`]'s own doc for why [`Self::reserve_ready`], not
    /// this method, is the one that actually enforces the declaration.
    ///
    /// # Errors
    ///
    /// See [`CommandLookupError`].
    pub fn declared_command(&self, name: &str, command: &str) -> Result<(), CommandLookupError> {
        let state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = state
            .entries
            .get(name)
            .ok_or(CommandLookupError::UnknownModule)?;
        if entry.host_commands.iter().any(|c| c == command) {
            Ok(())
        } else {
            Err(CommandLookupError::CommandNotDeclared)
        }
    }

    /// A3-3 (§6.1 steps ②/④/the in-flight cap; review M1): reserve one slot
    /// to actually SEND `command` on `name`'s current connection. THREE
    /// things are checked/mutated under this ONE lock acquisition — the
    /// declaration is still current, there IS a live connection, and the
    /// in-flight cap is not yet reached — so a rotation, a disable, or a
    /// concurrent reservation cannot land in the middle and make this
    /// caller's decision stale by the time it acts on it. In particular
    /// (review M1's own scenario): a command that passed
    /// [`Self::declared_command`] against the manifest in effect at request
    /// time, but whose module has since been ROTATED to a manifest that no
    /// longer declares it, is refused HERE — the two lookups are the same
    /// `entry.host_commands`, but only this one runs at the moment the
    /// command would actually be sent.
    ///
    /// The returned [`CommandSlot`] releases the in-flight reservation when
    /// dropped, whatever the caller does with it.
    ///
    /// # Errors
    ///
    /// See [`CommandRefusal`].
    pub fn reserve_ready(&self, name: &str, command: &str) -> Result<CommandSlot, CommandRefusal> {
        let state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        // An entry that vanished entirely (e.g. `Revoked` since the
        // pre-check) has, by definition, no declared commands any more
        // either — same refusal as "no longer declared".
        let entry = state.entries.get(name).ok_or(CommandRefusal::NotDeclared)?;
        if !entry.host_commands.iter().any(|c| c == command) {
            return Err(CommandRefusal::NotDeclared);
        }
        let live = entry
            .live_calls
            .as_ref()
            .filter(|live| live.generation.state() == DrainState::Running)
            .ok_or(CommandRefusal::NotReady)?;
        if live.in_flight.load(Ordering::Relaxed) >= MAX_KERNEL_CALLS_IN_FLIGHT {
            return Err(CommandRefusal::Busy);
        }
        live.in_flight.fetch_add(1, Ordering::Relaxed);
        Ok(CommandSlot {
            calls: live.calls.clone(),
            in_flight: Arc::clone(&live.in_flight),
        })
    }

    /// §5.5 (M2), called from the `stopping` task BEFORE `stop_usage_writer`:
    /// revoke every live generation (in-flight `model/complete` calls are
    /// cancelled by the same `modules_cut_off` cancellation tree a mounted
    /// package's calls are, since `AttachDeps.models` is the SAME
    /// `ModelCallbackDeps` clone), DROP every entry outright — not merely
    /// revoke its slot, since an entry's `grant` carries its OWN
    /// `ModelCallbackDeps` clone via `ModelGrant` — and (review M1) take the
    /// REGISTRY's own `deps` too, so a registry that never had any attached
    /// modules at all still drops its `ModelCallbackDeps` clone here rather
    /// than whenever the `Arc<AttachRegistry>` itself finally does (see
    /// [`RegistryState`]'s own doc for why that distinction is the whole
    /// point of this review round).
    pub fn revoke_all(&self) {
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        state.deps = None;
        for (_name, mut entry) in state.entries.drain() {
            entry.slot.revoke();
            // A3-3 (review M2): close BEFORE the entry (and its `live_calls`)
            // is dropped below, same reasoning as `release` — a `CommandSlot`
            // some caller is still holding a clone of must see `closed` the
            // instant shutdown decided this generation is gone, not merely
            // whenever the wire itself later notices.
            if let Some(live) = entry.live_calls.take() {
                live.calls.close();
            }
            // `entry` (and its `Arc<Grant>`) is dropped here.
        }
    }

    /// `GET /api/v1/attached`'s `attach_status`/`generation` for one name
    /// (§5.1: `detached | attached | disabled`). `("detached", None)` for a
    /// name this registry has never heard of — should not happen for a name
    /// that came from the same `attached.json` this registry was hydrated
    /// from, but a caller must not panic on a hydration race, so this is a
    /// plain default rather than an `Option`/`panic`.
    #[must_use]
    pub fn status_of(&self, name: &str) -> (&'static str, Option<u64>) {
        let state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = state.entries.get(name) else {
            return ("detached", None);
        };
        if entry.disabled {
            return ("disabled", None);
        }
        match &entry.last_generation {
            Some((number, generation)) if generation.state() == DrainState::Running => {
                ("attached", Some(*number))
            }
            _ => ("detached", None),
        }
    }

    /// The `on_commit` hook for [`crate::attached::register`]/`revoke`/
    /// `set_disabled` — see [`Change`]'s own doc comment for why this runs
    /// inside THEIR file-lock critical section rather than being called
    /// after it returns.
    ///
    /// Review M2 ①: a no-op once the registry is closed (`revoke_all` already
    /// ran, or wins a race against this call for the lock) — the daemon is
    /// exiting, no listener is left accepting handshakes for `entries` to
    /// matter to, and building a grant from `deps` would need a `deps` that
    /// no longer exists.
    pub fn on_change(&self, change: &Change<'_>) {
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let RegistryState { entries, deps } = &mut *state;
        let Some(deps) = deps.as_ref() else {
            return;
        };
        match *change {
            Change::Registered {
                name,
                rotated,
                manifest,
                manifest_digest,
                token_sha256_hex,
                token_id,
                disabled,
            } => {
                let Some(token_sha256) = decode_hex32(token_sha256_hex) else {
                    tracing::error!(
                        "on_change({name:?}): malformed token_sha256_hex from the store — this \
                         is a bug in `crate::attached`, not a user-supplied value"
                    );
                    return;
                };
                tracing::debug!(
                    module = name,
                    rotated,
                    "attach registry: applying a registration change"
                );
                match entries.entry(name.to_owned()) {
                    MapEntry::Occupied(mut o) => {
                        let entry = o.get_mut();
                        // §3.4: any register on an existing name — token-only
                        // rotation OR a full re-registration — revokes
                        // whatever generation is currently live; the old
                        // token no longer matches (`token_id` below), so a
                        // reconnect attempt with it gets `auth_failed`
                        // regardless, but the CONNECTION itself must not be
                        // left running on stale grants.
                        entry.slot.revoke();
                        // A3-3 (review M2): the connection this entry's
                        // `live_calls` (if any) belonged to is being revoked
                        // above — `close()` it FIRST, still under this lock,
                        // so a `CommandSlot` reserved just before this
                        // rotation landed can never enqueue a frame after
                        // this point (see `KernelCalls::close`'s own doc).
                        if let Some(live) = entry.live_calls.take() {
                            live.calls.close();
                        }
                        // §3.4: digest unchanged -> token-only rotation,
                        // REUSE the grant (rate limiter, model health table,
                        // event sink). Digest changed -> rebuild everything.
                        if entry.manifest_digest != manifest_digest {
                            entry.grant = Arc::new(build_grant(name, manifest, deps));
                        }
                        entry.manifest_digest = manifest_digest.to_owned();
                        entry.token_sha256 = token_sha256;
                        entry.token_id = token_id.to_owned();
                        entry.host_commands = manifest.host_commands().to_vec();
                        // Review M4: mirrors what `crate::attached::register`
                        // just wrote to disk — a rotation PRESERVES
                        // `disabled`, it does not clear it (an AgentEar
                        // auto-update re-running `attach add` must not
                        // silently undo a user's `PATCH ... {"enabled":false}`).
                        entry.disabled = disabled;
                    }
                    MapEntry::Vacant(v) => {
                        v.insert(Entry {
                            manifest_digest: manifest_digest.to_owned(),
                            token_sha256,
                            token_id: token_id.to_owned(),
                            disabled,
                            slot: AttachSlot::new(),
                            grant: Arc::new(build_grant(name, manifest, deps)),
                            host_commands: manifest.host_commands().to_vec(),
                            last_generation: None,
                            live_calls: None,
                        });
                    }
                }
            }
            Change::Revoked { name } => {
                if let Some(mut entry) = entries.remove(name) {
                    entry.slot.revoke();
                    // A3-3 (review M2): close before the removed entry (and
                    // its `live_calls`) is dropped at the end of this arm.
                    if let Some(live) = entry.live_calls.take() {
                        live.calls.close();
                    }
                }
            }
            Change::Disabled { name, disabled } => {
                if let Some(entry) = entries.get_mut(name) {
                    entry.disabled = disabled;
                    if disabled {
                        entry.slot.revoke();
                        // A3-3 (review M2): same close-before-clear as the
                        // rotation arm above.
                        if let Some(live) = entry.live_calls.take() {
                            live.calls.close();
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A `RunTrigger` that never fires — this test module never advances a
    /// schedule, it only needs a `Scheduler` to exist so `AttachDeps` can be
    /// built without wiring a real one. Same shape as `crate::domain::tests::NoopTrigger`.
    struct NoopTrigger;

    #[async_trait::async_trait]
    impl agent24_scheduler::RunTrigger for NoopTrigger {
        async fn trigger(
            &self,
            _invocation: &agent24_scheduler::ScheduleInvocation,
        ) -> agent24_scheduler::FireOutcome {
            agent24_scheduler::FireOutcome::Deferred {
                reason: agent24_scheduler::DeferReason::MountPending,
            }
        }
    }

    async fn deps() -> AttachDeps {
        let store = agent24_store::Store::open_memory().await.unwrap();
        AttachDeps {
            scheduler: agent24_scheduler::Scheduler::new(
                store.clone(),
                Arc::new(NoopTrigger),
                Arc::new(|_| {}),
            ),
            models: None,
            approval_broker: crate::module_approval_broker::ModuleApprovalBroker::new(
                store,
                crate::events::EventsHub::default(),
            ),
            events: crate::events::EventsHub::default(),
        }
    }

    fn manifest(name: &str, caps: &[&str]) -> agent24_domain::DomainOsManifest {
        agent24_domain::DomainOsManifest::from_yaml(&format!(
            "name: {name}\nversion: \"1\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             impl_kind: attached_process\nkernel_capabilities: [{}]\n",
            caps.join(", ")
        ))
        .unwrap()
    }

    fn register(registry: &AttachRegistry, name: &str, caps: &[&str]) -> (String, String) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let m = manifest(name, caps);
        let digest = "sha256:deadbeef".to_owned();
        let token_sha256_hex = "ab".repeat(32);
        // A fresh token_id every call, like the real `crate::attached::register`
        // (`mint_token_id`) — a test helper that reused the same id across
        // calls would make "stale token_id" tests pass for the wrong reason.
        let token_id = format!("tok_{name}_{n}");
        registry.on_change(&Change::Registered {
            name,
            rotated: false,
            manifest: &m,
            manifest_digest: &digest,
            token_sha256_hex: &token_sha256_hex,
            token_id: &token_id,
            disabled: false,
        });
        (digest, token_id)
    }

    /// Unwraps the ordinary handshake-refusal half of [`CommitRefused`] —
    /// every `commit()` test in this module expects that half; the `Closed`
    /// half has its own dedicated test.
    fn commit_err<T>(r: Result<T, CommitRefused>) -> HandshakeError {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(CommitRefused::Handshake(e)) => e,
            Err(CommitRefused::Closed) => panic!("expected a Handshake refusal, got Closed"),
        }
    }

    fn accepted(module: &str, token_id: &str) -> AttachedAccepted {
        AttachedAccepted {
            module: module.to_owned(),
            token_id: token_id.to_owned(),
            accepted: agent24_os_proto::initialize::Accepted {
                id: "1".to_owned(),
                result: agent24_os_proto::initialize::InitializeResult {
                    protocol_version: 1,
                    offer: Offer::none(),
                },
            },
        }
    }

    #[tokio::test]
    async fn expectation_is_none_for_an_unregistered_name() {
        let registry = AttachRegistry::new(deps().await);
        assert!(registry.expectation("nobody").is_none());
    }

    #[tokio::test]
    async fn a_registered_name_yields_an_expectation_and_commits() {
        let registry = AttachRegistry::new(deps().await);
        let (digest, token_id) = register(&registry, "agentear", &["events"]);
        let exp = registry.expectation("agentear").expect("registered");
        assert_eq!(exp.manifest_digest, digest);
        assert_eq!(exp.token_id, token_id);
        assert_eq!(exp.offer.provides, vec!["_a24/events/".to_owned()]);

        let claim = accepted("agentear", &token_id);
        let (number, generation, _methods) = registry.commit(&claim).unwrap();
        assert_eq!(number, 1);
        assert_eq!(generation.upstream(), None);
        assert_eq!(registry.status_of("agentear"), ("attached", Some(1)));
    }

    #[tokio::test]
    async fn a_stale_token_id_at_commit_is_auth_failed() {
        // Simulates a rotation landing between the pure check and commit.
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register(&registry, "agentear", &["events"]);
        register(&registry, "agentear", &["events"]); // rotates, mints a new token_id
        let stale_claim = accepted("agentear", &token_id);
        assert_eq!(
            commit_err(registry.commit(&stale_claim)),
            HandshakeError::AuthFailed
        );
    }

    #[tokio::test]
    async fn a_second_commit_while_one_is_live_is_busy() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register(&registry, "agentear", &["events"]);
        let claim = accepted("agentear", &token_id);
        registry.commit(&claim).unwrap();
        assert_eq!(commit_err(registry.commit(&claim)), HandshakeError::Busy);
    }

    #[tokio::test]
    async fn disabled_refuses_commit_with_forbidden() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register(&registry, "agentear", &["events"]);
        registry.on_change(&Change::Disabled {
            name: "agentear",
            disabled: true,
        });
        let claim = accepted("agentear", &token_id);
        assert_eq!(
            commit_err(registry.commit(&claim)),
            HandshakeError::Forbidden
        );
        assert_eq!(registry.status_of("agentear"), ("disabled", None));
    }

    #[tokio::test]
    async fn disabling_a_live_generation_revokes_it() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register(&registry, "agentear", &["events"]);
        let claim = accepted("agentear", &token_id);
        let (_, generation, _) = registry.commit(&claim).unwrap();
        registry.on_change(&Change::Disabled {
            name: "agentear",
            disabled: true,
        });
        assert_eq!(generation.state(), DrainState::Revoked);
    }

    #[tokio::test]
    async fn revoking_the_record_revokes_the_live_generation_and_forgets_the_name() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register(&registry, "agentear", &["events"]);
        let claim = accepted("agentear", &token_id);
        let (_, generation, _) = registry.commit(&claim).unwrap();
        registry.on_change(&Change::Revoked { name: "agentear" });
        assert_eq!(generation.state(), DrainState::Revoked);
        assert!(registry.expectation("agentear").is_none());
        assert_eq!(registry.status_of("agentear"), ("detached", None));
    }

    #[tokio::test]
    async fn release_after_eof_clears_the_slot_so_a_reconnect_can_install() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register(&registry, "agentear", &["events"]);
        let claim = accepted("agentear", &token_id);
        let (_, generation, _) = registry.commit(&claim).unwrap();
        registry.release("agentear", &generation);
        assert_eq!(generation.state(), DrainState::Revoked);
        // A fresh connection (same token_id — no rotation happened) can
        // install a new generation immediately.
        let (number, _generation2, _) = registry.commit(&claim).unwrap();
        assert_eq!(number, 2);
    }

    #[tokio::test]
    async fn rotation_reusing_the_digest_keeps_the_same_grant() {
        let registry = AttachRegistry::new(deps().await);
        register(&registry, "agentear", &["events"]);
        let grant_before = {
            let map = registry.inner.lock().unwrap();
            Arc::clone(&map.entries.get("agentear").unwrap().grant)
        };
        // Re-register with the SAME digest (token-only rotation).
        register(&registry, "agentear", &["events"]);
        let grant_after = {
            let map = registry.inner.lock().unwrap();
            Arc::clone(&map.entries.get("agentear").unwrap().grant)
        };
        assert!(
            Arc::ptr_eq(&grant_before, &grant_after),
            "a token-only rotation (same digest) must reuse the same grant instance"
        );
    }

    #[tokio::test]
    async fn revoke_all_revokes_every_live_generation_and_forgets_every_grant() {
        let registry = AttachRegistry::new(deps().await);
        let (_, a) = register(&registry, "agentear", &["events"]);
        let (_, b) = register(&registry, "other", &["events"]);
        let ga = registry.commit(&accepted("agentear", &a)).unwrap().1;
        let gb = registry.commit(&accepted("other", &b)).unwrap().1;
        registry.revoke_all();
        assert_eq!(ga.state(), DrainState::Revoked);
        assert_eq!(gb.state(), DrainState::Revoked);
        assert!(registry.expectation("agentear").is_none());
        assert!(registry.expectation("other").is_none());
    }

    /// Review M1: `revoke_all` must drop the REGISTRY's own `deps` (its
    /// `ModelCallbackDeps` clone with it) even when NO module was ever
    /// attached — the exact scenario that used to leave `deps` alive for the
    /// rest of the process's life (see `RegistryState`'s own doc). Proven
    /// here structurally, by checking `deps` is gone (`commit` afterwards
    /// answers `Closed`, which is only possible with `deps: None`); the
    /// end-to-end "the shutdown log no longer warns" proof lives in the
    /// blackbox suite (`c9`-adjacent).
    #[tokio::test]
    async fn revoke_all_drops_deps_even_with_no_attached_modules_ever() {
        let registry = AttachRegistry::new(deps().await);
        registry.revoke_all();
        let claim = accepted("nobody-was-ever-attached", "tok");
        assert!(matches!(
            registry.commit(&claim),
            Err(CommitRefused::Closed)
        ));
    }

    /// Review (Codex A3 follow-up, §5.5): [`AttachRegistry::is_closed`] is
    /// what `crate::attach_listener` checks BEFORE even calling
    /// `accept_attached` — see that method's own doc for why `expectation`
    /// alone (which returns `None` for every name once `revoke_all` has
    /// drained the map) cannot be relied on to tell "closed" apart from "no
    /// such module".
    #[tokio::test]
    async fn is_closed_starts_false_and_flips_permanently_on_revoke_all() {
        let registry = AttachRegistry::new(deps().await);
        assert!(!registry.is_closed());
        register(&registry, "agentear", &["events"]);
        assert!(
            !registry.is_closed(),
            "registering a module must not, by itself, close the registry"
        );
        registry.revoke_all();
        assert!(registry.is_closed());
        // Idempotent, like `revoke_all` itself.
        registry.revoke_all();
        assert!(registry.is_closed());
    }

    /// Review M2 ①: a handshake commit racing (or landing after) `revoke_all`
    /// gets `Closed`, NEVER `Handshake(AuthFailed)` — the whole point being
    /// that `crate::attach_listener` must not write an `auth_failed` wire
    /// frame for a perfectly good token just because the daemon happened to
    /// be shutting down at that instant (that would tell a real AgentEar to
    /// stop reconnecting permanently, per its own §5.6 table).
    #[tokio::test]
    async fn commit_after_revoke_all_is_closed_not_auth_failed() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register(&registry, "agentear", &["events"]);
        registry.revoke_all();
        let claim = accepted("agentear", &token_id);
        assert!(matches!(
            registry.commit(&claim),
            Err(CommitRefused::Closed)
        ));
    }

    /// Review M2 ①, the race window itself: a `commit` that starts BEFORE
    /// `revoke_all` grabs the lock still completes normally (closing is not
    /// retroactive to an in-flight commit that already holds the lock) —
    /// this is really just restating that `commit`'s closed-check happens
    /// once, at the top, under the one lock; included as a named regression
    /// rather than relying on that being implied by the two tests above.
    #[tokio::test]
    async fn a_commit_that_already_holds_the_lock_is_unaffected_by_a_concurrent_revoke_all() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register(&registry, "agentear", &["events"]);
        let claim = accepted("agentear", &token_id);
        // `commit` and `revoke_all` both take the SAME lock, so there is no
        // real interleaving to race here in a single-threaded test — this
        // documents the ordering guarantee (whichever gets the lock first
        // wins outright) rather than exercising a true data race.
        let installed = registry.commit(&claim);
        assert!(installed.is_ok());
        registry.revoke_all();
    }

    /// Review M4: a token-only rotation (same digest) PRESERVES `disabled`
    /// — the exact bug this review caught was `on_change` hardcoding
    /// `entry.disabled = false` on every register/rotate, which would let an
    /// AgentEar auto-update silently undo a user's `PATCH .../{"enabled":
    /// false}`.
    #[tokio::test]
    async fn rotation_preserves_disabled_it_does_not_clear_it() {
        let registry = AttachRegistry::new(deps().await);
        register(&registry, "agentear", &["events"]);
        registry.on_change(&Change::Disabled {
            name: "agentear",
            disabled: true,
        });
        assert_eq!(registry.status_of("agentear"), ("disabled", None));

        // A rotation (`crate::attached::register`'s own behaviour: it reads
        // back `previous_disabled` and passes it through) must NOT clear it.
        let m = manifest("agentear", &["events"]);
        registry.on_change(&Change::Registered {
            name: "agentear",
            rotated: true,
            manifest: &m,
            manifest_digest: "sha256:deadbeef", // same digest: token-only rotation
            token_sha256_hex: &"cd".repeat(32),
            token_id: "tok_rotated",
            disabled: true, // what `crate::attached::register` would pass through
        });
        assert_eq!(
            registry.status_of("agentear"),
            ("disabled", None),
            "a rotation must preserve `disabled`, not silently re-enable the module"
        );

        // An explicit re-enable (what `PATCH .../{"enabled":true}` drives)
        // still works.
        registry.on_change(&Change::Disabled {
            name: "agentear",
            disabled: false,
        });
        assert_eq!(registry.status_of("agentear"), ("detached", None));
    }

    // ───────────────────────── A3-3: reverse commands ─────────────────────────

    fn manifest_with_commands(
        name: &str,
        caps: &[&str],
        commands: &[&str],
    ) -> agent24_domain::DomainOsManifest {
        agent24_domain::DomainOsManifest::from_yaml(&format!(
            "name: {name}\nversion: \"1\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             impl_kind: attached_process\nkernel_capabilities: [{}]\nhost_commands: [{}]\n",
            caps.join(", "),
            commands.join(", ")
        ))
        .unwrap()
    }

    fn register_with_commands(
        registry: &AttachRegistry,
        name: &str,
        caps: &[&str],
        commands: &[&str],
    ) -> (String, String) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let m = manifest_with_commands(name, caps, commands);
        let digest = "sha256:deadbeef".to_owned();
        let token_sha256_hex = "ab".repeat(32);
        let token_id = format!("tok_{name}_{n}");
        registry.on_change(&Change::Registered {
            name,
            rotated: false,
            manifest: &m,
            manifest_digest: &digest,
            token_sha256_hex: &token_sha256_hex,
            token_id: &token_id,
            disabled: false,
        });
        (digest, token_id)
    }

    /// A genuine [`KernelCalls`] handle, built the same way
    /// `crate::attach_listener::handle_connection` builds one — just enough
    /// wire underneath it for these registry-level tests, which exercise
    /// [`AttachRegistry::reserve_ready`]'s bookkeeping (readiness, the
    /// in-flight cap, stale-generation rejection, M2's `close()`) rather than
    /// the wire itself (that is `agent24_os_proto::attach_mux`'s own test
    /// suite, and the real round trip is covered end-to-end by this crate's
    /// `tests/a3_3_host_commands_blackbox.rs`). The module side of the duplex
    /// is returned too so a test can assert nothing ever arrived on it.
    fn spawn_real_kernel_calls_with_module_side(
        methods: Methods,
    ) -> (KernelCalls, tokio::io::ReadHalf<tokio::io::DuplexStream>) {
        let (kernel_side, module_side) = tokio::io::duplex(64 * 1024);
        let (kernel_read, kernel_write) = tokio::io::split(kernel_side);
        let (calls, fut) = agent24_os_proto::attach_mux::serve_attached(
            tokio::io::BufReader::new(kernel_read),
            kernel_write,
            methods,
            agent24_os_proto::rpc::Limits::default(),
            std::future::pending(),
        );
        tokio::spawn(fut);
        let (module_read, _module_write) = tokio::io::split(module_side);
        (calls, module_read)
    }

    fn spawn_real_kernel_calls(methods: Methods) -> KernelCalls {
        spawn_real_kernel_calls_with_module_side(methods).0
    }

    #[tokio::test]
    async fn declared_command_checks_existence_then_declaration() {
        let registry = AttachRegistry::new(deps().await);
        assert_eq!(
            registry.declared_command("nobody", "speak").unwrap_err(),
            CommandLookupError::UnknownModule
        );
        register_with_commands(
            &registry,
            "agentear",
            &["events"],
            &["speak", "stop_playback"],
        );
        assert_eq!(
            registry.declared_command("agentear", "record").unwrap_err(),
            CommandLookupError::CommandNotDeclared
        );
        assert!(registry.declared_command("agentear", "speak").is_ok());
        assert!(
            registry
                .declared_command("agentear", "stop_playback")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn reserve_ready_is_not_ready_before_a_connection_and_after_it_ends() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register_with_commands(&registry, "agentear", &["events"], &["speak"]);
        assert_eq!(
            registry.reserve_ready("agentear", "speak").unwrap_err(),
            CommandRefusal::NotReady,
            "no handshake has ever committed yet"
        );

        let claim = accepted("agentear", &token_id);
        let (_, generation, methods) = registry.commit(&claim).unwrap();
        assert_eq!(
            registry.reserve_ready("agentear", "speak").unwrap_err(),
            CommandRefusal::NotReady,
            "commit() alone, before attach_kernel_calls, must not be usable"
        );

        registry.attach_kernel_calls("agentear", &generation, spawn_real_kernel_calls(methods));
        let slot = registry
            .reserve_ready("agentear", "speak")
            .expect("now ready");
        drop(slot);

        registry.release("agentear", &generation);
        assert_eq!(
            registry.reserve_ready("agentear", "speak").unwrap_err(),
            CommandRefusal::NotReady,
            "after release() the calls handle must be gone, not merely idle"
        );
    }

    #[tokio::test]
    async fn reserve_ready_enforces_the_in_flight_cap_and_releases_on_drop() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register_with_commands(&registry, "agentear", &["events"], &["speak"]);
        let claim = accepted("agentear", &token_id);
        let (_, generation, methods) = registry.commit(&claim).unwrap();
        registry.attach_kernel_calls("agentear", &generation, spawn_real_kernel_calls(methods));

        let mut slots = Vec::new();
        for _ in 0..MAX_KERNEL_CALLS_IN_FLIGHT {
            slots.push(
                registry
                    .reserve_ready("agentear", "speak")
                    .expect("under the cap must succeed"),
            );
        }
        assert_eq!(
            registry.reserve_ready("agentear", "speak").unwrap_err(),
            CommandRefusal::Busy,
            "the {MAX_KERNEL_CALLS_IN_FLIGHT}th reservation must be refused with zero frames sent"
        );

        // Dropping ONE reservation frees exactly one slot back up.
        slots.pop();
        let one_more = registry.reserve_ready("agentear", "speak");
        assert!(one_more.is_ok(), "releasing one slot must free exactly one");
        assert_eq!(
            registry.reserve_ready("agentear", "speak").unwrap_err(),
            CommandRefusal::Busy,
            "the cap must be exactly {MAX_KERNEL_CALLS_IN_FLIGHT}, not one looser"
        );
    }

    /// Review-worthy race (`AttachRegistry::attach_kernel_calls`'s own doc):
    /// a rotation lands between a connection's `commit()` and the point
    /// where its task gets around to installing its `KernelCalls` handle.
    ///
    /// Merely asserting `reserve_ready` still says `NotReady` after the late
    /// install is a WEAK check here — `stale_generation` is itself `Revoked`
    /// by the rotation, so `reserve_ready`'s OWN `DrainState::Running` filter
    /// would mask a missing identity check too (a first version of this test
    /// made exactly that mistake and passed against a mutant with the guard
    /// deleted). The identity check earns its keep in a DIFFERENT failure
    /// mode: a late stale install landing AFTER the fresh connection has
    /// ALREADY installed its own (good, live) calls handle must not
    /// CLOBBER it — that is what this test pins, by installing the fresh
    /// handle FIRST and confirming it is still the one `reserve_ready` hands
    /// out afterwards.
    #[tokio::test]
    async fn a_late_stale_install_never_clobbers_an_already_installed_fresh_one() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register_with_commands(&registry, "agentear", &["events"], &["speak"]);
        let claim = accepted("agentear", &token_id);
        let (_, stale_generation, stale_methods) = registry.commit(&claim).unwrap();

        // Rotate: revokes `stale_generation`, mints a fresh token, and a
        // fresh handshake installs a DIFFERENT generation as
        // `last_generation` — exactly as if the stale connection's task
        // simply hasn't gotten around to calling `attach_kernel_calls` yet.
        let (_, new_token_id) =
            register_with_commands(&registry, "agentear", &["events"], &["speak"]);
        let fresh_claim = accepted("agentear", &new_token_id);
        let (_, fresh_generation, fresh_methods) = registry.commit(&fresh_claim).unwrap();
        assert!(!Arc::ptr_eq(&stale_generation, &fresh_generation));

        // The FRESH connection's task wins the race and installs first.
        registry.attach_kernel_calls(
            "agentear",
            &fresh_generation,
            spawn_real_kernel_calls(fresh_methods),
        );
        assert!(
            registry.reserve_ready("agentear", "speak").is_ok(),
            "the fresh generation's calls handle must be usable once installed"
        );

        // The STALE connection's task now (late, after losing the race)
        // tries to install ITS calls handle — must be a no-op: it must NOT
        // overwrite the fresh entry that is genuinely live and ready.
        registry.attach_kernel_calls(
            "agentear",
            &stale_generation,
            spawn_real_kernel_calls(stale_methods),
        );
        assert!(
            registry.reserve_ready("agentear", "speak").is_ok(),
            "a late stale install must not clobber the already-installed fresh calls handle"
        );
    }

    /// Review M1: `declared_command` (the REST handler's cheap pre-check)
    /// passes against the manifest in effect at request time; the module is
    /// then ROTATED (still holding a live connection under the new manifest)
    /// to one that no longer declares the command; `reserve_ready` — which
    /// runs AFTER the pre-check, once the body has been read — must catch
    /// this and refuse `NotDeclared`, not fall through to sending a command
    /// the current manifest disowns.
    #[tokio::test]
    async fn reserve_ready_rejects_a_command_the_current_manifest_no_longer_declares() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register_with_commands(&registry, "agentear", &["events"], &["speak"]);

        // The REST handler's step ② pre-check, at request time: passes.
        assert!(registry.declared_command("agentear", "speak").is_ok());

        // A handshake commits and installs a live connection under the
        // ORIGINAL manifest (so `reserve_ready` has a live generation to
        // find — otherwise `NotReady` would mask the declaration check this
        // test is pinning, same pitfall the stale-install test's own doc
        // describes).
        let claim = accepted("agentear", &token_id);
        let (_, generation, methods) = registry.commit(&claim).unwrap();
        registry.attach_kernel_calls("agentear", &generation, spawn_real_kernel_calls(methods));
        assert!(
            registry.reserve_ready("agentear", "speak").is_ok(),
            "sanity: still declared and ready before the rotation"
        );

        // Rotate to a manifest that no longer declares "speak" — a REAL
        // registration change, same call `crate::attached::register` makes.
        let m = manifest_with_commands("agentear", &["events"], &["stop_playback"]);
        registry.on_change(&Change::Registered {
            name: "agentear",
            rotated: true,
            manifest: &m,
            manifest_digest: "sha256:deadbeef-v2",
            token_sha256_hex: &"ef".repeat(32),
            token_id: "tok_rotated_no_speak",
            disabled: false,
        });

        // The pre-check, run again NOW, already reflects the new manifest —
        // demonstrating this is a REAL rotation, not a stale read.
        assert_eq!(
            registry.declared_command("agentear", "speak").unwrap_err(),
            CommandLookupError::CommandNotDeclared
        );
        // The actual enforcement point: reserve_ready refuses it too, even
        // if a caller's `declared_command` pre-check had already run before
        // the rotation landed.
        assert_eq!(
            registry.reserve_ready("agentear", "speak").unwrap_err(),
            CommandRefusal::NotDeclared,
            "reserve_ready must re-check the declaration against the CURRENT manifest"
        );
        // The still-declared command works fine against the new connection
        // once one is established (not pinned further here — the rotation's
        // own revoke/reconnect story is covered elsewhere).
        assert_eq!(
            registry
                .reserve_ready("agentear", "stop_playback")
                .unwrap_err(),
            CommandRefusal::NotReady,
            "declared under the new manifest, but the OLD generation was revoked by the \
             rotation and no new connection has attached yet"
        );
    }

    /// Review M2: a `CommandSlot` reserved successfully, then the module is
    /// revoked (here: `PATCH .../{"enabled":false}`'s registry-side
    /// counterpart, `Change::Disabled`) BEFORE the reserved slot's `.call()`
    /// ever runs. The call must resolve `NotSent` — and, the part a weaker
    /// fix could still get wrong, the module's side of the wire must
    /// receive ZERO bytes: the revoke must win the race against the
    /// already-reserved slot actually enqueueing its frame.
    #[tokio::test]
    async fn revoking_after_reserve_prevents_the_frame_from_ever_being_sent() {
        use tokio::io::AsyncReadExt;

        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register_with_commands(&registry, "agentear", &["events"], &["speak"]);
        let claim = accepted("agentear", &token_id);
        let (_, generation, methods) = registry.commit(&claim).unwrap();
        let (calls, mut module_read) = spawn_real_kernel_calls_with_module_side(methods);
        registry.attach_kernel_calls("agentear", &generation, calls);

        let slot = registry
            .reserve_ready("agentear", "speak")
            .expect("reserve must succeed before the revoke below");

        // Revoke NOW — via disable, the same path `PATCH .../{"enabled":
        // false}` drives — strictly AFTER the reservation above, strictly
        // BEFORE the reserved slot ever calls `.call()`.
        registry.on_change(&Change::Disabled {
            name: "agentear",
            disabled: true,
        });

        let outcome = slot
            .calls()
            .call(
                agent24_os_proto::attach_mux::COMMAND_METHOD,
                serde_json::json!({"name": "speak", "body": {}}),
                std::time::Duration::from_secs(5),
            )
            .await;
        assert!(
            matches!(
                outcome,
                Err(agent24_os_proto::attach_mux::KernelCallFailed::NotSent)
            ),
            "expected NotSent, got {outcome:?}"
        );

        // The decisive part: nothing was ever written to the module's side
        // of the wire — the revoke's `close()` won the race BEFORE `call()`
        // could `try_send`, not merely after.
        let mut buf = [0u8; 16];
        let read = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            module_read.read(&mut buf),
        )
        .await;
        assert!(
            read.is_err(),
            "no bytes should ever have reached the module, got {read:?}"
        );
    }
}
