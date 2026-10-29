//! Independent presence confirmation (slice §3.3); no caller assertion proves presence.
use super::{Operation, UnixSeconds};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequesterIdentity {
    pub identifier: String,
    pub pid: i32,
}
#[derive(Clone, Debug)]
pub struct PresenceRequest {
    pub challenge_id: String,
    pub operation: Operation,
    pub method: String,
    pub path: String,
    pub summary: String,
    pub requester: RequesterIdentity,
    pub expires_at: UnixSeconds,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresenceOutcome {
    Allowed,
    Denied,
    Unavailable,
    TimedOut,
}
#[async_trait]
pub trait PresenceProvider: Send + Sync {
    async fn confirm(&self, req: PresenceRequest, cancel: CancellationToken) -> PresenceOutcome;
}
pub struct NoPresenceProvider;
#[async_trait]
impl PresenceProvider for NoPresenceProvider {
    async fn confirm(&self, _req: PresenceRequest, _cancel: CancellationToken) -> PresenceOutcome {
        PresenceOutcome::Unavailable
    }
}
