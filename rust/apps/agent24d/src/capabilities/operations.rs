use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Operation {
    ModelsAdmin,
    WorkspaceCreate,
    WorkspaceRelease,
    HostLease,
    ApprovalDecision,
    GrantManage,
    OverrideManage,
    ScheduleManage,
    ModuleAdmin,
    CapabilityMint,
    CapabilityRevoke,
    Shutdown,
    ModelsRead,
    WorkspaceResolve,
    SessionCreate,
    SessionRead,
    SessionTranscript,
    RunCreate,
    RunRead,
    RunCancel,
    EventsRead,
    ApprovalStatusRead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceScope {
    Global,
    Principal,
}

impl Operation {
    pub fn scope(self) -> ResourceScope {
        match self {
            Self::ModelsRead => ResourceScope::Global,
            _ => ResourceScope::Principal,
        }
    }

    pub fn creative_allowlist() -> BTreeSet<Self> {
        [
            Self::ModelsRead,
            Self::WorkspaceResolve,
            Self::SessionCreate,
            Self::SessionRead,
            Self::SessionTranscript,
            Self::RunCreate,
            Self::RunRead,
            Self::RunCancel,
            Self::EventsRead,
            Self::ApprovalStatusRead,
        ]
        .into_iter()
        .collect()
    }

    pub fn from_action(action: &str) -> Option<Self> {
        Some(match action {
            "models.admin" => Self::ModelsAdmin,
            "workspace.create" => Self::WorkspaceCreate,
            "workspace.release" => Self::WorkspaceRelease,
            "host.lease" => Self::HostLease,
            "approval.decision" => Self::ApprovalDecision,
            "grant.manage" => Self::GrantManage,
            "override.manage" => Self::OverrideManage,
            "schedule.manage" => Self::ScheduleManage,
            "module.admin" => Self::ModuleAdmin,
            "capability.mint" => Self::CapabilityMint,
            "capability.revoke" => Self::CapabilityRevoke,
            "shutdown" => Self::Shutdown,
            "models.read" | "models/read" => Self::ModelsRead,
            "workspace.resolve" | "workspace/resolve" => Self::WorkspaceResolve,
            "session.create" | "session/new" => Self::SessionCreate,
            "session.read" | "session/load" | "session/get" => Self::SessionRead,
            "session.transcript" | "session/transcript" => Self::SessionTranscript,
            "run.create" | "run/new" => Self::RunCreate,
            "run.read" | "run/get" => Self::RunRead,
            "run.cancel" | "run/cancel" => Self::RunCancel,
            "events.read" | "events/subscribe" => Self::EventsRead,
            "approval.status" | "approval/read" => Self::ApprovalStatusRead,
            _ => return None,
        })
    }
}
