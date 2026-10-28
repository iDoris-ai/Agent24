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
use tokio_util::sync::CancellationToken;

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
    expires_at: UnixSeconds,
}
struct AttachedSummary {
    body_sha256: String,
    name: String,
    expires_at: UnixSeconds,
}

pub struct FirstPartyAuthority {
    store: CapabilityStore,
    provider: Mutex<Option<Arc<dyn PresenceProvider>>>,
    grants: Mutex<HashMap<[u8; 32], Grant>>,
    attached_summaries: Mutex<HashMap<String, AttachedSummary>>,
}
impl FirstPartyAuthority {
    pub fn new(store: CapabilityStore) -> Self {
        Self {
            store,
            provider: Mutex::new(None),
            grants: Mutex::new(HashMap::new()),
            attached_summaries: Mutex::new(HashMap::new()),
        }
    }
    /// Derive display context from the rejected HTTP request itself. The
    /// frozen UDS frame carries only a hash, never the body or a trusted name.
    pub fn remember_attached_request(&self, token_id: &str, bytes: &[u8]) {
        let Ok(body) = serde_json::from_slice::<agent24_protocol::AttachedAddRequest>(bytes) else {
            return;
        };
        let Ok(manifest) = agent24_domain::DomainOsManifest::from_yaml(&body.manifest) else {
            return;
        };
        let Ok(mut summaries) = self.attached_summaries.lock() else {
            return;
        };
        let now = unix_now();
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
            AttachedSummary {
                body_sha256: body_digest(bytes),
                name: manifest.name().to_owned(),
                expires_at: now + GRANT_TTL,
            },
        );
    }
    fn attached_name(&self, token_id: &str, body_sha256: &str) -> Option<String> {
        self.attached_summaries
            .lock()
            .ok()?
            .get(token_id)
            .filter(|s| s.expires_at > unix_now() && s.body_sha256 == body_sha256)
            .map(|s| s.name.clone())
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
            || record.claims.expires_at <= unix_now()
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
                || record.claims.expires_at <= unix_now()
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
                    }),
                    workspace_id: None,
                    attachment_id: None,
                    principal_id: None,
                    daemon_generation: peer.generation.clone(),
                    host_generation: None,
                    sidecar_generation: None,
                    created_at: now,
                    expires_at: now + TOKEN_TTL,
                    allowed_operations: policies()
                        .iter()
                        .map(|p| p.operation)
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
            secret,
            expires_in: TOKEN_TTL,
        })
    }
    pub fn consume(&self, secret: &str, token_id: &str, target: &Target, now: UnixSeconds) -> bool {
        let Ok(token) = CapabilityToken::parse_bearer(secret) else {
            return false;
        };
        // Linearize token liveness and grant consumption with revocation and
        // renewal. HTTP's earlier check alone leaves a revocation race.
        let Ok(state) = self.store.shared.state.lock() else {
            return false;
        };
        let Some(record) = state
            .ids
            .get(token_id)
            .and_then(|key| state.records.get(key))
        else {
            return false;
        };
        if record.claims.audience != Audience::FirstPartyShell
            || !record
                .claims
                .first_party
                .as_ref()
                .is_some_and(|claims| peer_live(&claims.peer))
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
        grants.retain(|_, grant| grant.expires_at > now);
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
fn peer_live(peer: &EnrolledPeer) -> bool {
    agent24_os_proc::is_alive(peer.pid)
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
    if policy.level != AssuranceLevel::Elevated {
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
        let summary = format!(
            "{} (pid {}) wants to: {}",
            peer.identity.identifier,
            peer.pid,
            if target.method == "POST" && target.path == "/api/v1/attached" {
                self.attached_name(id, &target.body_sha256).map_or_else(
                    || "register attached module".to_owned(),
                    |name| format!("register attached module '{name}'"),
                )
            } else {
                format!("{} {}", target.method, target.path)
            }
        );
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
        if unix_now() >= expires_at {
            return Err(EnrollErrorCode::PresenceTimeout);
        }
        self.live_peer(peer, id)?;
        let secret = CapabilityToken::random();
        let now = unix_now();
        let mut grants = self.grants.lock().map_err(|_| EnrollErrorCode::Internal)?;
        grants.retain(|_, g| g.expires_at > now);
        grants.insert(
            digest(&secret),
            Grant {
                challenge_id: challenge_id.clone(),
                token_id: id.to_owned(),
                target,
                expires_at: now + GRANT_TTL,
            },
        );
        Ok(ElevationGrant {
            challenge_id,
            secret: secret.bearer(),
            expires_in: GRANT_TTL,
        })
    }
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
    fn request() -> ElevateRequest {
        ElevateRequest {
            method: "POST".into(),
            path: "/api/v1/attached".into(),
            body_sha256: body_digest(b"body"),
            client_summary: "CLIENT SECRET".into(),
        }
    }
    async fn grant(
        authority: &FirstPartyAuthority,
        peer: &EnrolledPeer,
        id: &str,
    ) -> ElevationGrant {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        authority
            .elevate(peer, id, request(), tx, CancellationToken::new())
            .await
            .unwrap()
    }
    #[test]
    fn daily_claims_ttl_scope_and_atomic_renewal() {
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
                    Operation::ModelsRead,
                    &Resource::global(),
                    auth.claims.expires_at
                )
                .is_err()
        );
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
    #[tokio::test]
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
        assert!(!authority.consume(&expired.secret, &token.token_id, &target, unix_now() + 60));
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
