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
        let identifier = std::env::var("AGENT24_TEST_ENROLL_IDENTIFIER")
            .ok()
            .filter(|id| ["ai.auraai.agent24", "agent24", "agent24d"].contains(&id.as_str()))
            .unwrap_or_else(|| "agent24".to_owned());
        Ok(VerifiedIdentity {
            kind: IdentityKind::MacosSecCode,
            identifier,
            team_id: "HYAP444FN9".into(),
            executable: std::env::current_exe().map_err(|_| EnrollErrorCode::IdentityRejected)?,
            audit_token,
        })
    }
}

/// Unsigned callers are let through: `AGENT24_TEST_PRESENCE=allow|deny`
/// (which also fixes the outcome, see [`fixed_presence`]) or
/// `AGENT24_TEST_UNSIGNED=1` (identity only: the challenge still goes to the
/// presence provider the start mode installs, so an unsigned desktop on a
/// build machine can answer it through its presence connection).
pub fn enabled() -> bool {
    switches(
        std::env::var("AGENT24_TEST_PRESENCE").ok().as_deref(),
        std::env::var("AGENT24_TEST_UNSIGNED").ok().as_deref(),
    )
    .0
}

/// The presence provider is replaced by a fixed answer: only for
/// `AGENT24_TEST_PRESENCE=allow|deny`, never by `AGENT24_TEST_UNSIGNED`.
pub fn fixed_presence() -> bool {
    switches(
        std::env::var("AGENT24_TEST_PRESENCE").ok().as_deref(),
        std::env::var("AGENT24_TEST_UNSIGNED").ok().as_deref(),
    )
    .1
}

/// (let unsigned callers through, replace the presence provider).
pub fn switches(presence: Option<&str>, unsigned: Option<&str>) -> (bool, bool) {
    let fixed = matches!(presence, Some("allow" | "deny"));
    (fixed || unsigned == Some("1"), fixed)
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

#[cfg(test)]
mod tests {
    use super::switches;

    /// PM 11-09: an unsigned desktop on a build machine answers through its
    /// presence connection; only `AGENT24_TEST_PRESENCE` fixes the outcome.
    #[test]
    fn identity_and_provider_are_separate_switches() {
        assert_eq!(switches(None, None), (false, false));
        assert_eq!(switches(Some("allow"), None), (true, true));
        assert_eq!(switches(Some("deny"), Some("1")), (true, true));
        assert_eq!(switches(None, Some("1")), (true, false));
        assert_eq!(switches(Some("maybe"), Some("yes")), (false, false));
    }
}
