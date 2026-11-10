//! Knowledge on `GET /api/v1/components` (ADR-038 C5).
//!
//! A configured binding is a remote service (D5): this probes `GET /system/capabilities`
//! once per cache window and maps the answer.
//!
//! - no endpoint configured → `stopped` (`not_configured`)
//! - TCP/TLS/timeout, or a non-2xx answer → `failed`, reason is a fixed code
//! - an envelope `code` other than exact success (integer `0`, or `"0"` /
//!   `"ok"`) → `failed` (`upstream_<code>`), and `data` is not read as a
//!   declaration. A number that is not that integer (`1.5`, a magnitude
//!   outside `i64`, `-0.5`) is `upstream_error`
//! - declaration missing, a required field absent (`protocol_versions`, `mode`,
//!   `supported_operations`, `server_version`, `limits` — READONLY R9.2 — or
//!   one of `limits`' six schema keys), `mode` is not `integration`, or major
//!   `1` is absent (READONLY R9.4, D3) → `incompatible`
//!   (`component_contract_mismatch`)
//! - major `1` and `integration`, but `search` or `read` is absent (READONLY R9.2;
//!   OPS-v1 does not add a required operation yet) → `degraded`
//!   (`missing_capability:<name>`)
//! - otherwise, with no credential → `ready`. This only means the capability
//!   declaration is complete and the contract is compatible. It does not mean
//!   dependencies are healthy (READONLY R9.3). Host detail carries
//!   `basis: "capabilities"`
//! - otherwise, with a credential → the declaration is judged first (above);
//!   only a compatible, complete one goes on to `POST /agent24/knowledge/ready`
//!   (§8, R8.2: the probe needs a credential). `live` and both `search` and
//!   `read` `ready` → `ready`; anything else (`not_ready`/`disabled`, `live`
//!   false, a non-2xx answer, a transport error, an unreadable body, a
//!   mismatched echo) → `degraded` (`not_ready`). `ingest` is not looked at
//!   (read-only v1). Host detail carries `basis: "ready"`
//!
//! The client tolerates unknown fields and unknown operation names (D3: minor is
//! additive), including extra keys inside `limits`. The server's `msg` and body
//! text never leave this module; only codes do. `server_version` is copied only
//! when it is a short token. Endpoints are probed together under one deadline.
//! Each request is sent only after the binding is still active and the
//! credential epoch is unchanged. A target that fails that re-check is not
//! judged and not cached: [`KnowledgeProbeCache::assess`] reports the answer
//! as `stale`, and the caller takes a new snapshot (ADR-038 C5, N1 of #1571).
//!
//! JSON numbers follow JSON Schema's value rule: `-0`, `1.0` and `1e3` are
//! integers. [`integral`] is the one place that reads them.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::future::join_all;

use crate::{
    Credential, HttpConfigError, HttpTransport, HttpTransportConfig, KnowledgeOperation,
    KnowledgeTransport, TransportError,
};
use agent24_components::ComponentState;
use serde_json::{Map, Number, Value, json};
use tokio::sync::watch;

/// How long a probe answer is reused. `GET /api/v1/components` must not hit the
/// remote on every poll.
pub const KNOWLEDGE_PROBE_TTL: Duration = Duration::from_secs(30);

/// How long the D3 gate trusts a `failed` reading of its own endpoint before it
/// asks again. A failed endpoint has no declaration, so every call to it is
/// refused; a long window would keep refusing after it recovered.
pub const KNOWLEDGE_PROBE_FAILED_TTL: Duration = Duration::from_secs(4);

/// Bound on one capabilities exchange, connect included (D6).
pub const KNOWLEDGE_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// READONLY R3.2: the only contract major this build speaks.
const EXPECTED_PROTOCOL_MAJOR: u64 = 1;

/// READONLY R9.2 operations this build requires. Alphabetical, so the reason
/// names a stable one when several are missing.
const REQUIRED_OPERATIONS: [&str; 2] = ["read", "search"];

/// `capabilities.schema.json` `limits.required`. Order is the schema's.
const LIMIT_KEYS: [&str; 6] = [
    "max_results",
    "max_concurrency",
    "max_deadline_ms",
    "max_read_bytes",
    "max_page_limit",
    "max_snippet_bytes",
];

const MAX_CAPABILITIES_BYTES: usize = 64 * 1024;

const BASIS_CAPABILITIES: &str = "capabilities";
const BASIS_READY: &str = "ready";

/// `degraded` reason when §8 says the service is not ready.
const NOT_READY: &str = "not_ready";

/// `generation` of a readiness probe. It is a probe, not a call under an
/// activation; the schema wants a positive integer and the server echoes it.
const READY_PROBE_GENERATION: u64 = 1;

/// One configured endpoint to probe.
///
/// The cache and singleflight key is the base URL, `(host, binding)` and
/// `credential_epoch`. Two bindings can reuse a sequence number; the binding
/// id keeps their answers apart. The epoch is not a secret, and the credential
/// itself is not part of the key.
pub struct ProbeTarget {
    pub host: String,
    pub binding: String,
    pub base_url: String,
    pub credential: Option<Credential>,
    pub credential_epoch: u64,
}

/// What `GET /api/v1/components` shows for Knowledge.
#[derive(Clone, Debug)]
pub struct ProbeView {
    pub state: ComponentState,
    pub reason: Option<String>,
    pub version: Option<String>,
    pub detail: Option<Value>,
}

#[derive(Clone)]
struct ContractSeen {
    actual: Vec<u64>,
    mode: Option<&'static str>,
}

#[derive(Clone)]
struct Judged {
    state: ComponentState,
    reason: Option<String>,
    version: Option<String>,
    missing: Vec<String>,
    contract: Option<ContractSeen>,
    /// What the answer rests on: `capabilities`, or `ready` once §8 was probed.
    basis: &'static str,
    probed_at: u64,
}

impl Judged {
    fn failed(code: impl Into<String>, probed_at: u64) -> Self {
        Self {
            state: ComponentState::Failed,
            reason: Some(code.into()),
            version: None,
            missing: Vec::new(),
            contract: None,
            basis: BASIS_CAPABILITIES,
            probed_at,
        }
    }

    fn rank(&self) -> u8 {
        match self.state {
            ComponentState::Incompatible => 3,
            ComponentState::Failed => 2,
            ComponentState::Degraded => 1,
            _ => 0,
        }
    }

    fn view(&self) -> ProbeView {
        ProbeView {
            state: self.state,
            reason: self.reason.clone(),
            version: self.version.clone(),
            detail: Some(json!({
                "basis": self.basis,
                "probed_at": self.probed_at,
                "missing_capabilities": self.missing,
                "contract_version": contract_value(self.contract.as_ref()),
            })),
        }
    }
}

struct Decl {
    majors: Vec<u64>,
    mode: String,
    operations: BTreeSet<String>,
    server_version: Option<String>,
}

enum ParseError {
    Malformed,
    NoDeclaration,
    Upstream(String),
}

/// What [`KnowledgeProbeCache::assess`] hands back. `stale` means a target
/// failed the re-check before it was sent, so `view` rests on a snapshot that
/// no longer holds. It was not cached; the caller takes a new snapshot.
pub struct Assessed {
    pub view: ProbeView,
    /// One answer per probed base URL, for the caller that needs a single
    /// endpoint's reading (the D3 gate) instead of the folded `view`.
    pub per_url: Arc<HashMap<String, ProbeView>>,
    pub stale: bool,
}

impl Assessed {
    fn fresh(answer: Answer) -> Self {
        Self {
            view: answer.view,
            per_url: answer.per_url,
            stale: false,
        }
    }

    /// No cache or probe behind it (nothing configured, or the bindings kept
    /// moving).
    pub(crate) fn without_urls(view: ProbeView) -> Self {
        Self {
            view,
            per_url: Arc::default(),
            stale: false,
        }
    }
}

/// What one probe round produced: the folded view the component list shows and
/// each endpoint's own reading.
#[derive(Clone)]
struct Answer {
    view: ProbeView,
    per_url: Arc<HashMap<String, ProbeView>>,
}

impl CacheEntry {
    fn fresh_for(&self, gate_url: Option<&str>) -> Duration {
        let failed = gate_url
            .and_then(|url| self.answer.per_url.get(url))
            .is_some_and(|view| view.state == ComponentState::Failed);
        if failed {
            KNOWLEDGE_PROBE_FAILED_TTL
        } else {
            KNOWLEDGE_PROBE_TTL
        }
    }
}

struct CacheEntry {
    fingerprint: String,
    at: Instant,
    answer: Answer,
}

#[derive(Default)]
struct CacheInner {
    entry: Option<CacheEntry>,
    flights: HashMap<String, watch::Sender<Option<Answer>>>,
}

/// Shared by every `GET /api/v1/components`. One in-flight probe per endpoint
/// set; callers that arrive while it runs wait for that answer. A cancelled
/// leader only removes its flight. It does not publish a failure; waiters
/// enter `assess` again.
#[derive(Default)]
pub struct KnowledgeProbeCache {
    inner: Mutex<CacheInner>,
}

struct FlightGuard<'a> {
    cache: &'a KnowledgeProbeCache,
    fingerprint: String,
    tx: Option<watch::Sender<Option<Answer>>>,
    finished: bool,
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // The leader was cancelled before it published. Drop the flight and
        // close the channel with no value. A synthetic `transport` failure
        // would be this task's cancellation, not the remote's answer.
        let Some(tx) = self.tx.take() else {
            return;
        };
        let mut guard = self
            .cache
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        guard.flights.remove(&self.fingerprint);
        drop(guard);
        drop(tx);
    }
}

impl KnowledgeProbeCache {
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn assess(
        &self,
        targets: &[ProbeTarget],
        now: Instant,
        still_valid: &(dyn Fn(&ProbeTarget) -> bool + Sync),
    ) -> Assessed {
        self.assess_for(targets, now, still_valid, None).await
    }

    /// As [`Self::assess`]. With `gate_url`, an entry whose reading of that
    /// endpoint is `failed` is reused for [`KNOWLEDGE_PROBE_FAILED_TTL`] only,
    /// so one failure does not close the D3 gate for a whole
    /// [`KNOWLEDGE_PROBE_TTL`]. The component list passes `None`.
    pub(crate) async fn assess_for(
        &self,
        targets: &[ProbeTarget],
        now: Instant,
        still_valid: &(dyn Fn(&ProbeTarget) -> bool + Sync),
        gate_url: Option<&str>,
    ) -> Assessed {
        if targets.is_empty() {
            return Assessed::without_urls(not_configured());
        }
        let fingerprint = fingerprint(targets);
        loop {
            enum Role {
                Hit(Answer),
                Wait(watch::Receiver<Option<Answer>>),
                Lead(watch::Sender<Option<Answer>>),
            }
            let role = {
                let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(entry) = &guard.entry
                    && entry.fingerprint == fingerprint
                    && now.saturating_duration_since(entry.at) < entry.fresh_for(gate_url)
                {
                    Role::Hit(entry.answer.clone())
                } else if let Some(tx) = guard.flights.get(&fingerprint) {
                    Role::Wait(tx.subscribe())
                } else {
                    let (tx, _rx) = watch::channel(None);
                    guard.flights.insert(fingerprint.clone(), tx.clone());
                    Role::Lead(tx)
                }
            };
            match role {
                Role::Hit(answer) => return Assessed::fresh(answer),
                Role::Wait(rx) => {
                    if let Some(answer) = wait_flight(rx).await {
                        return Assessed::fresh(answer);
                    }
                    // The leader left without a result. Try again: hit a
                    // newer cache entry, wait on a new leader, or lead.
                }
                Role::Lead(tx) => {
                    let mut flight = FlightGuard {
                        cache: self,
                        fingerprint: fingerprint.clone(),
                        tx: Some(tx),
                        finished: false,
                    };
                    let (answer, stale) = probe_all(targets, still_valid).await;
                    if stale {
                        // Not the answer for any current set of targets. Leave
                        // without publishing: the guard drops the flight and
                        // the waiters go round again.
                        return Assessed {
                            view: answer.view,
                            per_url: answer.per_url,
                            stale,
                        };
                    }
                    {
                        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
                        guard.entry = Some(CacheEntry {
                            fingerprint: fingerprint.clone(),
                            at: now,
                            answer: answer.clone(),
                        });
                        guard.flights.remove(&fingerprint);
                    }
                    if let Some(tx) = flight.tx.take() {
                        let _ = tx.send(Some(answer.clone()));
                    }
                    flight.finished = true;
                    return Assessed::fresh(answer);
                }
            }
        }
    }

    /// Receivers parked on the current flight. Tests use this to abort the
    /// leader only after a waiter has subscribed.
    pub(crate) fn inflight_receivers(&self) -> usize {
        let guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard
            .flights
            .values()
            .map(watch::Sender::receiver_count)
            .sum()
    }
}

/// `None` when the leader closed the channel without publishing. The last
/// published value still counts if the send won the race with the drop.
async fn wait_flight(mut rx: watch::Receiver<Option<Answer>>) -> Option<Answer> {
    loop {
        if let Some(view) = rx.borrow().clone() {
            return Some(view);
        }
        if rx.changed().await.is_err() {
            return rx.borrow().clone();
        }
    }
}

fn not_configured() -> ProbeView {
    ProbeView {
        state: ComponentState::Stopped,
        reason: Some("not_configured".to_owned()),
        version: None,
        detail: None,
    }
}

fn fingerprint(targets: &[ProbeTarget]) -> String {
    let mut lines: Vec<String> = targets
        .iter()
        .map(|target| {
            format!(
                "{}\n{}\n{}\n{}",
                target.base_url, target.host, target.binding, target.credential_epoch
            )
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

/// Every target shares `KNOWLEDGE_PROBE_TIMEOUT` measured from this call.
/// A target whose binding was deactivated, or whose credential epoch moved,
/// is omitted and not requested; the flag says one was.
async fn probe_all(
    targets: &[ProbeTarget],
    still_valid: &(dyn Fn(&ProbeTarget) -> bool + Sync),
) -> (Answer, bool) {
    let deadline = Instant::now() + KNOWLEDGE_PROBE_TIMEOUT;
    let judged = join_all(
        targets
            .iter()
            .map(|target| fetch(target, deadline, still_valid)),
    )
    .await;
    let stale = judged.iter().any(Option::is_none);
    let per_url: HashMap<String, ProbeView> = targets
        .iter()
        .zip(&judged)
        .filter_map(|(target, judged)| Some((target.base_url.clone(), judged.as_ref()?.view())))
        .collect();
    let parts: Vec<Judged> = judged.into_iter().flatten().collect();
    let answer = Answer {
        view: fold(&parts),
        per_url: Arc::new(per_url),
    };
    (answer, stale)
}

fn fold(parts: &[Judged]) -> ProbeView {
    let Some(worst) = parts.iter().map(Judged::rank).max() else {
        return not_configured();
    };
    if worst == 1 {
        return union_degraded(parts);
    }
    if worst == 0 {
        // Several ready endpoints: claim no more than the weakest one proved.
        let weakest = parts
            .iter()
            .rev()
            .find(|part| part.basis == BASIS_CAPABILITIES)
            .or_else(|| parts.last());
        return weakest.map_or_else(not_configured, Judged::view);
    }
    parts
        .iter()
        .rev()
        .find(|part| part.rank() == worst)
        .map(Judged::view)
        .unwrap_or_else(not_configured)
}

fn union_degraded(parts: &[Judged]) -> ProbeView {
    let mut missing = BTreeSet::new();
    let mut version = None;
    let mut contract = None;
    let mut probed_at = 0;
    let mut basis = BASIS_READY;
    for part in parts {
        if part.state != ComponentState::Degraded {
            continue;
        }
        missing.extend(part.missing.iter().cloned());
        if version.is_none() {
            version.clone_from(&part.version);
        }
        if contract.is_none() {
            contract.clone_from(&part.contract);
        }
        probed_at = part.probed_at;
        if part.basis == BASIS_CAPABILITIES {
            basis = BASIS_CAPABILITIES;
        }
    }
    let missing: Vec<String> = missing.into_iter().collect();
    // A named missing operation says more than "not ready".
    let reason = Some(missing.first().map_or_else(
        || NOT_READY.to_owned(),
        |name| format!("missing_capability:{name}"),
    ));
    Judged {
        state: ComponentState::Degraded,
        reason,
        version,
        missing,
        contract,
        basis,
        probed_at,
    }
    .view()
}

async fn fetch(
    target: &ProbeTarget,
    deadline: Instant,
    still_valid: &(dyn Fn(&ProbeTarget) -> bool + Sync),
) -> Option<Judged> {
    // After the list handler snapshotted this target, the binding may have
    // been deactivated or the credential replaced. Do not send the old key.
    if !still_valid(target) {
        return None;
    }
    let budget = deadline.saturating_duration_since(Instant::now());
    if budget.is_zero() {
        return Some(Judged::failed("timeout", probed_at_now()));
    }
    let built = match &target.credential {
        Some(credential) => HttpTransport::new(HttpTransportConfig {
            base_url: target.base_url.clone(),
            credential: credential.clone(),
            max_response_bytes: MAX_CAPABILITIES_BYTES,
            connect_timeout: budget,
        }),
        None => HttpTransport::anonymous(target.base_url.clone(), MAX_CAPABILITIES_BYTES, budget),
    };
    let transport = match built {
        Ok(transport) => transport,
        Err(err) => return Some(judge_config(err)),
    };
    let judged = match transport
        .send(KnowledgeOperation::Capabilities, Vec::new(), budget)
        .await
    {
        Ok(response) => judge_response(response.status, &response.body),
        Err(err) => judge_transport(err),
    };
    // §8 needs a credential (R8.2), and only a declaration that is already
    // compatible and complete is worth asking the service about.
    if judged.state != ComponentState::Ready || target.credential.is_none() {
        return Some(judged);
    }
    // Same rule as before the first request: the key goes out again only if
    // it is still the binding's.
    if !still_valid(target) {
        return None;
    }
    let judged = probe_ready(&transport, deadline, judged).await;
    // The service has answered; if the key moved while it did, the answer
    // belongs to a snapshot that no longer holds.
    still_valid(target).then_some(judged)
}

/// `POST /agent24/knowledge/ready` (R8.2, no scope). `judged` is the compatible
/// declaration it refines.
async fn probe_ready(transport: &HttpTransport, deadline: Instant, judged: Judged) -> Judged {
    let budget = deadline.saturating_duration_since(Instant::now());
    let request_id = uuid::Uuid::new_v4().to_string();
    let ready = if budget.is_zero() {
        false
    } else {
        let body = json!({
            "protocol_version": EXPECTED_PROTOCOL_MAJOR,
            "request_id": request_id,
            "generation": READY_PROBE_GENERATION,
        });
        match transport
            .send(
                KnowledgeOperation::Ready,
                body.to_string().into_bytes(),
                budget,
            )
            .await
        {
            Ok(response) => {
                (200..300).contains(&response.status) && ready_ok(&response.body, &request_id)
            }
            Err(_) => false,
        }
    };
    if ready {
        return Judged {
            basis: BASIS_READY,
            probed_at: probed_at_now(),
            ..judged
        };
    }
    Judged {
        state: ComponentState::Degraded,
        reason: Some(NOT_READY.to_owned()),
        basis: BASIS_READY,
        probed_at: probed_at_now(),
        ..judged
    }
}

/// R8.2: the process is live and both operations this build calls are `ready`.
/// `ingest` is `disabled` in read-only v1 and is not looked at. The echoed
/// `request_id` must be ours.
fn ready_ok(body: &[u8], request_id: &str) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let operation_ready = |name: &str| {
        value
            .pointer(&format!("/operations/{name}/state"))
            .and_then(Value::as_str)
            == Some("ready")
    };
    value.get("request_id").and_then(Value::as_str) == Some(request_id)
        && value.get("live").and_then(Value::as_bool) == Some(true)
        && REQUIRED_OPERATIONS.into_iter().all(operation_ready)
}

fn judge_config(err: HttpConfigError) -> Judged {
    let code = match err {
        HttpConfigError::InvalidCredential => "invalid_credential",
        _ => "unreachable",
    };
    Judged::failed(code, probed_at_now())
}

fn judge_transport(err: TransportError) -> Judged {
    let code = match err {
        TransportError::Timeout => "timeout",
        TransportError::Unreachable => "unreachable",
        TransportError::ResponseTooLarge => "response_too_large",
        TransportError::Other(_) => "transport",
    };
    Judged::failed(code, probed_at_now())
}

fn judge_response(status: u16, body: &[u8]) -> Judged {
    let probed_at = probed_at_now();
    if !(200..300).contains(&status) {
        return Judged::failed(format!("http_{status}"), probed_at);
    }
    match parse(body) {
        Ok(decl) => judge_decl(decl, probed_at),
        Err(ParseError::Malformed) => Judged::failed("malformed", probed_at),
        Err(ParseError::NoDeclaration) => incompatible(None, None, probed_at),
        Err(ParseError::Upstream(code)) => Judged::failed(code, probed_at),
    }
}

fn judge_decl(decl: Decl, probed_at: u64) -> Judged {
    let mode = known_mode(&decl.mode);
    let contract = Some(ContractSeen {
        actual: decl.majors.clone(),
        mode,
    });
    let version = decl.server_version.as_deref().and_then(safe_version);
    if !decl.majors.contains(&EXPECTED_PROTOCOL_MAJOR) || mode != Some("integration") {
        return incompatible(contract, version, probed_at);
    }
    let missing: Vec<String> = REQUIRED_OPERATIONS
        .into_iter()
        .filter(|op| !decl.operations.contains(*op))
        .map(str::to_owned)
        .collect();
    if missing.is_empty() {
        return Judged {
            state: ComponentState::Ready,
            reason: None,
            version,
            missing,
            contract,
            basis: BASIS_CAPABILITIES,
            probed_at,
        };
    }
    let reason = missing
        .first()
        .map(|name| format!("missing_capability:{name}"));
    Judged {
        state: ComponentState::Degraded,
        reason,
        version,
        missing,
        contract,
        basis: BASIS_CAPABILITIES,
        probed_at,
    }
}

fn incompatible(contract: Option<ContractSeen>, version: Option<String>, probed_at: u64) -> Judged {
    Judged {
        state: ComponentState::Incompatible,
        reason: Some("component_contract_mismatch".to_owned()),
        version,
        missing: Vec::new(),
        contract,
        basis: BASIS_CAPABILITIES,
        probed_at,
    }
}

fn known_mode(mode: &str) -> Option<&'static str> {
    match mode {
        "integration" => Some("integration"),
        "standalone" => Some("standalone"),
        _ => None,
    }
}

/// A version token safe to show. Anything else (spaces, paths, prose) is dropped.
fn safe_version(raw: &str) -> Option<String> {
    let ok = (1..=64).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
    ok.then(|| raw.to_owned())
}

fn contract_value(seen: Option<&ContractSeen>) -> Value {
    let Some(seen) = seen else {
        return Value::Null;
    };
    let mut value = Map::new();
    value.insert("expected".to_owned(), json!("1"));
    value.insert("actual".to_owned(), json!(seen.actual));
    if let Some(mode) = seen.mode {
        value.insert("mode".to_owned(), json!(mode));
    }
    Value::Object(value)
}

fn probed_at_now() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        Err(_) => 0,
    }
}

fn parse(body: &[u8]) -> Result<Decl, ParseError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ParseError::Malformed)?;
    // A non-success envelope is the upstream's answer. `data: {}` (or a
    // document that is not a declaration) must not be judged as the contract.
    if let Some(code) = upstream_code(&value, raw_code(body).as_deref()) {
        return Err(ParseError::Upstream(code));
    }
    if let Some(data) = value.get("data") {
        if data.is_null() {
            return Err(ParseError::NoDeclaration);
        }
        return parse_data(data);
    }
    if value.get("integration").is_some() {
        return parse_data(&value);
    }
    Err(ParseError::NoDeclaration)
}

fn parse_data(data: &Value) -> Result<Decl, ParseError> {
    let Some(agent) = data
        .get("integration")
        .and_then(|integration| integration.get("agent24_knowledge"))
    else {
        return Err(ParseError::NoDeclaration);
    };
    let Some(agent) = agent.as_object() else {
        return Err(ParseError::Malformed);
    };
    let majors = match agent.get("protocol_versions") {
        Some(Value::Array(items)) if items.is_empty() => return Err(ParseError::NoDeclaration),
        Some(Value::Array(items)) => {
            let mut majors = Vec::with_capacity(items.len());
            for item in items {
                let Some(n) = item.as_u64().filter(|n| *n > 0) else {
                    return Err(ParseError::Malformed);
                };
                majors.push(n);
            }
            majors
        }
        Some(_) => return Err(ParseError::Malformed),
        None => return Err(ParseError::NoDeclaration),
    };
    let mode = match agent.get("mode") {
        Some(Value::String(mode)) => mode.clone(),
        Some(_) => return Err(ParseError::Malformed),
        None => return Err(ParseError::NoDeclaration),
    };
    let operations = match agent.get("supported_operations") {
        Some(Value::Array(items)) => {
            let mut operations = BTreeSet::new();
            for item in items {
                let Some(name) = item.as_str() else {
                    return Err(ParseError::Malformed);
                };
                if !name.is_empty() {
                    operations.insert(name.to_owned());
                }
            }
            operations
        }
        Some(_) => return Err(ParseError::Malformed),
        None => return Err(ParseError::NoDeclaration),
    };
    let server_version = match agent.get("server_version") {
        Some(Value::String(version)) => Some(version.clone()),
        Some(_) => return Err(ParseError::Malformed),
        None => return Err(ParseError::NoDeclaration),
    };
    // READONLY R9.2 / capabilities.schema.json: `limits` is required, same as
    // `server_version`. Absent → incompatible; the wrong JSON type, or a schema
    // key that is not a positive integer → malformed. Extra keys are kept.
    match agent.get("limits") {
        Some(value) => check_limits(value)?,
        None => return Err(ParseError::NoDeclaration),
    }
    Ok(Decl {
        majors,
        mode,
        operations,
        server_version,
    })
}

/// Schema `positive_int` for each required `limits` key. Missing → the
/// declaration is incomplete. Present but not an integer ≥ 1 → malformed.
/// Keys the schema does not name are ignored (D3: a minor only adds).
fn check_limits(value: &Value) -> Result<(), ParseError> {
    let Some(obj) = value.as_object() else {
        return Err(ParseError::Malformed);
    };
    for key in LIMIT_KEYS {
        match obj.get(key) {
            None => return Err(ParseError::NoDeclaration),
            Some(Value::Number(n)) => match integral(n) {
                Some(n) if n >= 1 => {}
                _ => return Err(ParseError::Malformed),
            },
            Some(_) => return Err(ParseError::Malformed),
        }
    }
    Ok(())
}

/// The integer a JSON number stands for, by JSON Schema's value rule: `-0`,
/// `1.0` and `1e3` are integers, `1.5` is not. `None` for a fraction, a
/// non-finite value, or a magnitude the exact range below cannot hold (2^53 and
/// up: a float no longer names one integer).
fn integral(n: &Number) -> Option<i128> {
    if let Some(i) = n.as_i64() {
        return Some(i128::from(i));
    }
    if let Some(u) = n.as_u64() {
        return Some(i128::from(u));
    }
    const EXACT: f64 = 9_007_199_254_740_992.0; // 2^53
    let f = n.as_f64()?;
    // `-0.0` has no fraction and is `0`.
    (f.is_finite() && f.fract() == 0.0 && f.abs() < EXACT).then_some(f as i128)
}

/// The text of the envelope's top-level `code`, as sent. A `Number` has already
/// lost it (`1e-400` is `0.0`).
fn raw_code(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Top<'a> {
        #[serde(borrow)]
        code: Option<&'a serde_json::value::RawValue>,
    }
    let top: Top<'_> = serde_json::from_slice(body).ok()?;
    top.code.map(|raw| raw.get().to_owned())
}

/// Whether a JSON number's text has a nonzero digit before any exponent: the
/// value is not zero, whatever a float made of it.
fn nonzero_mantissa(text: &str) -> bool {
    text.split(['e', 'E'])
        .next()
        .is_some_and(|mantissa| mantissa.bytes().any(|b| (b'1'..=b'9').contains(&b)))
}

/// A numeric or token `code` other than success. Only the integer `0` (however
/// it is written: `0`, `-0`, `0.0`, `0e5`) is a numeric success. A fraction or
/// a magnitude [`integral`] cannot read is `upstream_error`, not a pass. Free text is also
/// `upstream_error`, so the server's sentence is not copied.
fn upstream_code(value: &Value, raw: Option<&str>) -> Option<String> {
    let code = value.get("code")?;
    match code {
        Value::Number(n) => match integral(n) {
            // `1e-400` reads as `0.0` once it is a float. Its text says it is
            // not zero (a fraction too small for f64), so it is not success.
            Some(0) if raw.is_some_and(nonzero_mantissa) => Some("upstream_error".to_owned()),
            Some(0) => None,
            Some(n) => Some(format!("upstream_{n}")),
            None => Some("upstream_error".to_owned()),
        },
        Value::String(s) if s == "0" || s == "ok" => None,
        Value::String(s) if is_code_token(s) => Some(format!("upstream_{s}")),
        Value::String(_) => Some("upstream_error".to_owned()),
        _ => Some("upstream_error".to_owned()),
    }
}

fn is_code_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::{ComponentState, Credential, ProbeTarget, fingerprint, judge_response, ready_ok};
    use serde_json::{Value, json};

    fn target(binding: &str, epoch: u64, secret: &str) -> ProbeTarget {
        ProbeTarget {
            host: "host-a".to_owned(),
            binding: binding.to_owned(),
            base_url: "http://127.0.0.1:9/api/v1".to_owned(),
            credential: Some(Credential::Bearer(secret.to_owned())),
            credential_epoch: epoch,
        }
    }

    #[test]
    fn fingerprint_names_the_binding_and_not_the_secret() {
        let secret = "fingerprint-secret-value";
        let fp = fingerprint(std::slice::from_ref(&target("binding-b", 2, secret)));
        assert!(fp.contains("host-a\nbinding-b\n2"), "{fp}");
        assert!(fp.contains("http://127.0.0.1:9/api/v1"), "{fp}");
        assert!(!fp.contains(secret), "{fp}");
        let other = fingerprint(&[
            target("binding-b", 2, secret),
            target("binding-a", 2, "other-secret-value"),
        ]);
        let same_shape = fingerprint(&[
            target("binding-a", 2, "other-secret-value"),
            target("binding-b", 2, secret),
        ]);
        assert_eq!(other, same_shape);
        assert_ne!(
            fingerprint(std::slice::from_ref(&target("binding-a", 2, secret))),
            fingerprint(std::slice::from_ref(&target("binding-b", 2, secret)))
        );
    }

    // #1675 (READONLY r12 §0.3): the receiving side ignores what a later
    // server adds to ready and capabilities. These lock the probe's reading
    // as it is: WeKnora#146 adds `operations.candidate` and write operation
    // names to `supported_operations`, and a build already shipped must judge
    // them as it judges today.

    fn ready(operations: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "protocol_version": 1, "request_id": "req-1", "generation": 1,
            "live": true, "operations": operations, "x_future": {"a": 1},
        }))
        .unwrap()
    }

    #[test]
    fn ready_ignores_a_candidate_key_and_unknown_states_outside_search_and_read() {
        let base = json!({"search": {"state": "ready"}, "read": {"state": "ready"}, "ingest": {"state": "disabled"}});
        assert!(ready_ok(&ready(base.clone()), "req-1"));
        for candidate in [
            json!({"state": "ready"}),
            json!({"state": "disabled", "reason": "x"}),
            json!({"state": "warming_up"}),
            json!("not an object"),
        ] {
            let mut ops = base.clone();
            ops["candidate"] = candidate.clone();
            ops["pack"] = json!({"state": "not_ready"});
            assert!(ready_ok(&ready(ops), "req-1"), "candidate = {candidate}");
        }
        // `ingest` is not looked at, whatever it says or whether it is there.
        let mut ops = base.clone();
        ops["ingest"] = json!({"state": "ready"});
        assert!(ready_ok(&ready(ops.clone()), "req-1"));
        ops.as_object_mut().unwrap().remove("ingest");
        assert!(ready_ok(&ready(ops), "req-1"));
    }

    #[test]
    fn ready_reads_an_unknown_state_of_search_or_read_as_not_ready() {
        for (name, state) in [
            ("search", "warming_up"),
            ("read", "ready_soon"),
            ("search", "READY"),
        ] {
            let mut ops = json!({"search": {"state": "ready"}, "read": {"state": "ready"}, "ingest": {"state": "disabled"}});
            ops[name]["state"] = json!(state);
            ops["candidate"] = json!({"state": "ready"});
            assert!(!ready_ok(&ready(ops), "req-1"), "{name} = {state}");
        }
    }

    fn capabilities(agent: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"code": 0, "data": {"x_upstream": true, "integration": {"agent24_knowledge": agent}}})).unwrap()
    }

    fn declaration(operations: Value) -> Value {
        json!({
            "protocol_versions": [1], "mode": "integration", "supported_operations": operations,
            "server_version": "0.9.0",
            "limits": {"max_results": 10, "max_concurrency": 2, "max_deadline_ms": 1000, "max_read_bytes": 4096, "max_page_limit": 5, "max_snippet_bytes": 512},
        })
    }

    #[test]
    fn capabilities_with_write_operations_and_unknown_keys_judge_as_today() {
        let writes = [
            "candidate.propose",
            "candidate.review",
            "candidate.approve",
            "entry.withdraw",
        ];
        let mut operations = vec![json!("search"), json!("read")];
        operations.extend(writes.iter().map(|op| json!(op)));
        let mut agent = declaration(Value::Array(operations));
        agent["write_limits"] = json!({"max_candidates": 3});
        agent["limits"]["max_candidate_bytes"] = json!(1024);
        let judged = judge_response(200, &capabilities(agent));
        assert_eq!(judged.state, ComponentState::Ready);
        assert!(judged.missing.is_empty(), "{:?}", judged.missing);
        assert_eq!(judged.reason, None);
    }

    #[test]
    fn a_write_operation_name_never_stands_in_for_search_or_read() {
        let agent = declaration(json!([
            "search",
            "candidate.propose",
            "kb_candidate_propose",
            "read_all"
        ]));
        let judged = judge_response(200, &capabilities(agent));
        assert_eq!(judged.state, ComponentState::Degraded);
        assert_eq!(judged.missing, vec!["read".to_owned()]);
        assert_eq!(judged.reason.as_deref(), Some("missing_capability:read"));
    }
}
