//! Process-bound daily tokens and single-use D4 elevation grants.
use super::presence::*;
use super::{
    token::{CapabilityToken, digest, random_capability_id},
    types::{FirstPartyClaims, FirstPartyTier},
    *,
};
use crate::enroll_api::*;
use async_trait::async_trait;
use axum::http::Method;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub const TOKEN_TTL: u64 = 3600;
const GRANT_TTL: u64 = 60;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub operation: Operation,
    pub method: String,
    pub template: String,
    pub path: String,
    pub body_sha256: String,
}
struct Grant {
    challenge_id: String,
    token_id: String,
    target: Target,
    expires_at: Instant,
}
/// A presence summary the daemon derived from the rejected request's own
/// body, kept until the matching `elevate` arrives (one per token).
struct RequestSummary {
    target: Target,
    summary: String,
    expires_at: Instant,
}

pub struct FirstPartyAuthority {
    store: CapabilityStore,
    provider: Mutex<Option<Arc<dyn PresenceProvider>>>,
    grants: Mutex<HashMap<[u8; 32], Grant>>,
    request_summaries: Mutex<HashMap<String, RequestSummary>>,
    module_surfaces: Mutex<Arc<Vec<super::delegation::ModuleSurface>>>,
}
impl FirstPartyAuthority {
    pub fn new(store: CapabilityStore) -> Self {
        Self {
            store,
            provider: Mutex::new(None),
            grants: Mutex::new(HashMap::new()),
            request_summaries: Mutex::new(HashMap::new()),
            module_surfaces: Mutex::new(Arc::new(Vec::new())),
        }
    }
    /// Derive display context from the rejected HTTP request itself. The
    /// frozen UDS frame carries only a hash, never the body or a trusted name.
    pub fn remember_attached_request(
        &self,
        token_id: &str,
        bytes: &[u8],
        registrations: &[agent24_protocol::AttachedView],
    ) {
        let Ok(body) = serde_json::from_slice::<agent24_protocol::AttachedAddRequest>(bytes) else {
            return;
        };
        let Ok(manifest) = agent24_domain::DomainOsManifest::from_yaml(&body.manifest) else {
            return;
        };
        let summary = format!(
            "register attached module '{}'; {}; allow_relax: {}; model_access: {}; kernel_capabilities: [{}]; host_commands: [{}]",
            manifest.name(),
            if registrations.iter().any(|r| r.name == manifest.name()) {
                "replace existing registration"
            } else {
                "new registration"
            },
            body.allow_relax,
            manifest.model_access().as_str(),
            manifest
                .kernel_capabilities()
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            manifest.host_commands().join(", "),
        );
        self.remember(
            token_id,
            Target {
                operation: Operation::ModuleAdmin,
                method: "POST".into(),
                template: "/api/v1/attached".into(),
                path: "/api/v1/attached".into(),
                body_sha256: body_digest(bytes),
            },
            summary,
        );
    }
    /// I-1: derive the presence summary for an iDoris install or connect
    /// from the rejected request's body. Any other template, or a body the
    /// handler would refuse, leaves nothing to approve.
    pub fn remember_idoris_request(&self, token_id: &str, template: &str, bytes: &[u8]) {
        let summary = match template {
            "/api/v1/idoris/install" => {
                let Ok(body) =
                    serde_json::from_slice::<agent24d::idoris_host::InstallRequest>(bytes)
                else {
                    return;
                };
                format!(
                    "install the locked iDoris release (downloads and runs software after integrity and signature checks); consent: {}",
                    body.consent
                )
            }
            "/api/v1/idoris/connect" => {
                let Ok(body) =
                    serde_json::from_slice::<agent24d::idoris_host::ConnectRequest>(bytes)
                else {
                    return;
                };
                let (Some(endpoint), Some(daily), Some(deep)) = (
                    shown(&body.endpoint),
                    shown(&body.daily_model),
                    body.deep_model
                        .as_deref()
                        .map_or_else(|| Some("none".to_owned()), shown),
                ) else {
                    return;
                };
                format!(
                    "connect iDoris to oMLX at {endpoint}; daily model: {daily}; deep model: {deep}; share the local oMLX credential with iDoris: {}; save oMLX credential: {}; replace existing connection: {}; core reserved bytes: {}",
                    body.consent.as_ref().is_some_and(|c| c.share_omlx_key),
                    body.save_omlx_key,
                    body.replace,
                    body.core_reserved_bytes
                        .map_or_else(|| "daemon allocation".to_owned(), |n| n.to_string()),
                )
            }
            _ => return,
        };
        self.remember(
            token_id,
            Target {
                operation: Operation::IdorisLifecycle,
                method: "POST".into(),
                template: template.into(),
                path: template.into(),
                body_sha256: body_digest(bytes),
            },
            summary,
        );
    }
    fn remember(&self, token_id: &str, target: Target, summary: String) {
        // Defence in depth only: this caps `details`, not the dialog string.
        // The authoritative check is in `elevate()`, which measures the full
        // "{identifier} (pid {pid}) wants to: {details}" the desktop clips.
        if summary.encode_utf16().count() > SUMMARY_MAX_UTF16 {
            return;
        }
        let Ok(mut summaries) = self.request_summaries.lock() else {
            return;
        };
        let now = Instant::now();
        summaries.retain(|_, s| s.expires_at > now);
        if summaries.len() >= 128
            && let Some(oldest) = summaries
                .iter()
                .min_by_key(|(_, s)| s.expires_at)
                .map(|(id, _)| id.clone())
        {
            summaries.remove(&oldest);
        }
        summaries.insert(
            token_id.to_owned(),
            RequestSummary {
                target,
                summary,
                expires_at: now + Duration::from_secs(GRANT_TTL),
            },
        );
    }
    pub fn set_module_surfaces(&self, surfaces: Arc<Vec<super::delegation::ModuleSurface>>) {
        if let Ok(mut slot) = self.module_surfaces.lock() {
            *slot = surfaces;
        }
    }
    fn target_summary(&self, token_id: &str, target: &Target) -> Option<String> {
        if matches!(
            target.template.as_str(),
            "/api/v1/attached" | "/api/v1/idoris/install" | "/api/v1/idoris/connect"
        ) {
            return self
                .request_summaries
                .lock()
                .ok()?
                .get(token_id)
                .filter(|s| s.expires_at > Instant::now() && s.target == *target)
                .map(|s| s.summary.clone());
        }
        if target.body_sha256 != body_digest(b"") {
            return None;
        }
        match target.template.as_str() {
            "/api/v1/idoris/start" => return Some("start managed iDoris".to_owned()),
            "/api/v1/idoris/stop" => return Some("stop managed iDoris".to_owned()),
            "/api/v1/idoris/omlx/candidates" => {
                return Some(
                    "list local oMLX endpoints, their identity, authentication and model IDs"
                        .to_owned(),
                );
            }
            "/api/v1/idoris/operations/{id}/cancel" => {
                let id = target
                    .path
                    .strip_prefix("/api/v1/idoris/operations/")?
                    .strip_suffix("/cancel")?;
                return Some(format!("cancel iDoris operation {}", shown(id)?));
            }
            _ => {}
        }
        let name = target
            .path
            .strip_prefix("/api/v1/os/")?
            .strip_suffix("/delegation")?;
        let surfaces = self.module_surfaces.lock().ok()?;
        let surface = surfaces.iter().find(|s| s.name == name)?;
        if surface.routes_sha256.len() != 64
            || !surface.routes_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        Some(format!(
            "delegate module '{}'; version: {}; namespace: {}; routes_sha256: {}",
            surface.name,
            surface.version,
            surface.namespace,
            &surface.routes_sha256[..12]
        ))
    }
    pub fn set_provider(&self, provider: Option<Arc<dyn PresenceProvider>>) {
        if let Ok(mut slot) = self.provider.lock() {
            *slot = provider;
        }
    }
    fn live_peer(&self, peer: &EnrolledPeer, id: &str) -> Result<(), EnrollErrorCode> {
        let mut state = self
            .store
            .shared
            .state
            .lock()
            .map_err(|_| EnrollErrorCode::Internal)?;
        let key = state
            .ids
            .get(id)
            .copied()
            .ok_or(EnrollErrorCode::NotEnrolled)?;
        let record = state
            .records
            .get(&key)
            .ok_or(EnrollErrorCode::NotEnrolled)?;
        if record.claims.first_party.as_ref().map(|c| &c.peer) != Some(peer) {
            return Err(EnrollErrorCode::NotEnrolled);
        }
        if record.revoked
            || record.expired(unix_now())
            || record.claims.daemon_generation != self.store.daemon_generation()
            || !peer_live(peer)
        {
            state.ids.remove(id);
            state.records.remove(&key);
            state.epoch = state.epoch.wrapping_add(1);
            let _ = self.store.shared.changed.send(state.epoch);
            return Err(EnrollErrorCode::NotEnrolled);
        }
        Ok(())
    }
    fn mint(&self, peer: &EnrolledPeer, old: Option<&str>) -> Result<IssuedToken, EnrollErrorCode> {
        if peer.generation != self.store.daemon_generation() {
            return Err(EnrollErrorCode::IdentityRejected);
        }
        let mut state = self
            .store
            .shared
            .state
            .lock()
            .map_err(|_| EnrollErrorCode::Internal)?;
        if let Some(id) = old {
            let digest = state
                .ids
                .get(id)
                .copied()
                .ok_or(EnrollErrorCode::NotEnrolled)?;
            let record = state
                .records
                .get(&digest)
                .ok_or(EnrollErrorCode::NotEnrolled)?;
            if record.revoked
                || record.expired(unix_now())
                || record.claims.first_party.as_ref().map(|c| &c.peer) != Some(peer)
            {
                return Err(EnrollErrorCode::NotEnrolled);
            }
        }
        if !peer_live(peer) {
            if let Some(id) = old
                && let Some(key) = state.ids.remove(id)
            {
                state.records.remove(&key);
                state.epoch = state.epoch.wrapping_add(1);
                let _ = self.store.shared.changed.send(state.epoch);
            }
            return Err(EnrollErrorCode::IdentityRejected);
        }
        let now = unix_now();
        let minted = self
            .store
            .insert_locked(
                &mut state,
                CapabilityClaims {
                    capability_id: random_capability_id(),
                    audience: Audience::FirstPartyShell,
                    first_party: Some(FirstPartyClaims {
                        peer: peer.clone(),
                        tier: FirstPartyTier::Daily,
                        deadline: Instant::now() + Duration::from_secs(TOKEN_TTL),
                    }),
                    workspace_id: None,
                    attachment_id: None,
                    principal_id: None,
                    daemon_generation: peer.generation.clone(),
                    host_generation: None,
                    sidecar_generation: None,
                    created_at: now,
                    expires_at: now + TOKEN_TTL,
                    // The presence channel answers this token's own elevation
                    // challenges: a first-party shell must never reach it,
                    // or an enrolled CLI could approve itself (#1447 F1).
                    // `presence_desktop::gate` is the second, independent layer.
                    allowed_operations: policies()
                        .iter()
                        .map(|p| p.operation)
                        .filter(|op| !op.is_presence_channel())
                        .chain([Operation::ModuleRoute])
                        .collect(),
                },
            )
            .map_err(|_| EnrollErrorCode::Internal)?;
        if let Some(id) = old
            && let Some(digest) = state.ids.remove(id)
        {
            state.records.remove(&digest);
            state.epoch = state.epoch.wrapping_add(1);
            let _ = self.store.shared.changed.send(state.epoch);
        }
        let (secret, token_id, _) = minted.into_bearer_parts();
        tracing::info!(
            event = "first_party_issued",
            token_id,
            pid = peer.pid,
            identifier = peer.identity.identifier,
            generation = peer.generation,
            renewed = old.is_some()
        );
        Ok(IssuedToken {
            token_id,
            secret: Zeroizing::new(secret),
            expires_in: TOKEN_TTL,
        })
    }
    pub fn consume(&self, secret: &str, token_id: &str, target: &Target, now: UnixSeconds) -> bool {
        let Ok(token) = CapabilityToken::parse_bearer(secret) else {
            return false;
        };
        // Linearize token liveness and grant consumption with revocation and
        // renewal. HTTP's earlier check alone leaves a revocation race.
        let Ok(mut state) = self.store.shared.state.lock() else {
            return false;
        };
        let Some(record) = state
            .ids
            .get(token_id)
            .and_then(|key| state.records.get(key))
        else {
            return false;
        };
        if record
            .claims
            .first_party
            .as_ref()
            .is_some_and(|c| !peer_live(&c.peer))
        {
            if let Some(key) = state.ids.remove(token_id) {
                state.records.remove(&key);
            }
            state.epoch = state.epoch.wrapping_add(1);
            let _ = self.store.shared.changed.send(state.epoch);
            return false;
        }
        if record.claims.audience != Audience::FirstPartyShell
            || record.claims.first_party.is_none()
            || self
                .store
                .validate_record(
                    record,
                    target.operation,
                    &Resource::global(),
                    now,
                    state.epoch,
                )
                .is_err()
        {
            return false;
        }
        let Ok(mut grants) = self.grants.lock() else {
            return false;
        };
        grants.retain(|_, grant| grant.expires_at > Instant::now());
        let key = digest(&token);
        let valid = grants
            .get(&key)
            .is_some_and(|grant| grant.token_id == token_id && grant.target == *target);
        if valid && let Some(grant) = grants.remove(&key) {
            tracing::info!(event = "elevation_consumed", token_id, challenge_id = grant.challenge_id, operation = ?target.operation, method = target.method, path = target.path, body_sha256 = target.body_sha256);
        } else if !valid {
            tracing::info!(event = "elevation_rejected", token_id, operation = ?target.operation, reason = "invalid_or_expired");
        }
        valid
    }
}
pub(crate) fn peer_live(peer: &EnrolledPeer) -> bool {
    let audit_live = match peer.identity.audit_token {
        Some(token) => {
            token.pid() == peer.pid
                && token.euid() == peer.uid
                && agent24_os_proc::audit_token_is_live(token)
        }
        None => cfg!(test),
    };
    audit_live
        && agent24_os_proc::is_alive(peer.pid)
        && agent24_os_proc::start_time(peer.pid)
            .is_ok_and(|s| s.sec == peer.start.sec && s.usec == peer.start.usec)
}
/// Resolve the actual path against the same fixed table used by HTTP auth.
pub fn resolve_target(request: &ElevateRequest) -> Result<Target, EnrollErrorCode> {
    let method: Method = request
        .method
        .parse()
        .map_err(|_| EnrollErrorCode::UnknownRoute)?;
    if !request.path.starts_with('/') || request.path.contains(['?', '#']) {
        return Err(EnrollErrorCode::UnknownRoute);
    }
    let policy = policies()
        .iter()
        .find(|p| p.method == method.as_str() && template_matches(p.template, &request.path))
        .ok_or(EnrollErrorCode::UnknownRoute)?;
    // Full CORE-2 will enable elevated operations individually; each must first
    // have a daemon-generated summary derived from its actual request body.
    let supported = (policy.operation == Operation::ModuleAdmin
        && method == Method::POST
        && matches!(
            policy.template,
            "/api/v1/attached" | "/api/v1/os/{name}/delegation"
        ))
        // I-1: the iDoris lifecycle, each with a daemon-derived summary.
        || (policy.operation == Operation::IdorisLifecycle
            && matches!(
                (method.as_str(), policy.template),
                ("POST", "/api/v1/idoris/install")
                    | ("POST", "/api/v1/idoris/connect")
                    | ("POST", "/api/v1/idoris/start")
                    | ("POST", "/api/v1/idoris/stop")
                    | ("POST", "/api/v1/idoris/operations/{id}/cancel")
                    | ("GET", "/api/v1/idoris/omlx/candidates")
            ));
    if policy.level != AssuranceLevel::Elevated || !supported {
        return Err(EnrollErrorCode::NotElevatedOperation);
    }
    Ok(Target {
        operation: policy.operation,
        method: method.to_string(),
        template: policy.template.to_owned(),
        path: request.path.clone(),
        body_sha256: request.body_sha256.clone(),
    })
}
fn template_matches(template: &str, path: &str) -> bool {
    let a: Vec<_> = template.split('/').collect();
    let b: Vec<_> = path.split('/').collect();
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| *a == b || (a.starts_with('{') && a.ends_with('}') && !b.is_empty()))
}
#[async_trait]
impl EnrollAuthority for FirstPartyAuthority {
    fn issue(&self, peer: &EnrolledPeer) -> Result<IssuedToken, EnrollErrorCode> {
        self.mint(peer, None)
    }
    fn revoke(&self, peer: &EnrolledPeer, id: &str) {
        if let Ok(mut state) = self.store.shared.state.lock()
            && let Some(key) = state.ids.get(id).copied()
            && state
                .records
                .get(&key)
                .and_then(|r| r.claims.first_party.as_ref())
                .is_some_and(|c| &c.peer == peer)
        {
            state.ids.remove(id);
            state.records.remove(&key);
            state.epoch = state.epoch.wrapping_add(1);
            let _ = self.store.shared.changed.send(state.epoch);
        }
    }
    fn renew(&self, peer: &EnrolledPeer, id: &str) -> Result<IssuedToken, EnrollErrorCode> {
        self.mint(peer, Some(id))
    }
    async fn elevate(
        &self,
        peer: &EnrolledPeer,
        id: &str,
        request: ElevateRequest,
        pending: tokio::sync::oneshot::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<ElevationGrant, EnrollErrorCode> {
        self.live_peer(peer, id)?;
        let target = resolve_target(&request)?;
        let challenge_id = random_capability_id()[..16].to_owned();
        let expires_at = unix_now() + GRANT_TTL;
        let deadline = Instant::now() + Duration::from_secs(GRANT_TTL);
        let details = self
            .target_summary(id, &target)
            .ok_or(EnrollErrorCode::NotElevatedOperation)?;
        let summary = format!(
            "{} (pid {}) wants to: {}",
            peer.identity.identifier, peer.pid, details
        );
        // The desktop clips this whole string, not just `details`: refuse
        // before anything is sent to `pending` or shown, so no tail is hidden.
        if summary.encode_utf16().count() > SUMMARY_MAX_UTF16 {
            return Err(EnrollErrorCode::NotElevatedOperation);
        }
        let req = PresenceRequest {
            challenge_id: challenge_id.clone(),
            operation: target.operation,
            method: target.method.clone(),
            path: target.path.clone(),
            summary,
            requester: RequesterIdentity {
                identifier: peer.identity.identifier.clone(),
                pid: peer.pid,
            },
            expires_at,
        };
        let provider = self
            .provider
            .lock()
            .map_err(|_| EnrollErrorCode::Internal)?
            .clone()
            .unwrap_or_else(|| Arc::new(NoPresenceProvider));
        let _ = pending.send(challenge_id.clone());
        // Yield once so the endpoint always sends pending before a fast outcome.
        tokio::task::yield_now().await;
        let outcome = tokio::select! {
            _ = cancel.cancelled() => return Err(EnrollErrorCode::PresenceTimeout),
            result = tokio::time::timeout(Duration::from_secs(GRANT_TTL), provider.confirm(req, cancel.clone())) => result.unwrap_or(PresenceOutcome::TimedOut),
        };
        tracing::info!(event = "presence_outcome", token_id = id, challenge_id, operation = ?target.operation, outcome = ?outcome);
        match outcome {
            PresenceOutcome::Allowed => {}
            PresenceOutcome::Denied => return Err(EnrollErrorCode::PresenceDenied),
            PresenceOutcome::Unavailable => return Err(EnrollErrorCode::PresenceUnavailable),
            PresenceOutcome::TimedOut => return Err(EnrollErrorCode::PresenceTimeout),
        }
        if Instant::now() >= deadline {
            return Err(EnrollErrorCode::PresenceTimeout);
        }
        self.live_peer(peer, id)?;
        let secret = CapabilityToken::random();
        let now = Instant::now();
        let mut grants = self.grants.lock().map_err(|_| EnrollErrorCode::Internal)?;
        grants.retain(|_, g| g.expires_at > now);
        grants.insert(
            digest(&secret),
            Grant {
                challenge_id: challenge_id.clone(),
                token_id: id.to_owned(),
                target,
                expires_at: now + Duration::from_secs(GRANT_TTL),
            },
        );
        Ok(ElevationGrant {
            challenge_id,
            secret: Zeroizing::new(secret.bearer()),
            expires_in: GRANT_TTL,
        })
    }
}

/// The longest request-supplied value a presence summary shows.
const SHOWN_MAX_CHARS: usize = 120;

/// The longest whole dialog summary, in UTF-16 code units. MUST match the
/// desktop `clip()` limit in `apps/desktop/src/main/presence-poller.ts`: a
/// longer string would be clipped there and its tail hidden from the user.
/// The authoritative check is in `elevate()`, on the full string including
/// the "{identifier} (pid {pid}) wants to: " prefix and for every summary
/// source (including delegation); `remember()` repeats it on the details
/// alone as redundancy.
const SUMMARY_MAX_UTF16: usize = 500;

/// A request-supplied string as a presence summary shows it, or `None` when
/// it cannot be shown faithfully, which leaves nothing to approve. The value
/// comes back as a double-quoted, escaped JSON string so its boundaries are
/// unambiguous even if it contains `; ` or other summary punctuation.
/// Refused rather than dropped or cut: control, format (bidi overrides,
/// zero-width), line and paragraph separator characters; any whitespace other
/// than a plain ASCII space; variation selectors; and the invisible filler
/// letters (U+3164, U+2800, U+115F, U+1160, U+FFA0). A value over
/// `SHOWN_MAX_CHARS` is refused. The length of the *whole* summary is capped
/// separately (`SUMMARY_MAX_UTF16`, enforced in `elevate`); this function
/// alone does not stop a long summary from being clipped by the dialog.
fn shown(value: &str) -> Option<String> {
    use unicode_general_category::{GeneralCategory as G, get_general_category};
    let hidden = |c: char| {
        matches!(
            get_general_category(c),
            G::Control | G::Format | G::LineSeparator | G::ParagraphSeparator
        ) || (c.is_whitespace() && c != ' ')
            || matches!(
                c,
                '\u{3164}' | '\u{2800}' | '\u{115F}' | '\u{1160}' | '\u{FFA0}' | '\u{FE00}'
                    ..='\u{FE0F}'
            )
    };
    if value.chars().count() > SHOWN_MAX_CHARS || value.chars().any(hidden) {
        return None;
    }
    serde_json::to_string(value).ok()
}
pub fn body_digest(body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
pub(crate) fn test_peer(generation: &str) -> EnrolledPeer {
    let pid = std::process::id() as i32;
    let start =
        agent24_os_proc::start_time(pid).unwrap_or(agent24_os_proc::ProcStart { sec: 0, usec: 0 });
    EnrolledPeer {
        conn_id: "original-connection".into(),
        uid: 0,
        pid,
        start: ProcStart {
            sec: start.sec,
            usec: start.usec,
        },
        identity: VerifiedIdentity {
            kind: IdentityKind::MacosSecCode,
            identifier: "agent24".into(),
            team_id: "HYAP444FN9".into(),
            executable: std::env::current_exe().unwrap_or_default(),
            audit_token: None,
        },
        generation: generation.into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    struct Provider(PresenceOutcome);
    #[async_trait]
    impl PresenceProvider for Provider {
        async fn confirm(
            &self,
            req: PresenceRequest,
            _cancel: CancellationToken,
        ) -> PresenceOutcome {
            assert!(req.summary.contains("register attached module"));
            assert!(!req.summary.contains("CLIENT SECRET"));
            self.0
        }
    }
    fn attached_body() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"manifest": "name: agentear\nversion: '1'\nroute_namespace: /api/v1/agentear\nevent_module: agentear\ndata_dir: ~/.agent24/os/agentear/\nimpl_kind: attached_process\n"})).unwrap()
    }
    fn request() -> ElevateRequest {
        ElevateRequest {
            method: "POST".into(),
            path: "/api/v1/attached".into(),
            body_sha256: body_digest(&attached_body()),
            client_summary: "CLIENT SECRET".into(),
        }
    }
    async fn grant(
        authority: &FirstPartyAuthority,
        peer: &EnrolledPeer,
        id: &str,
    ) -> ElevationGrant {
        authority.remember_attached_request(id, &attached_body(), &[]);
        let (tx, _rx) = tokio::sync::oneshot::channel();
        authority
            .elevate(peer, id, request(), tx, CancellationToken::new())
            .await
            .unwrap()
    }
    #[tokio::test(start_paused = true)]
    async fn daily_token_ttl_ignores_wall_clock_jumps() {
        let store = CapabilityStore::new("g");
        let authority = FirstPartyAuthority::new(store.clone());
        let token = authority.issue(&test_peer("g")).unwrap();
        assert!(
            store
                .require_live_bearer(&token.secret, unix_now() + TOKEN_TTL * 100)
                .is_ok()
        );
        assert!(
            store
                .validate_bearer(
                    &token.secret,
                    Operation::ModelsRead,
                    &Resource::global(),
                    unix_now() + TOKEN_TTL * 100
                )
                .is_ok()
        );
        tokio::time::advance(Duration::from_secs(TOKEN_TTL)).await;
        assert!(store.require_live_bearer(&token.secret, 0).is_err());
        assert!(
            store
                .validate_bearer(&token.secret, Operation::ModelsRead, &Resource::global(), 0)
                .is_err()
        );
        assert!(authority.renew(&test_peer("g"), &token.token_id).is_err());
    }
    #[test]
    fn attached_summary_discloses_relax_and_registration_kind() {
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        let mut body: serde_json::Value = serde_json::from_slice(&attached_body()).unwrap();
        for allow in [false, true] {
            body["allow_relax"] = allow.into();
            let bytes = serde_json::to_vec(&body).unwrap();
            authority.remember_attached_request("id", &bytes, &[]);
            let mut req = request();
            req.body_sha256 = body_digest(&bytes);
            let summary = authority
                .target_summary("id", &resolve_target(&req).unwrap())
                .unwrap();
            assert!(
                summary.contains(&format!("allow_relax: {allow}")),
                "{summary}"
            );
            assert!(summary.contains("new registration"), "{summary}");
            let registered = agent24_protocol::AttachedView {
                name: "agentear".into(),
                manifest_digest: String::new(),
                token_id: String::new(),
                created_at: String::new(),
                attach_status: "detached".into(),
                generation: None,
            };
            authority.remember_attached_request("id", &bytes, &[registered]);
            let summary = authority
                .target_summary("id", &resolve_target(&req).unwrap())
                .unwrap();
            assert!(
                summary.contains("replace existing registration"),
                "{summary}"
            );
            assert!(
                summary.contains(&format!("allow_relax: {allow}")),
                "{summary}"
            );
        }
    }
    #[tokio::test(start_paused = true)]
    async fn daily_claims_ttl_scope_and_atomic_renewal() {
        let store = CapabilityStore::new("g");
        let authority = FirstPartyAuthority::new(store.clone());
        let peer = test_peer("g");
        let old = authority.issue(&peer).unwrap();
        let auth = store
            .validate_bearer(
                &old.secret,
                Operation::ModelsRead,
                &Resource::global(),
                unix_now(),
            )
            .unwrap();
        assert_eq!(auth.claims.audience, Audience::FirstPartyShell);
        assert_eq!(
            auth.claims.first_party.as_ref().unwrap().tier,
            FirstPartyTier::Daily
        );
        assert_eq!(auth.claims.expires_at - auth.claims.created_at, 3600);
        assert!(
            store
                .validate_bearer(
                    &old.secret,
                    Operation::ModuleRead,
                    &Resource::workspace("other"),
                    unix_now()
                )
                .is_err()
        );
        let mut other = peer.clone();
        other.conn_id = "different".into();
        assert!(authority.renew(&other, &old.token_id).is_err());
        let new = authority.renew(&peer, &old.token_id).unwrap();
        assert!(store.require_live_bearer(&old.secret, unix_now()).is_err());
        assert!(store.require_live_bearer(&new.secret, unix_now()).is_ok());
        assert!(authority.renew(&peer, &old.token_id).is_err());
    }
    #[test]
    fn invalid_process_on_uds_renewal_deletes_the_token() {
        let store = CapabilityStore::new("g");
        let authority = FirstPartyAuthority::new(store.clone());
        let mut peer = test_peer("g");
        let token = authority.issue(&peer).unwrap();
        peer.start.sec += 1;
        {
            let mut state = store.shared.state.lock().unwrap();
            let key = state.ids[&token.token_id];
            state
                .records
                .get_mut(&key)
                .unwrap()
                .claims
                .first_party
                .as_mut()
                .unwrap()
                .peer = peer.clone();
        }
        assert!(authority.renew(&peer, &token.token_id).is_err());
        assert!(
            !store
                .shared
                .state
                .lock()
                .unwrap()
                .ids
                .contains_key(&token.token_id)
        );
    }

    #[test]
    fn process_exit_or_reused_pid_deletes_the_token() {
        let store = CapabilityStore::new("g");
        let authority = FirstPartyAuthority::new(store.clone());
        let token = authority.issue(&test_peer("g")).unwrap();
        let parsed = CapabilityToken::parse_bearer(&token.secret).unwrap();
        {
            let mut state = store.shared.state.lock().unwrap();
            let record = state.records.get_mut(&digest(&parsed)).unwrap();
            record.claims.first_party.as_mut().unwrap().peer.start.sec += 1;
        }
        assert!(
            store
                .require_live_bearer(&token.secret, unix_now())
                .is_err()
        );
        assert!(
            !store
                .shared
                .state
                .lock()
                .unwrap()
                .ids
                .contains_key(&token.token_id)
        );
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut peer = test_peer("g");
        peer.pid = child.id() as i32;
        let start = agent24_os_proc::start_time(peer.pid).unwrap();
        peer.start = ProcStart {
            sec: start.sec,
            usec: start.usec,
        };
        let token = authority.issue(&peer).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            store
                .validate_bearer(
                    &token.secret,
                    Operation::ModelsRead,
                    &Resource::global(),
                    unix_now()
                )
                .is_err()
        );
        assert!(
            !store
                .shared
                .state
                .lock()
                .unwrap()
                .ids
                .contains_key(&token.token_id)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn grants_bind_every_target_field_expire_and_consume_once() {
        let store = CapabilityStore::new("g");
        let authority = Arc::new(FirstPartyAuthority::new(store));
        authority.set_provider(Some(Arc::new(Provider(PresenceOutcome::Allowed))));
        let peer = test_peer("g");
        let token = authority.issue(&peer).unwrap();
        let grant = grant(&authority, &peer, &token.token_id).await;
        let target = resolve_target(&request()).unwrap();
        for field in 0..5 {
            let mut wrong = target.clone();
            match field {
                0 => wrong.path.push('x'),
                1 => wrong.method = "DELETE".into(),
                2 => wrong.body_sha256 = body_digest(b"changed"),
                3 => wrong.template.push('x'),
                _ => wrong.operation = Operation::Shutdown,
            }
            assert!(!authority.consume(&grant.secret, &token.token_id, &wrong, unix_now()));
        }
        assert!(!authority.consume(&grant.secret, "other-token", &target, unix_now()));
        let a = authority.clone();
        let b = authority.clone();
        let secret = grant.secret.clone();
        let id = token.token_id.clone();
        let t = target.clone();
        let one = std::thread::spawn(move || a.consume(&secret, &id, &t, unix_now()));
        let secret = grant.secret.clone();
        let id = token.token_id.clone();
        let t = target.clone();
        let two = std::thread::spawn(move || b.consume(&secret, &id, &t, unix_now()));
        assert_ne!(one.join().unwrap(), two.join().unwrap());
        let expired = self::grant(&authority, &peer, &token.token_id).await;
        tokio::time::advance(Duration::from_secs(GRANT_TTL)).await;
        assert!(!authority.consume(&expired.secret, &token.token_id, &target, 0));
    }
    #[tokio::test(start_paused = true)]
    async fn grant_ttl_ignores_wall_clock_jumps() {
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        authority.set_provider(Some(Arc::new(Provider(PresenceOutcome::Allowed))));
        let peer = test_peer("g");
        let token = authority.issue(&peer).unwrap();
        let first = grant(&authority, &peer, &token.token_id).await;
        let target = resolve_target(&request()).unwrap();
        assert_eq!(first.expires_in, GRANT_TTL);
        assert!(authority.consume(
            &first.secret,
            &token.token_id,
            &target,
            unix_now() + TOKEN_TTL * 100
        ));
        let expired = grant(&authority, &peer, &token.token_id).await;
        tokio::time::advance(Duration::from_secs(GRANT_TTL)).await;
        assert!(!authority.consume(&expired.secret, &token.token_id, &target, 0));
    }
    #[tokio::test]
    async fn revoked_tokens_cannot_consume_grants() {
        let store = CapabilityStore::new("g");
        let authority = FirstPartyAuthority::new(store.clone());
        authority.set_provider(Some(Arc::new(Provider(PresenceOutcome::Allowed))));
        let peer = test_peer("g");
        let token = authority.issue(&peer).unwrap();
        let grant = grant(&authority, &peer, &token.token_id).await;
        assert!(store.revoke_by_id(&token.token_id));
        assert!(!authority.consume(
            &grant.secret,
            &token.token_id,
            &resolve_target(&request()).unwrap(),
            unix_now()
        ));
    }

    #[tokio::test]
    async fn independent_presence_failures_never_mint_a_grant() {
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        let peer = test_peer("g");
        let token = authority.issue(&peer).unwrap();
        authority.remember_attached_request(&token.token_id, &attached_body(), &[]);
        for (outcome, code) in [
            (PresenceOutcome::Denied, EnrollErrorCode::PresenceDenied),
            (
                PresenceOutcome::Unavailable,
                EnrollErrorCode::PresenceUnavailable,
            ),
            (PresenceOutcome::TimedOut, EnrollErrorCode::PresenceTimeout),
        ] {
            authority.set_provider(Some(Arc::new(Provider(outcome))));
            let (tx, _rx) = tokio::sync::oneshot::channel();
            assert_eq!(
                authority
                    .elevate(
                        &peer,
                        &token.token_id,
                        request(),
                        tx,
                        CancellationToken::new()
                    )
                    .await
                    .err(),
                Some(code)
            );
        }
        assert!(authority.grants.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn delegation_summary_requires_registered_routes_and_empty_body() {
        struct Capture(Mutex<Vec<PresenceRequest>>);
        #[async_trait]
        impl PresenceProvider for Capture {
            async fn confirm(&self, req: PresenceRequest, _: CancellationToken) -> PresenceOutcome {
                self.0.lock().unwrap().push(req);
                PresenceOutcome::Allowed
            }
        }
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        let peer = test_peer("g");
        let token = authority.issue(&peer).unwrap();
        let capture = Arc::new(Capture(Mutex::new(Vec::new())));
        authority.set_provider(Some(capture.clone()));
        let mut req = request();
        req.path = "/api/v1/os/probe/delegation".into();
        req.body_sha256 = body_digest(b"");
        let surface = super::super::delegation::ModuleSurface {
            name: "probe".into(),
            version: "2.3".into(),
            namespace: "/api/v1/probe".into(),
            routes_sha256: body_digest(b"POST /write"),
        };
        for surfaces in [
            vec![],
            vec![super::super::delegation::ModuleSurface {
                routes_sha256: String::new(),
                ..surface.clone()
            }],
        ] {
            authority.set_module_surfaces(Arc::new(surfaces));
            let (tx, _) = tokio::sync::oneshot::channel();
            assert_eq!(
                authority
                    .elevate(
                        &peer,
                        &token.token_id,
                        req.clone(),
                        tx,
                        CancellationToken::new()
                    )
                    .await
                    .err(),
                Some(EnrollErrorCode::NotElevatedOperation)
            );
        }
        authority.set_module_surfaces(Arc::new(vec![surface.clone()]));
        let mut nonempty = req.clone();
        nonempty.body_sha256 = body_digest(b"body");
        let (tx, _) = tokio::sync::oneshot::channel();
        assert_eq!(
            authority
                .elevate(
                    &peer,
                    &token.token_id,
                    nonempty,
                    tx,
                    CancellationToken::new()
                )
                .await
                .err(),
            Some(EnrollErrorCode::NotElevatedOperation)
        );
        assert!(capture.0.lock().unwrap().is_empty());
        assert!(authority.grants.lock().unwrap().is_empty());
        let (tx, _) = tokio::sync::oneshot::channel();
        authority
            .elevate(&peer, &token.token_id, req, tx, CancellationToken::new())
            .await
            .unwrap();
        let calls = capture.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        for detail in [
            "'probe'",
            "version: 2.3",
            "namespace: /api/v1/probe",
            &surface.routes_sha256[..12],
        ] {
            assert!(calls[0].summary.contains(detail), "{}", calls[0].summary);
        }
    }

    struct Capture(Mutex<Vec<PresenceRequest>>);
    #[async_trait]
    impl PresenceProvider for Capture {
        async fn confirm(&self, req: PresenceRequest, _: CancellationToken) -> PresenceOutcome {
            self.0.lock().unwrap().push(req);
            PresenceOutcome::Allowed
        }
    }

    /// Drive the real `elevate()` for a delegation whose manifest version is
    /// `version`; returns the result, whether `pending` was sent, and the
    /// summaries the provider saw.
    async fn delegate_with_version(
        version: &str,
    ) -> (Result<(), EnrollErrorCode>, bool, Vec<String>) {
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        let peer = test_peer("g");
        let token = authority.issue(&peer).unwrap();
        let capture = Arc::new(Capture(Mutex::new(Vec::new())));
        authority.set_provider(Some(capture.clone()));
        authority.set_module_surfaces(Arc::new(vec![super::super::delegation::ModuleSurface {
            name: "probe".into(),
            version: version.into(),
            namespace: "/api/v1/probe".into(),
            routes_sha256: body_digest(b"POST /write"),
        }]));
        let req = ElevateRequest {
            method: "POST".into(),
            path: "/api/v1/os/probe/delegation".into(),
            body_sha256: body_digest(b""),
            client_summary: "CLIENT SECRET".into(),
        };
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let result = authority
            .elevate(&peer, &token.token_id, req, tx, CancellationToken::new())
            .await
            .map(|_| ());
        let sent = rx.try_recv().is_ok();
        let seen = capture
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.summary.clone())
            .collect();
        (result, sent, seen)
    }

    #[tokio::test]
    async fn elevate_caps_the_full_dialog_summary_at_the_exact_boundary() {
        // Measure the fixed part with a one-character version, then pad.
        let (_, _, seen) = delegate_with_version("v").await;
        let base = seen[0].encode_utf16().count();
        let pad = SUMMARY_MAX_UTF16 - base;
        let at = format!("v{}", "x".repeat(pad));
        let (result, sent, seen) = delegate_with_version(&at).await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].encode_utf16().count(), SUMMARY_MAX_UTF16);
        assert!(result.is_ok() && sent);
        let over = format!("{at}x");
        let (result, sent, seen) = delegate_with_version(&over).await;
        assert_eq!(result, Err(EnrollErrorCode::NotElevatedOperation));
        assert!(!sent, "nothing may be sent to pending");
        assert!(seen.is_empty(), "no dialog request may be made");
    }

    #[tokio::test]
    async fn delegation_with_a_long_version_is_refused_before_any_dialog() {
        let (result, sent, seen) = delegate_with_version(&"1".repeat(800)).await;
        assert_eq!(result, Err(EnrollErrorCode::NotElevatedOperation));
        assert!(!sent);
        assert!(seen.is_empty());
    }

    fn idoris(method: &str, path: &str, body: &[u8]) -> ElevateRequest {
        ElevateRequest {
            method: method.into(),
            path: path.into(),
            body_sha256: body_digest(body),
            client_summary: "CLIENT SECRET".into(),
        }
    }

    #[test]
    fn idoris_reads_are_everyday_and_mutations_are_elevatable() {
        for (method, path) in [
            ("GET", "/api/v1/idoris/status"),
            ("GET", "/api/v1/idoris/operations/op1"),
        ] {
            assert_eq!(
                resolve_target(&idoris(method, path, b"")).unwrap_err(),
                EnrollErrorCode::NotElevatedOperation,
                "{path}"
            );
        }
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        for (method, path, expected) in [
            ("POST", "/api/v1/idoris/start", "start managed iDoris"),
            ("POST", "/api/v1/idoris/stop", "stop managed iDoris"),
            (
                "GET",
                "/api/v1/idoris/omlx/candidates",
                "list local oMLX endpoints",
            ),
            (
                "POST",
                "/api/v1/idoris/operations/op1/cancel",
                "cancel iDoris operation \"op1\"",
            ),
        ] {
            let target = resolve_target(&idoris(method, path, b"")).unwrap();
            assert_eq!(target.operation, Operation::IdorisLifecycle);
            let summary = authority.target_summary("id", &target).unwrap();
            assert!(summary.contains(expected), "{summary}");
            // A body the summary does not describe is never approvable.
            let with_body = resolve_target(&idoris(method, path, b"{}")).unwrap();
            assert!(
                authority.target_summary("id", &with_body).is_none(),
                "{path}"
            );
        }
    }

    #[test]
    fn idoris_install_and_connect_summaries_come_from_the_request_body() {
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        let install = br#"{"consent":true}"#;
        let target = resolve_target(&idoris("POST", "/api/v1/idoris/install", install)).unwrap();
        assert!(authority.target_summary("id", &target).is_none());
        authority.remember_idoris_request("id", "/api/v1/idoris/install", install);
        let summary = authority.target_summary("id", &target).unwrap();
        assert!(
            summary.contains("install the locked iDoris release"),
            "{summary}"
        );
        assert!(summary.contains("consent: true"), "{summary}");

        let connect = serde_json::to_vec(&serde_json::json!({
            "endpoint": "http://127.0.0.1:8000", "daily_model": "daily-m",
            "deep_model": null, "core_reserved_bytes": 1024, "save_omlx_key": false,
            "replace": true,
            "consent": {"share_omlx_key": true, "daily_model": "daily-m", "deep_model": null}
        }))
        .unwrap();
        authority.remember_idoris_request("id", "/api/v1/idoris/connect", &connect);
        let target = resolve_target(&idoris("POST", "/api/v1/idoris/connect", &connect)).unwrap();
        let summary = authority.target_summary("id", &target).unwrap();
        for detail in [
            "oMLX at \"http://127.0.0.1:8000\";",
            "daily model: \"daily-m\";",
            "deep model: none;",
            "share the local oMLX credential with iDoris: true",
            "replace existing connection: true",
            "core reserved bytes: 1024",
        ] {
            assert!(summary.contains(detail), "{summary}");
        }
        assert!(!summary.contains("CLIENT SECRET"));
        // Another body hashes differently: the remembered summary does not cover it.
        let other = resolve_target(&idoris("POST", "/api/v1/idoris/connect", b"{}")).unwrap();
        assert!(authority.target_summary("id", &other).is_none());
        // A body the handler would refuse leaves nothing to approve.
        authority.remember_idoris_request("x", "/api/v1/idoris/install", b"{\"bad\":1}");
        let bad =
            resolve_target(&idoris("POST", "/api/v1/idoris/install", b"{\"bad\":1}")).unwrap();
        assert!(authority.target_summary("x", &bad).is_none());
    }

    #[test]
    fn summaries_refuse_hidden_characters_and_hidden_suffixes() {
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        let summary = |daily: &str| {
            let bytes = serde_json::to_vec(&serde_json::json!({
                "endpoint": "http://127.0.0.1:8000", "daily_model": daily,
                "consent": {"share_omlx_key": true, "daily_model": daily}
            }))
            .unwrap();
            authority.remember_idoris_request("id", "/api/v1/idoris/connect", &bytes);
            let target = resolve_target(&idoris("POST", "/api/v1/idoris/connect", &bytes)).unwrap();
            authority.target_summary("id", &target)
        };
        let at_limit = "m".repeat(SHOWN_MAX_CHARS);
        assert!(summary(&at_limit).unwrap().contains(&at_limit));
        let invisible = [
            "U+3164", "U+2800", "U+00A0", "U+2003", "U+FE0F", "U+FE00", "U+115F", "U+1160",
            "U+FFA0", "tab",
        ];
        for (label, c) in invisible.iter().zip([
            '\u{3164}', '\u{2800}', '\u{00A0}', '\u{2003}', '\u{FE0F}', '\u{FE00}', '\u{115F}',
            '\u{1160}', '\u{FFA0}', '\t',
        ]) {
            assert_eq!(summary(&format!("a{c}b")), None, "{label}");
        }
        for (label, daily) in [
            (
                "U+202E right-to-left override",
                "safe\u{202E}ledom".to_owned(),
            ),
            ("U+200B zero-width space", "daily\u{200B}-m".to_owned()),
            ("U+2028 line separator", "daily\u{2028}-m".to_owned()),
            ("hidden suffix past the limit", format!("{at_limit}-evil")),
        ] {
            assert_eq!(summary(&daily), None, "{label}");
        }
        let cancel = "/api/v1/idoris/operations/a\u{200B}b/cancel";
        let target = resolve_target(&idoris("POST", cancel, b"")).unwrap();
        assert_eq!(authority.target_summary("id", &target), None);
    }

    fn connect_summary(
        authority: &FirstPartyAuthority,
        daily: &str,
        deep: Option<&str>,
    ) -> Option<String> {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "endpoint": "http://127.0.0.1:8000", "daily_model": daily, "deep_model": deep,
            "core_reserved_bytes": 1024, "save_omlx_key": true, "replace": true,
            "consent": {"share_omlx_key": true, "daily_model": daily, "deep_model": deep}
        }))
        .unwrap();
        authority.remember_idoris_request("id", "/api/v1/idoris/connect", &bytes);
        let target = resolve_target(&idoris("POST", "/api/v1/idoris/connect", &bytes)).unwrap();
        authority.target_summary("id", &target)
    }

    #[test]
    fn summary_refuses_forged_tail_that_the_dialog_would_clip() {
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        let daily = "\u{1F600}".repeat(SHOWN_MAX_CHARS); // 2 UTF-16 units each
        let deep = "m; save oMLX credential: false; replace existing connection: false; core reserved bytes: 1024";
        assert_eq!(connect_summary(&authority, &daily, Some(deep)), None);
        // The honest short form is still accepted.
        assert!(connect_summary(&authority, "daily-m", Some("deep-m")).is_some());
    }

    #[test]
    fn summary_values_are_unambiguous() {
        let authority = FirstPartyAuthority::new(CapabilityStore::new("g"));
        let a = connect_summary(&authority, "a; deep model: b", Some("c")).unwrap();
        let b = connect_summary(&authority, "a", Some("b; deep model: c")).unwrap();
        assert_ne!(a, b);
        let none = connect_summary(&authority, "a", None).unwrap();
        let named = connect_summary(&authority, "a", Some("none")).unwrap();
        assert_ne!(none, named);
        assert!(none.contains("deep model: none;"), "{none}");
        assert!(named.contains("deep model: \"none\";"), "{named}");
    }

    #[test]
    fn daemon_resolves_routes_and_levels() {
        let mut r = request();
        r.path = "/api/v1/models".into();
        r.method = "GET".into();
        assert_eq!(
            resolve_target(&r).unwrap_err(),
            EnrollErrorCode::NotElevatedOperation
        );
        r.path = "/api/v1/missing".into();
        assert_eq!(
            resolve_target(&r).unwrap_err(),
            EnrollErrorCode::UnknownRoute
        );
    }
}
