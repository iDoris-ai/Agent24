//! Independent presence confirmation (slice §3.3); no caller assertion proves presence.
use super::{Operation, UnixSeconds};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequesterIdentity {
    pub identifier: String,
    pub pid: i32,
    /// Loop 4 (PM 11-20, a floor): who is asking, from the executable the
    /// daemon verified at enrolment, so the box never shows a CLI's request
    /// as the app's (both carry the same signing identifier).
    pub kind: RequesterKind,
}

/// What kind of program asks for a confirmation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequesterKind {
    /// The Agent24 app itself (`…/Contents/MacOS/Agent24`).
    Desktop,
    /// The `agent24` command line (any `agent24…` program but the daemon).
    CommandLine,
    Other,
}

impl RequesterKind {
    /// From the executable the daemon verified for the peer; never from
    /// anything the peer says.
    #[must_use]
    pub fn of_executable(path: &std::path::Path) -> Self {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if name == "Agent24" && parent == "MacOS" {
            Self::Desktop
        } else if name.starts_with("agent24") && !name.starts_with("agent24d") {
            Self::CommandLine
        } else {
            Self::Other
        }
    }

    /// On the wire to the desktop, which words it in the user's language.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::CommandLine => "cli",
            Self::Other => "other",
        }
    }

    /// In the daemon's own summary text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Desktop => "the Agent24 app",
            Self::CommandLine => "the command line (agent24 CLI)",
            Self::Other => "another program",
        }
    }
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
