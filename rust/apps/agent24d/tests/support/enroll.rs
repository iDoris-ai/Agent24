//! Test-only substitutes for unsigned CLI binaries and desktop presence.
//! S5 manual acceptance must verify real macOS signatures on signed binaries.

use std::io;

use crate::capabilities::presence::{PresenceOutcome, PresenceProvider, PresenceRequest};
use crate::enroll_api::{EnrollErrorCode, IdentityKind, ProcStart, VerifiedIdentity};
use crate::enroll_endpoint::{IdentityVerifier, SystemIdentityVerifier};
use tokio_util::sync::CancellationToken;

pub struct UnsignedIdentity;
impl IdentityVerifier for UnsignedIdentity {
    fn start_time(&self, pid: i32) -> io::Result<ProcStart> {
        SystemIdentityVerifier.start_time(pid)
    }

    fn verify(
        &self,
        _pid: i32,
        audit_token: Option<agent24_os_proc::AuditToken>,
    ) -> Result<VerifiedIdentity, EnrollErrorCode> {
        Ok(VerifiedIdentity {
            kind: IdentityKind::MacosSecCode,
            identifier: "agent24".into(),
            team_id: "HYAP444FN9".into(),
            executable: std::env::current_exe().map_err(|_| EnrollErrorCode::IdentityRejected)?,
            audit_token,
        })
    }
}

pub fn enabled() -> bool {
    matches!(
        std::env::var("AGENT24_TEST_PRESENCE").as_deref(),
        Ok("allow" | "deny")
    )
}

pub struct TestPresence;
#[async_trait::async_trait]
impl PresenceProvider for TestPresence {
    async fn confirm(&self, _req: PresenceRequest, _cancel: CancellationToken) -> PresenceOutcome {
        match std::env::var("AGENT24_TEST_PRESENCE").as_deref() {
            Ok("allow") => PresenceOutcome::Allowed,
            Ok("deny") => PresenceOutcome::Denied,
            _ => PresenceOutcome::Unavailable,
        }
    }
}
