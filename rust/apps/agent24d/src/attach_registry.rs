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
use std::sync::{Arc, Mutex, PoisonError};

use agent24_domain::{Capability, DomainOsManifest, EventBroadcast, EventSink, Grants};
use agent24_os_proto::attach::AttachSlot;
use agent24_os_proto::drain::{DrainState, Generation};
use agent24_os_proto::initialize::{AttachedAccepted, AttachedExpectation, HandshakeError, Offer};
use agent24_os_proto::rpc::Methods;
use agent24_os_proto::supervisor::MethodsFor;

use crate::attached::Change;

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
    /// The most recent generation this slot ever installed — kept after it
    /// ends (unlike `AttachSlot`, which clears its own `current` on
    /// `release`/`revoke`) purely so `GET /api/v1/attached` can report a
    /// `generation` number. Read through [`Generation::state`] to tell "was
    /// live" from "is live": an attached generation never re-enters
    /// `Running` once it leaves it (§5.3: no drain, no resume), so
    /// `DrainState::Running` is exactly "this is the current connection".
    last_generation: Option<(u64, Arc<Generation>)>,
}

/// The live registry: one [`Entry`] per name in `attached.json`, guarded by
/// one lock — see the module doc for why that lock is never held across an
/// `await`.
pub struct AttachRegistry {
    inner: Mutex<HashMap<String, Entry>>,
    deps: AttachDeps,
}

fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl AttachRegistry {
    #[must_use]
    pub fn new(deps: AttachDeps) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            deps,
        }
    }

    /// Hydrate from `attached.json` at daemon startup (§5.6: "daemon
    /// 重启：注册记录从 `attached.json` 读回...所有模块回到 `Detached`").
    /// Must run BEFORE the attach listener takes its first connection —
    /// otherwise a handshake could race a name that has not been loaded yet
    /// and see a spurious `auth_failed`. Best-effort per name: one record
    /// whose manifest no longer parses is logged and skipped rather than
    /// failing every other module's hydration.
    pub fn hydrate(&self, path: &std::path::Path) -> Result<(), String> {
        let stored = crate::attached::load_all(path)?;
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        for (name, entry) in stored {
            let Some(token_sha256) = decode_hex32(&entry.token_sha256_hex) else {
                tracing::error!(
                    "attached.json record {name:?} has a malformed token_sha256; skipping \
                     hydration for it (it will not be reachable until re-registered)"
                );
                continue;
            };
            let grant = build_grant(&name, &entry.manifest, &self.deps);
            map.insert(
                name,
                Entry {
                    manifest_digest: entry.manifest_digest,
                    token_sha256,
                    token_id: entry.token_id,
                    disabled: entry.disabled,
                    slot: AttachSlot::new(),
                    grant: Arc::new(grant),
                    last_generation: None,
                },
            );
        }
        Ok(())
    }

    /// The pure-check `lookup` callback for
    /// [`agent24_os_proto::initialize::accept_attached`] (§4.3 ①) — a quick
    /// lock-and-clone, released before any token comparison. Returns `Some`
    /// for ANY registered name, disabled or not: whether it is disabled is a
    /// [`Self::commit`]-time question (§4.3's own note on why `Busy`/
    /// `Forbidden` are produced by the locked step, not the pure one).
    #[must_use]
    pub fn expectation(&self, name: &str) -> Option<AttachedExpectation> {
        let map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(name).map(|e| AttachedExpectation {
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
    /// (same contract as [`agent24_os_proto::initialize::accept`]).
    ///
    /// # Errors
    ///
    /// [`HandshakeError::AuthFailed`] if the record is gone or was rotated to
    /// a different `token_id` since the pure check; [`HandshakeError::Forbidden`]
    /// if disabled; [`HandshakeError::Busy`] if another generation is already
    /// live (§5.4 Q3=a: first comer keeps it).
    pub fn commit(
        &self,
        claim: &AttachedAccepted,
    ) -> Result<(u64, Arc<Generation>, Methods), HandshakeError> {
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = map
            .get_mut(&claim.module)
            .ok_or(HandshakeError::AuthFailed)?;
        if entry.token_id != claim.token_id {
            return Err(HandshakeError::AuthFailed);
        }
        if entry.disabled {
            return Err(HandshakeError::Forbidden);
        }
        let (number, generation) = entry
            .slot
            .install()
            .map_err(|_slot_busy| HandshakeError::Busy)?;
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
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = map.get_mut(name) {
            if entry
                .last_generation
                .as_ref()
                .is_some_and(|(_, live)| Arc::ptr_eq(live, generation))
            {
                entry.slot.revoke();
            }
            entry.slot.release(generation);
        }
    }

    /// §5.5 (M2), called from the `stopping` task BEFORE `stop_usage_writer`:
    /// revoke every live generation (in-flight `model/complete` calls are
    /// cancelled by the same `modules_cut_off` cancellation tree a mounted
    /// package's calls are, since `AttachDeps.models` is the SAME
    /// `ModelCallbackDeps` clone) and then DROP every entry outright — not
    /// merely revoke its slot. An entry's `grant` carries the one remaining
    /// clone of `ModelCallbackDeps` (via its `ModelGrant`, if any), and that
    /// clone's `usage` sender must actually be dropped here, not merely
    /// quiesced, for `stop_usage_writer`'s channel to close on schedule (the
    /// design's own reasoning for why this ordering matters).
    pub fn revoke_all(&self) {
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        for (_name, mut entry) in map.drain() {
            entry.slot.revoke();
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
        let map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = map.get(name) else {
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
    pub fn on_change(&self, change: &Change<'_>) {
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        match *change {
            Change::Registered {
                name,
                rotated,
                manifest,
                manifest_digest,
                token_sha256_hex,
                token_id,
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
                match map.entry(name.to_owned()) {
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
                        // §3.4: digest unchanged -> token-only rotation,
                        // REUSE the grant (rate limiter, model health table,
                        // event sink). Digest changed -> rebuild everything.
                        if entry.manifest_digest != manifest_digest {
                            entry.grant = Arc::new(build_grant(name, manifest, &self.deps));
                        }
                        entry.manifest_digest = manifest_digest.to_owned();
                        entry.token_sha256 = token_sha256;
                        entry.token_id = token_id.to_owned();
                        // A register/rotate is a deliberate re-pairing action
                        // (`crate::attached::register`'s own doc): it always
                        // clears `disabled`, mirroring the fresh
                        // `AttachedRecord` it just wrote to disk.
                        entry.disabled = false;
                    }
                    MapEntry::Vacant(v) => {
                        v.insert(Entry {
                            manifest_digest: manifest_digest.to_owned(),
                            token_sha256,
                            token_id: token_id.to_owned(),
                            disabled: false,
                            slot: AttachSlot::new(),
                            grant: Arc::new(build_grant(name, manifest, &self.deps)),
                            last_generation: None,
                        });
                    }
                }
            }
            Change::Revoked { name } => {
                if let Some(mut entry) = map.remove(name) {
                    entry.slot.revoke();
                }
            }
            Change::Disabled { name, disabled } => {
                if let Some(entry) = map.get_mut(name) {
                    entry.disabled = disabled;
                    if disabled {
                        entry.slot.revoke();
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
        });
        (digest, token_id)
    }

    fn err_of<T>(r: Result<T, HandshakeError>) -> HandshakeError {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
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
            err_of(registry.commit(&stale_claim)),
            HandshakeError::AuthFailed
        );
    }

    #[tokio::test]
    async fn a_second_commit_while_one_is_live_is_busy() {
        let registry = AttachRegistry::new(deps().await);
        let (_, token_id) = register(&registry, "agentear", &["events"]);
        let claim = accepted("agentear", &token_id);
        registry.commit(&claim).unwrap();
        assert_eq!(err_of(registry.commit(&claim)), HandshakeError::Busy);
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
        assert_eq!(err_of(registry.commit(&claim)), HandshakeError::Forbidden);
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
            Arc::clone(&map.get("agentear").unwrap().grant)
        };
        // Re-register with the SAME digest (token-only rotation).
        register(&registry, "agentear", &["events"]);
        let grant_after = {
            let map = registry.inner.lock().unwrap();
            Arc::clone(&map.get("agentear").unwrap().grant)
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
}
