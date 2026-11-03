//! Knowledge on `GET /api/v1/components` (ADR-038 C5).
//!
//! A configured binding is a remote service (D5): this probes `GET /system/capabilities`
//! once per cache window and maps the answer.
//!
//! - no endpoint configured → `stopped` (`not_configured`)
//! - TCP/TLS/timeout, or a non-2xx answer → `failed`, reason is a fixed code
//! - an envelope `code` other than success (`0` / `"ok"`) → `failed`
//!   (`upstream_<code>`), and `data` is not read as a declaration
//! - declaration missing, a required field absent (`protocol_versions`, `mode`,
//!   `supported_operations`, `server_version`, `limits` — READONLY R9.2), `mode`
//!   is not `integration`, or major `1` is absent (READONLY R9.4, D3) →
//!   `incompatible` (`component_contract_mismatch`)
//! - major `1` and `integration`, but `search` or `read` is absent (READONLY R9.2;
//!   OPS-v1 does not add a required operation yet) → `degraded`
//!   (`missing_capability:<name>`)
//! - otherwise → `ready`
//!
//! The client tolerates unknown fields and unknown operation names (D3: minor is
//! additive). The server's `msg` and body text never leave this module; only
//! codes do. `server_version` is copied only when it is a short token.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent24_components::ComponentState;
use agent24_knowledge::{
    Credential, HttpConfigError, HttpTransport, HttpTransportConfig, KnowledgeOperation,
    KnowledgeTransport, TransportError,
};
use serde_json::{Map, Value, json};
use tokio::sync::watch;

/// How long a probe answer is reused. `GET /api/v1/components` must not hit the
/// remote on every poll.
pub const KNOWLEDGE_PROBE_TTL: Duration = Duration::from_secs(30);

/// Bound on one capabilities exchange, connect included (D6).
pub const KNOWLEDGE_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// READONLY R3.2: the only contract major this build speaks.
const EXPECTED_PROTOCOL_MAJOR: u64 = 1;

/// READONLY R9.2 operations this build requires. Alphabetical, so the reason
/// names a stable one when several are missing.
const REQUIRED_OPERATIONS: [&str; 2] = ["read", "search"];

const MAX_CAPABILITIES_BYTES: usize = 64 * 1024;

/// One configured endpoint to probe. `credential_epoch` changes when the
/// credential changes so a cached answer is not reused under another secret.
/// The epoch is not a secret; the credential is.
pub(crate) struct ProbeTarget {
    pub base_url: String,
    pub credential: Option<Credential>,
    pub credential_epoch: u64,
}

/// What `GET /api/v1/components` shows for Knowledge.
#[derive(Clone, Debug)]
pub(crate) struct ProbeView {
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

struct CacheEntry {
    fingerprint: String,
    at: Instant,
    view: ProbeView,
}

#[derive(Default)]
struct CacheInner {
    entry: Option<CacheEntry>,
    flights: HashMap<String, watch::Sender<Option<ProbeView>>>,
}

/// Shared by every `GET /api/v1/components`. One in-flight probe per endpoint
/// set; callers that arrive while it runs wait for that answer. A cancelled
/// leader only removes its flight. It does not publish a failure; waiters
/// enter `assess` again.
#[derive(Default)]
pub(crate) struct KnowledgeProbeCache {
    inner: Mutex<CacheInner>,
}

struct FlightGuard<'a> {
    cache: &'a KnowledgeProbeCache,
    fingerprint: String,
    tx: Option<watch::Sender<Option<ProbeView>>>,
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
    pub(crate) async fn assess(&self, targets: &[ProbeTarget], now: Instant) -> ProbeView {
        if targets.is_empty() {
            return not_configured();
        }
        let fingerprint = fingerprint(targets);
        loop {
            enum Role {
                Hit(ProbeView),
                Wait(watch::Receiver<Option<ProbeView>>),
                Lead(watch::Sender<Option<ProbeView>>),
            }
            let role = {
                let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(entry) = &guard.entry
                    && entry.fingerprint == fingerprint
                    && now.saturating_duration_since(entry.at) < KNOWLEDGE_PROBE_TTL
                {
                    Role::Hit(entry.view.clone())
                } else if let Some(tx) = guard.flights.get(&fingerprint) {
                    Role::Wait(tx.subscribe())
                } else {
                    let (tx, _rx) = watch::channel(None);
                    guard.flights.insert(fingerprint.clone(), tx.clone());
                    Role::Lead(tx)
                }
            };
            match role {
                Role::Hit(view) => return view,
                Role::Wait(rx) => {
                    if let Some(view) = wait_flight(rx).await {
                        return view;
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
                    let view = probe_all(targets).await;
                    {
                        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
                        guard.entry = Some(CacheEntry {
                            fingerprint: fingerprint.clone(),
                            at: now,
                            view: view.clone(),
                        });
                        guard.flights.remove(&fingerprint);
                    }
                    if let Some(tx) = flight.tx.take() {
                        let _ = tx.send(Some(view.clone()));
                    }
                    flight.finished = true;
                    return view;
                }
            }
        }
    }

    /// Receivers parked on the current flight. Tests use this to abort the
    /// leader only after a waiter has subscribed.
    #[cfg(test)]
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
async fn wait_flight(mut rx: watch::Receiver<Option<ProbeView>>) -> Option<ProbeView> {
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
        .map(|target| format!("{}\n{}", target.base_url, target.credential_epoch))
        .collect();
    lines.sort();
    lines.join("\n")
}

async fn probe_all(targets: &[ProbeTarget]) -> ProbeView {
    let mut parts = Vec::with_capacity(targets.len());
    for target in targets {
        parts.push(fetch(target).await);
    }
    fold(&parts)
}

fn fold(parts: &[Judged]) -> ProbeView {
    let Some(worst) = parts.iter().map(Judged::rank).max() else {
        return not_configured();
    };
    if worst == 1 {
        return union_degraded(parts);
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
    }
    let missing: Vec<String> = missing.into_iter().collect();
    let reason = missing
        .first()
        .map(|name| format!("missing_capability:{name}"));
    Judged {
        state: ComponentState::Degraded,
        reason,
        version,
        missing,
        contract,
        probed_at,
    }
    .view()
}

async fn fetch(target: &ProbeTarget) -> Judged {
    let built = match &target.credential {
        Some(credential) => HttpTransport::new(HttpTransportConfig {
            base_url: target.base_url.clone(),
            credential: credential.clone(),
            max_response_bytes: MAX_CAPABILITIES_BYTES,
            connect_timeout: KNOWLEDGE_PROBE_TIMEOUT,
        }),
        None => HttpTransport::anonymous(
            target.base_url.clone(),
            MAX_CAPABILITIES_BYTES,
            KNOWLEDGE_PROBE_TIMEOUT,
        ),
    };
    let transport = match built {
        Ok(transport) => transport,
        Err(err) => return judge_config(err),
    };
    match transport
        .send(
            KnowledgeOperation::Capabilities,
            Vec::new(),
            KNOWLEDGE_PROBE_TIMEOUT,
        )
        .await
    {
        Ok(response) => judge_response(response.status, &response.body),
        Err(err) => judge_transport(err),
    }
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
    if let Some(code) = upstream_code(&value) {
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
    // `server_version`. Absent → incompatible; the wrong JSON type → malformed.
    match agent.get("limits") {
        Some(Value::Object(_)) => {}
        Some(_) => return Err(ParseError::Malformed),
        None => return Err(ParseError::NoDeclaration),
    }
    Ok(Decl {
        majors,
        mode,
        operations,
        server_version,
    })
}

/// A numeric or token `code` other than success. Free text becomes
/// `upstream_error` so the server's sentence is not copied.
fn upstream_code(value: &Value) -> Option<String> {
    let code = value.get("code")?;
    match code {
        Value::Number(n) => {
            let n = n.as_i64()?;
            (n != 0).then(|| format!("upstream_{n}"))
        }
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
