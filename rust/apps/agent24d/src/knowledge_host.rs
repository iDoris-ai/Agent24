//! The daemon side of the private Knowledge host interface (KB-HOST-01 PR-B,
//! #1085 design v2 §2). Only the desktop's ProductHost capability may use it:
//! it activates and deactivates Knowledge bindings and hands the daemon the
//! short-lived credential C of an active binding, or takes it back. C lives
//! only in this process's memory; no response and no log line carries it.
//!
//! Every record belongs to the ProductHost capability that wrote it (decided
//! 2026-10-24): a lookup always names the capability it is made for, so a
//! binding cannot be found by its id alone (design v2 §3).
//!
//! Each change carries a sequence number from the host, starting at 1, that
//! must be greater than any it sent before for that binding; the binding and
//! its credential share one counter. The high-water mark outlives
//! deactivation, so a change that arrives after a later one cannot undo it: a
//! push overtaken by a revoke or a deactivation cannot bring the old C back.
//! Every change but a push drops C, so C is there only while the newest change
//! applied is the push that set it; a removal overtaken by a reactivation is
//! refused, but the reactivation has already dropped C.
//! A host that does not know whether a change landed sends the state it wants
//! again under a new number. A binding whose mark reaches `u64::MAX` takes no
//! further change until the daemon restarts.
//!
//! A binding id is the string axum's `Path` decodes from the URL, so `b1` and
//! `%62%31` name the same binding. It is only ever a key in this map.
//!
//! A run may name a binding (`RunCreate.knowledge_binding_id`, PR-C): only the
//! host that activated it can, and the run is pinned to that host and that
//! activation, its generation. The pin lives in memory, like the records
//! (decided 2026-10-25): a restarted daemon has a new host capability, so a
//! pin could not match again wherever it was kept, and a run that names a
//! binding but has no pin is refused, never bound anew.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use agent24_components::ComponentState;
use agent24_protocol::state_file::AuthMode;
use axum::body::Body;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Extension, Path, Query, State};
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::capabilities::{Audience, Authorization, UnixSeconds, unix_now};
use crate::knowledge_probe::{Assessed, KnowledgeProbeCache, ProbeTarget, ProbeView};
use agent24_domain::http::read_body_or_response;

use crate::server::{AppState, error_response};

/// Bindings one host may hold records for, deactivated ones included.
pub const MAX_BINDINGS_PER_HOST: usize = 1024;
const MAX_BINDING_ID_BYTES: usize = 256;
/// Run pins kept; the oldest goes first, and its run is then refused.
const MAX_PINNED_RUNS: usize = 4096;
/// Session pins kept. None is ever dropped (see `pin_session`); past this
/// many, a session not yet pinned is refused until the daemon restarts.
const MAX_PINNED_SESSIONS: usize = 65_536;
const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;
/// Knowledge bases one activation may name.
pub const MAX_KNOWLEDGE_BASES: usize = 64;
const MAX_KNOWLEDGE_BASE_ID_CHARS: usize = 256;

/// How the Knowledge service expects C to be presented. `Debug` never prints
/// the secret.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    Bearer(String),
    ApiKey(String),
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("Credential::Bearer(<redacted>)"),
            Self::ApiKey(_) => f.write_str("Credential::ApiKey(<redacted>)"),
        }
    }
}

/// The credential of an active binding, with the sequence number it was
/// pushed under so a caller can tell which C a request used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentCredential {
    pub credential: Credential,
    pub seq: u64,
    pub expires_at: UnixSeconds,
}

/// Where an active binding's requests go and what they may search: the
/// Knowledge server's API base and the knowledge bases of the binding. Set by
/// the activation that made the generation, so it cannot change under a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub base_url: String,
    pub knowledge_base_ids: Vec<String>,
}

/// One active binding as a call sees it, read under one lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingNow {
    pub generation: u64,
    pub endpoint: Option<Endpoint>,
    pub credential: Option<CurrentCredential>,
}

#[derive(Clone, Default)]
struct Entry {
    seq: u64,
    active: bool,
    /// The sequence number of the activation in effect: the generation.
    activated_seq: u64,
    endpoint: Option<Endpoint>,
    credential: Option<CurrentCredential>,
}

/// What a run was bound to when it was created (design v2 §3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunBinding {
    pub host: String,
    pub binding: String,
    pub generation: u64,
}

/// Why a run has no usable binding; Knowledge tools refuse with its code.
// Consumed by the Knowledge tools (PR-E).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnowledgeRefusal {
    /// The run names no binding.
    Unbound,
    /// It names one, but not one active under the host and generation it was
    /// pinned to, or the pin is gone.
    Stale,
}

impl KnowledgeRefusal {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn code(self) -> &'static str {
        match self {
            Self::Unbound => "knowledge_unbound",
            Self::Stale => "knowledge_binding_stale",
        }
    }
}

#[derive(Default)]
struct RunPins {
    pins: HashMap<String, RunBinding>,
    order: VecDeque<String>,
}

/// A change that carries nothing but its sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Also drops the credential: the host pushes C again after activating.
    /// Leaves the binding without an endpoint; see
    /// [`KnowledgeHostRegistry::activate`].
    Activate,
    /// Also drops the credential.
    Deactivate,
    /// Drops the credential; the binding stays as it is.
    RevokeCredential,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeError {
    /// `seq` is not greater than one this host already sent for the binding.
    StaleSequence,
    /// A credential was pushed for a binding that is not active.
    BindingInactive,
    TooManyBindings,
    Unavailable,
}

/// Records keyed by (host capability id, binding id).
#[derive(Default)]
pub struct KnowledgeHostRegistry {
    entries: Mutex<HashMap<(String, String), Entry>>,
    runs: Mutex<RunPins>,
    /// The activation each session's runs were bound to: a session carries
    /// its Knowledge results into later turns, and the Knowledge tools send
    /// those back to the binding's endpoint (ADR-K1-02 §2.1.1 item 6), so a
    /// session never moves to another activation.
    sessions: Mutex<HashMap<String, RunBinding>>,
    /// Woken on every change applied, so a call in flight can tell at once
    /// that its binding moved (`crate::knowledge_backend`).
    changed: tokio::sync::Notify,
    /// `/system/capabilities` answers for `GET /api/v1/components` (ADR-038 C5).
    probes: KnowledgeProbeCache,
}

/// How many times `assess_snapshot` takes a fresh snapshot after a target
/// failed its re-check.
const SNAPSHOT_ATTEMPTS: usize = 3;

/// Probes `targets`; whenever one failed its re-check, `retake` supplies a new
/// snapshot, up to [`SNAPSHOT_ATTEMPTS`] tries in all. `gate_url` is passed to
/// the cache (see `KnowledgeProbeCache::assess_for`).
pub(crate) async fn assess_until_fresh(
    cache: &KnowledgeProbeCache,
    targets: &[ProbeTarget],
    now: Instant,
    current: &(dyn Fn(&ProbeTarget) -> bool + Sync),
    retake: &(dyn Fn() -> Vec<ProbeTarget> + Sync),
    gate_url: Option<&str>,
) -> Assessed {
    let mut retaken;
    let mut snapshot = targets;
    for attempt in 0..SNAPSHOT_ATTEMPTS {
        let assessed = cache.assess_for(snapshot, now, current, gate_url).await;
        if !assessed.stale {
            return assessed;
        }
        if attempt + 1 < SNAPSHOT_ATTEMPTS {
            retaken = retake();
            snapshot = &retaken;
        }
    }
    Assessed::without_urls(ProbeView {
        state: ComponentState::Failed,
        reason: Some("binding_changed".to_owned()),
        version: None,
        detail: None,
    })
}

/// Whether a business call may leave (`KnowledgeHostRegistry::contract_gate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContractGate {
    Open,
    /// The declaration says the contract does not fit, or lacks the operation.
    Incompatible,
    /// No declaration has been obtained.
    NoDeclaration,
}

impl KnowledgeHostRegistry {
    pub fn apply(
        &self,
        host: &str,
        binding: &str,
        seq: u64,
        change: Change,
    ) -> Result<(), ChangeError> {
        self.change(host, binding, seq, |entry| {
            match change {
                Change::Activate => {
                    entry.active = true;
                    entry.activated_seq = seq;
                    entry.endpoint = None;
                    entry.credential = None;
                }
                Change::Deactivate => {
                    entry.active = false;
                    entry.endpoint = None;
                    entry.credential = None;
                }
                Change::RevokeCredential => entry.credential = None,
            }
            Ok(())
        })
    }

    /// Activates `binding` with the endpoint its calls use; like
    /// [`Change::Activate`] otherwise.
    pub fn activate(
        &self,
        host: &str,
        binding: &str,
        seq: u64,
        endpoint: Endpoint,
    ) -> Result<(), ChangeError> {
        self.change(host, binding, seq, |entry| {
            entry.active = true;
            entry.activated_seq = seq;
            entry.endpoint = Some(endpoint);
            entry.credential = None;
            Ok(())
        })
    }

    /// Generation, endpoint and C of `binding` if `host` has it active; an
    /// expired C reads as none. A missing record or a poisoned lock counts as
    /// inactive.
    pub fn binding_now(&self, host: &str, binding: &str, now: UnixSeconds) -> Option<BindingNow> {
        let entries = self.entries.lock().ok()?;
        let entry = entries
            .get(&(host.to_owned(), binding.to_owned()))
            .filter(|entry| entry.active)?;
        let credential = entry
            .credential
            .clone()
            .filter(|current| current.expires_at > now);
        Some(BindingNow {
            generation: entry.activated_seq,
            endpoint: entry.endpoint.clone(),
            credential,
        })
    }

    /// Pushes or replaces C of an active binding.
    pub fn push_credential(
        &self,
        host: &str,
        binding: &str,
        seq: u64,
        credential: Credential,
        expires_at: UnixSeconds,
    ) -> Result<(), ChangeError> {
        self.change(host, binding, seq, |entry| {
            if !entry.active {
                return Err(ChangeError::BindingInactive);
            }
            entry.credential = Some(CurrentCredential {
                credential,
                seq,
                expires_at,
            });
            Ok(())
        })
    }

    /// C of an active binding of `host`, unless it has expired by `now`; an
    /// expired C is dropped here. Only an active binding holds one: a push
    /// needs the binding active, and deactivation drops it.
    // Consumed by run binding (PR-C) and the Provider (KB-C1-01).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn credential(
        &self,
        host: &str,
        binding: &str,
        now: UnixSeconds,
    ) -> Option<CurrentCredential> {
        let mut entries = self.entries.lock().ok()?;
        let entry = entries.get_mut(&(host.to_owned(), binding.to_owned()))?;
        if entry
            .credential
            .as_ref()
            .is_some_and(|current| current.expires_at <= now)
        {
            entry.credential = None;
        }
        entry.credential.clone()
    }

    /// Applies `apply` to a copy of the record and stores it only when `seq`
    /// is fresh and `apply` succeeds, so a refused change leaves no trace.
    fn change(
        &self,
        host: &str,
        binding: &str,
        seq: u64,
        apply: impl FnOnce(&mut Entry) -> Result<(), ChangeError>,
    ) -> Result<(), ChangeError> {
        let mut entries = self.entries.lock().map_err(|_| ChangeError::Unavailable)?;
        let key = (host.to_owned(), binding.to_owned());
        let mut entry = match entries.get(&key) {
            Some(entry) => entry.clone(),
            None if entries.keys().filter(|(owner, _)| owner == host).count()
                >= MAX_BINDINGS_PER_HOST =>
            {
                return Err(ChangeError::TooManyBindings);
            }
            None => Entry::default(),
        };
        if seq <= entry.seq {
            return Err(ChangeError::StaleSequence);
        }
        apply(&mut entry)?;
        entry.seq = seq;
        entries.insert(key, entry);
        drop(entries);
        self.changed.notify_waiters();
        Ok(())
    }

    /// Woken on every change applied after `notified()` was enabled.
    pub fn changed(&self) -> &tokio::sync::Notify {
        &self.changed
    }

    /// The generation of `binding` if `host` has it active. A missing record
    /// or a poisoned lock counts as inactive.
    pub fn generation(&self, host: &str, binding: &str) -> Option<u64> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(&(host.to_owned(), binding.to_owned()))?;
        entry.active.then_some(entry.activated_seq)
    }

    /// Pins `run_id` to what it was bound to at creation.
    pub fn pin_run(&self, run_id: &str, binding: RunBinding) {
        let Ok(mut runs) = self.runs.lock() else {
            return;
        };
        if runs.pins.insert(run_id.to_owned(), binding).is_none() {
            runs.order.push_back(run_id.to_owned());
        }
        while runs.order.len() > MAX_PINNED_RUNS {
            if let Some(oldest) = runs.order.pop_front() {
                runs.pins.remove(&oldest);
            }
        }
    }

    /// Ties `session_id` to `binding`; false when the session's runs were
    /// already bound to another host, binding or activation. A pin is never
    /// dropped, so a session without one had no bound run since this daemon
    /// started and none is still out: its `took_knowledge` (read from the
    /// store) is then complete, and true means results from an activation
    /// lost with a restart, so it is refused too. Past the cap, and on a
    /// poisoned lock, false.
    pub fn pin_session(
        &self,
        session_id: &str,
        binding: &RunBinding,
        took_knowledge: bool,
    ) -> bool {
        let Ok(mut sessions) = self.sessions.lock() else {
            return false;
        };
        match sessions.get(session_id) {
            Some(pinned) => pinned == binding,
            None if took_knowledge || sessions.len() >= MAX_PINNED_SESSIONS => false,
            None => {
                sessions.insert(session_id.to_owned(), binding.clone());
                true
            }
        }
    }

    /// The binding a run may use now: the one it names in its persisted input
    /// (`selector`), still active under the host and generation it was pinned
    /// to. Nothing else is looked up, so a run never moves to a new binding.
    // Consumed by the Knowledge tools (PR-E).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn resolve_run(
        &self,
        run_id: &str,
        selector: Option<&str>,
    ) -> Result<RunBinding, KnowledgeRefusal> {
        let selector = selector.ok_or(KnowledgeRefusal::Unbound)?;
        let pin = self
            .runs
            .lock()
            .ok()
            .and_then(|runs| runs.pins.get(run_id).cloned())
            .filter(|pin| pin.binding == selector)
            .ok_or(KnowledgeRefusal::Stale)?;
        match self.generation(&pin.host, &pin.binding) {
            Some(generation) if generation == pin.generation => Ok(pin),
            _ => Err(KnowledgeRefusal::Stale),
        }
    }

    /// Active bindings that name an endpoint, one per base URL.
    ///
    /// A live credential beats the same URL with none, because its seq is at
    /// least 1. Several live credentials use the greatest seq, then the
    /// greatest `(host, binding)`. `HashMap` iteration order is not the rule.
    /// A poisoned `entries` lock still yields the records (`into_inner`); an
    /// empty list would show a configured binding as `stopped`.
    pub(crate) fn probe_targets(&self, now: UnixSeconds) -> Vec<ProbeTarget> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        struct Choice {
            epoch: u64,
            owner: (String, String),
            target: ProbeTarget,
        }
        let mut by_url: BTreeMap<String, Choice> = BTreeMap::new();
        for (owner, entry) in entries.iter() {
            if !entry.active {
                continue;
            }
            let Some(endpoint) = &entry.endpoint else {
                continue;
            };
            let current = entry
                .credential
                .clone()
                .filter(|current| current.expires_at > now);
            let epoch = current.as_ref().map(|current| current.seq).unwrap_or(0);
            let credential = current.map(|current| match current.credential {
                Credential::Bearer(token) => agent24_knowledge::Credential::Bearer(token),
                Credential::ApiKey(key) => agent24_knowledge::Credential::ApiKey(key),
            });
            let base_url = endpoint.base_url.clone();
            let replace = match by_url.get(&base_url) {
                None => true,
                Some(prev) => (epoch, owner) > (prev.epoch, &prev.owner),
            };
            if replace {
                by_url.insert(
                    base_url.clone(),
                    Choice {
                        epoch,
                        owner: owner.clone(),
                        target: ProbeTarget {
                            host: owner.0.clone(),
                            binding: owner.1.clone(),
                            base_url,
                            credential,
                            credential_epoch: epoch,
                        },
                    },
                );
            }
        }
        by_url.into_values().map(|choice| choice.target).collect()
    }

    /// The snapshotted target still names an active binding whose endpoint and
    /// credential epoch have not moved. A probe calls this again before it
    /// sends, and skips the target when this is false.
    pub(crate) fn probe_target_current(&self, target: &ProbeTarget) -> bool {
        let now = unix_now();
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = entries.get(&(target.host.clone(), target.binding.clone())) else {
            return false;
        };
        if !entry.active {
            return false;
        }
        let Some(endpoint) = &entry.endpoint else {
            return false;
        };
        if endpoint.base_url != target.base_url {
            return false;
        }
        let epoch = entry
            .credential
            .as_ref()
            .filter(|current| current.expires_at > now)
            .map(|current| current.seq)
            .unwrap_or(0);
        epoch == target.credential_epoch
    }

    /// How many component reads are waiting on the current capabilities flight.
    #[cfg(test)]
    pub(crate) fn inflight_receivers(&self) -> usize {
        self.probes.inflight_receivers()
    }

    /// D5 remote subset for Knowledge, from a cached capabilities probe.
    pub(crate) async fn component_status(&self, now: Instant) -> ProbeView {
        let targets = self.probe_targets(unix_now());
        self.assess_snapshot(&targets, now).await
    }

    /// Probe `targets` as snapshotted. A binding that was deactivated or whose
    /// credential epoch moved since the snapshot is not called, and the answer
    /// that left it out is not used (N1 of #1571: it could read `ready` for an
    /// endpoint that fails now): the snapshot is taken again. If the bindings
    /// keep moving through every retry, the answer is `failed`
    /// (`binding_changed`), never a stale `ready` or `stopped`.
    pub(crate) async fn assess_snapshot(&self, targets: &[ProbeTarget], now: Instant) -> ProbeView {
        self.assess_snapshot_for(targets, now, None).await.view
    }

    async fn assess_snapshot_for(
        &self,
        targets: &[ProbeTarget],
        now: Instant,
        gate_url: Option<&str>,
    ) -> Assessed {
        assess_until_fresh(
            &self.probes,
            targets,
            now,
            &|target| self.probe_target_current(target),
            &|| self.probe_targets(unix_now()),
            gate_url,
        )
        .await
    }

    /// D3: no business call before the version check. Reads the same probe
    /// cache as `GET /api/v1/components` (an expired entry runs one probe, in
    /// the same singleflight) but looks only at `base_url`, the endpoint this
    /// call goes to: another binding's failure or mismatch does not refuse it.
    /// `operation` is `search` or `read`.
    pub(crate) async fn contract_gate(
        &self,
        base_url: &str,
        operation: &str,
        now: Instant,
    ) -> ContractGate {
        let targets = self.probe_targets(unix_now());
        let assessed = self
            .assess_snapshot_for(&targets, now, Some(base_url))
            .await;
        let Some(view) = assessed.per_url.get(base_url) else {
            // This endpoint was not probed: it left the snapshot, or the
            // bindings kept moving.
            return ContractGate::NoDeclaration;
        };
        match view.state {
            ComponentState::Incompatible => ContractGate::Incompatible,
            // No capability declaration was obtained: unreachable, rejected,
            // unreadable.
            ComponentState::Failed | ComponentState::Stopped => ContractGate::NoDeclaration,
            ComponentState::Degraded => {
                let lacks = view
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("missing_capabilities"))
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|missing| missing.iter().any(|name| name == operation));
                if lacks {
                    ContractGate::Incompatible
                } else {
                    ContractGate::Open
                }
            }
            _ => ContractGate::Open,
        }
    }

    /// Whether `host` has `binding` active.
    // Consumed by run binding (PR-C) and the Provider (KB-C1-01).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_active(&self, host: &str, binding: &str) -> bool {
        self.entries.lock().is_ok_and(|entries| {
            entries
                .get(&(host.to_owned(), binding.to_owned()))
                .is_some_and(|entry| entry.active)
        })
    }
}

/// The capability id of the ProductHost making this request. Legacy mode has
/// no such authority (its shared token also reaches every other route), and a
/// Creative runtime is not the host, so both get `None`.
fn host_capability(
    state: &AppState,
    authorization: Option<Extension<Authorization>>,
) -> Option<String> {
    match (state.auth_mode, authorization) {
        (AuthMode::Capabilities, Some(Extension(authorization)))
            if authorization.claims.audience == Audience::ProductHost =>
        {
            Some(authorization.claims.capability_id)
        }
        _ => None,
    }
}

/// Opaque, but never a path: an encoded `/` or `\` in the URL must not turn
/// into one after routing, and `.` and `..` are no ids either.
fn valid_binding_id(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= MAX_BINDING_ID_BYTES
        && !value
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

fn change_response(result: Result<(), ChangeError>) -> Response {
    let (status, code, message) = match result {
        Ok(()) => return StatusCode::NO_CONTENT.into_response(),
        Err(ChangeError::StaleSequence) => (
            StatusCode::CONFLICT,
            "stale_sequence",
            "A later change for this binding was already applied",
        ),
        Err(ChangeError::BindingInactive) => (
            StatusCode::CONFLICT,
            "binding_inactive",
            "The binding is not active",
        ),
        Err(ChangeError::TooManyBindings) => (
            StatusCode::CONFLICT,
            "too_many_bindings",
            "Too many Knowledge bindings",
        ),
        Err(ChangeError::Unavailable) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "knowledge_host_unavailable",
            "Knowledge host state is unavailable",
        ),
    };
    error_response(status, code, message)
}

/// Says nothing about which part was wrong: a parse error could quote C.
fn invalid_request() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Malformed request",
    )
}

#[derive(Deserialize)]
pub struct SeqQuery {
    seq: u64,
}

/// Why a request was refused before it reached the registry.
pub enum Refused {
    NotHost,
    Malformed,
    Inactive,
    /// The session's earlier runs were bound to another activation, or took
    /// in Knowledge results from one it can no longer tell.
    SessionRebound,
    /// The session's tool result origins could not be read.
    SourcesUnavailable,
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        match self {
            Self::NotHost => error_response(
                StatusCode::FORBIDDEN,
                "host_authority_required",
                "Only the product host may use this interface",
            ),
            Self::Malformed => invalid_request(),
            Self::Inactive => error_response(
                StatusCode::CONFLICT,
                "knowledge_binding_inactive",
                "The product host has no active Knowledge binding by that id",
            ),
            Self::SessionRebound => error_response(
                StatusCode::CONFLICT,
                "knowledge_session_rebound",
                "This session's runs used another activation of that Knowledge binding; start a new session",
            ),
            Self::SourcesUnavailable => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "source_policy_unavailable",
                "This session's sources could not be read",
            ),
        }
    }
}

/// The host making the request, the binding and the change's sequence number.
/// An id that is not UTF-8 gets the same answer as any other malformed one.
fn host_and_seq(
    state: &AppState,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SeqQuery>, QueryRejection>,
) -> Result<(String, String, u64), Refused> {
    let host = host_capability(state, authorization).ok_or(Refused::NotHost)?;
    match (path, query) {
        (Ok(Path(binding_id)), Ok(Query(SeqQuery { seq })))
            if seq > 0 && valid_binding_id(&binding_id) =>
        {
            Ok((host, binding_id, seq))
        }
        _ => Err(Refused::Malformed),
    }
}

/// Checks a run's `knowledge_binding_id` at creation and returns what to pin
/// the run to: only the product host may name a binding, only one it has
/// active, and in a session only the activation its earlier runs had.
pub async fn bind_run(
    state: &AppState,
    authorization: Option<Extension<Authorization>>,
    binding_id: &str,
    session_id: Option<&str>,
) -> Result<RunBinding, Refused> {
    let host = host_capability(state, authorization).ok_or(Refused::NotHost)?;
    if !valid_binding_id(binding_id) {
        return Err(Refused::Malformed);
    }
    let generation = state
        .knowledge_host
        .generation(&host, binding_id)
        .ok_or(Refused::Inactive)?;
    let binding = RunBinding {
        host,
        binding: binding_id.to_owned(),
        generation,
    };
    if let Some(session_id) = session_id {
        // Read before the pin is taken; a run that adds such results has
        // pinned the session before it executes.
        let took_knowledge = state
            .store
            .session_tool_result_origins(session_id)
            .await
            .map_err(|_| Refused::SourcesUnavailable)?
            .iter()
            .any(|origin| crate::knowledge_tools::KNOWLEDGE_ORIGINS.contains(&origin.as_str()));
        if !state
            .knowledge_host
            .pin_session(session_id, &binding, took_knowledge)
        {
            return Err(Refused::SessionRebound);
        }
    }
    Ok(binding)
}

/// `PUT /api/v1/host/knowledge/bindings/{binding_id}?seq=n` activates the
/// binding, with `{"endpoint", "knowledge_base_ids"}` for its calls or with
/// no body (then every call is refused); `DELETE` of the same deactivates it.
pub async fn change(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    method: Method,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SeqQuery>, QueryRejection>,
    req: Request<Body>,
) -> Response {
    let (host, binding_id, seq) = match host_and_seq(&state, authorization, path, query) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    let registry = &state.knowledge_host;
    if method != Method::PUT {
        return change_response(registry.apply(&host, &binding_id, seq, Change::Deactivate));
    }
    let body = match read_body_or_response(req).await {
        Ok(body) => body,
        Err(refused) => return refused,
    };
    if body.is_empty() {
        return change_response(registry.apply(&host, &binding_id, seq, Change::Activate));
    }
    let Some(endpoint) = endpoint_of(&body) else {
        return invalid_request();
    };
    change_response(registry.activate(&host, &binding_id, seq, endpoint))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivateBody {
    endpoint: String,
    knowledge_base_ids: Vec<String>,
}

/// The activation body `{"endpoint", "knowledge_base_ids"}`: an API base the
/// HTTP transport accepts (`https`, or `http` to a loopback IP) and 1 to
/// [`MAX_KNOWLEDGE_BASES`] distinct, non-blank ids.
fn endpoint_of(body: &[u8]) -> Option<Endpoint> {
    let body: ActivateBody = serde_json::from_slice(body).ok()?;
    agent24_knowledge::validate_base_url(&body.endpoint).ok()?;
    let ids = &body.knowledge_base_ids;
    let id_ok =
        |id: &String| !id.trim().is_empty() && id.chars().count() <= MAX_KNOWLEDGE_BASE_ID_CHARS;
    let distinct = ids.iter().collect::<std::collections::HashSet<_>>().len() == ids.len();
    let ids_ok = !ids.is_empty() && ids.len() <= MAX_KNOWLEDGE_BASES && distinct;
    let ok = ids_ok && ids.iter().all(id_ok);
    ok.then_some(Endpoint {
        base_url: body.endpoint,
        knowledge_base_ids: body.knowledge_base_ids,
    })
}

/// `DELETE /api/v1/host/knowledge/bindings/{binding_id}/credential?seq=n`
/// drops C at once. Accepted for a binding this host never activated too, so
/// it can be sent before the push it overtakes.
pub async fn revoke_credential(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SeqQuery>, QueryRejection>,
) -> Response {
    let (host, binding_id, seq) = match host_and_seq(&state, authorization, path, query) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    change_response(
        state
            .knowledge_host
            .apply(&host, &binding_id, seq, Change::RevokeCredential),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CredentialKind {
    Bearer,
    ApiKey,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialBody {
    kind: CredentialKind,
    value: String,
    expires_at: UnixSeconds,
}

/// `PUT /api/v1/host/knowledge/bindings/{binding_id}/credential?seq=n` with
/// `{"kind": "bearer"|"api_key", "value", "expires_at"}` pushes or replaces C.
pub async fn push_credential(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SeqQuery>, QueryRejection>,
    req: Request<Body>,
) -> Response {
    let (host, binding_id, seq) = match host_and_seq(&state, authorization, path, query) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    // The kernel's body cap and its 413 envelope, like every other handler.
    let body = match read_body_or_response(req).await {
        Ok(body) => body,
        Err(refused) => return refused,
    };
    let Ok(body) = serde_json::from_slice::<CredentialBody>(&body) else {
        return invalid_request();
    };
    // Visible ASCII only: C goes into a request header as it is.
    let value_ok = !body.value.is_empty()
        && body.value.len() <= MAX_CREDENTIAL_BYTES
        && body.value.bytes().all(|b| b.is_ascii_graphic());
    if !value_ok || body.expires_at <= unix_now() {
        return invalid_request();
    }
    let credential = match body.kind {
        CredentialKind::Bearer => Credential::Bearer(body.value),
        CredentialKind::ApiKey => Credential::ApiKey(body.value),
    };
    change_response(state.knowledge_host.push_credential(
        &host,
        &binding_id,
        seq,
        credential,
        body.expires_at,
    ))
}

#[cfg(test)]
mod tests;
