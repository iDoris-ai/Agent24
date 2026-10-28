//! CORE-2 AgentEar slice — the seam between the enrolment socket (S1,
//! `enroll_endpoint.rs`) and the first-party token authority (S2,
//! `capabilities/first_party.rs`). See `docs/agent/CORE-2-AGENTEAR-SLICE.md` §3.
//!
//! Frozen interface: S1 and S2 add this file byte-for-byte identical. Change
//! it only in a PR that updates both sides.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Process start time as the kernel reports it; `(pid, start)` names one
/// process for its whole life (design D5 step 2/4).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcStart {
    pub sec: u64,
    pub usec: u64,
}

/// How the peer's identity was established (design D5 step 3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityKind {
    MacosSecCode,
}

/// Identity verified by the daemon itself, never taken from the peer's frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedIdentity {
    pub kind: IdentityKind,
    /// Code-signing identifier, e.g. `agent24`.
    pub identifier: String,
    pub team_id: String,
    pub executable: std::path::PathBuf,
}

/// A peer that passed the handshake on one enrolment connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrolledPeer {
    pub conn_id: String,
    pub uid: u32,
    pub pid: i32,
    pub start: ProcStart,
    pub identity: VerifiedIdentity,
    pub generation: String,
}

/// A freshly minted first-party token. `secret` leaves the daemon exactly
/// once, in the `enrolled` frame; the authority keeps only its digest.
pub struct IssuedToken {
    pub token_id: String,
    pub secret: String,
    pub expires_in: u64,
}

impl std::fmt::Debug for IssuedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedToken")
            .field("token_id", &self.token_id)
            .field("secret", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// The `elevate` frame after framing checks (slice §3.1).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElevateRequest {
    pub method: String,
    pub path: String,
    /// Lower-case hex SHA-256 of the exact request body (empty body included).
    pub body_sha256: String,
    /// Client-supplied text; logged only, never shown as the summary (D4.1).
    pub client_summary: String,
}

pub struct ElevationGrant {
    pub challenge_id: String,
    pub secret: String,
    pub expires_in: u64,
}

impl std::fmt::Debug for ElevationGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElevationGrant")
            .field("challenge_id", &self.challenge_id)
            .field("secret", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// Wire error codes of slice §3.1; `as_code` is the exact string sent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnrollErrorCode {
    IdentityRejected,
    GenerationMismatch,
    UnsupportedVersion,
    BadFrame,
    Timeout,
    NotEnrolled,
    PresenceDenied,
    PresenceUnavailable,
    PresenceTimeout,
    NotElevatedOperation,
    UnknownRoute,
    Internal,
}

impl EnrollErrorCode {
    pub fn as_code(self) -> &'static str {
        match self {
            Self::IdentityRejected => "identity_rejected",
            Self::GenerationMismatch => "generation_mismatch",
            Self::UnsupportedVersion => "unsupported_version",
            Self::BadFrame => "bad_frame",
            Self::Timeout => "timeout",
            Self::NotEnrolled => "not_enrolled",
            Self::PresenceDenied => "presence_denied",
            Self::PresenceUnavailable => "presence_unavailable",
            Self::PresenceTimeout => "presence_timeout",
            Self::NotElevatedOperation => "not_elevated_operation",
            Self::UnknownRoute => "unknown_route",
            Self::Internal => "internal",
        }
    }
}

/// Implemented by S2. S1 calls it only after the identity check and the
/// second `(pid, start)` read both passed.
#[async_trait]
pub trait EnrollAuthority: Send + Sync {
    fn issue(&self, peer: &EnrolledPeer) -> Result<IssuedToken, EnrollErrorCode>;
    /// Renew on the original connection only (design §10 R4): the old token
    /// is revoked atomically with the new mint.
    fn renew(&self, peer: &EnrolledPeer, token_id: &str) -> Result<IssuedToken, EnrollErrorCode>;
    /// Runs the D4 challenge and waits for the presence provider. S1 sends
    /// `elevation_pending` through `pending` before the wait begins.
    async fn elevate(
        &self,
        peer: &EnrolledPeer,
        token_id: &str,
        request: ElevateRequest,
        pending: tokio::sync::oneshot::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<ElevationGrant, EnrollErrorCode>;
}
