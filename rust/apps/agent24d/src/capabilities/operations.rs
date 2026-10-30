use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Operation {
    ModelsAdmin,
    ComponentInstall,
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
    KnowledgeHost,
    IdorisLifecycle,
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
    // CORE-2 §3 names, appended so existing variant order stays put.
    HealthRead,
    MemoryRead,
    MemoryForget,
    MemorySettingsWrite,
    ChatCreate,
    CommRead,
    CommWrite,
    ModuleRead,
    ScheduleRead,
    UsageRead,
    ConfigRead,
    ConsentRevoke,
    ModuleRoute,
    // CORE-2 AgentEar S3: desktop confirmation channel (ProductHost only).
    PresenceRead,
    PresenceDecide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceScope {
    Global,
    Principal,
}

impl Operation {
    /// The desktop confirmation channel (CORE-2 AgentEar S3): ProductHost
    /// only, never granted to a first-party shell token.
    pub fn is_presence_channel(self) -> bool {
        matches!(self, Self::PresenceRead | Self::PresenceDecide)
    }

    pub fn scope(self) -> ResourceScope {
        match self {
            Self::ModelsRead => ResourceScope::Global,
            _ => ResourceScope::Principal,
        }
    }

    /// Historical Creative grant. CORE-2 R5: mapping a route does not add its
    /// operation here. Route admission is a separate gate.
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

    pub const fn action(self) -> &'static str {
        match self {
            Self::ModelsAdmin => "models.admin",
            Self::ComponentInstall => "component.install",
            Self::WorkspaceCreate => "workspace.create",
            Self::WorkspaceRelease => "workspace.release",
            Self::HostLease => "host.lease",
            Self::ApprovalDecision => "approval.decision",
            Self::GrantManage => "grant.manage",
            Self::OverrideManage => "override.manage",
            Self::ScheduleManage => "schedule.manage",
            Self::ModuleAdmin => "module.admin",
            Self::CapabilityMint => "capability.mint",
            Self::CapabilityRevoke => "capability.revoke",
            Self::KnowledgeHost => "knowledge.host",
            Self::IdorisLifecycle => "idoris.lifecycle",
            Self::Shutdown => "shutdown",
            Self::ModelsRead => "models.read",
            Self::WorkspaceResolve => "workspace.resolve",
            Self::SessionCreate => "session.create",
            Self::SessionRead => "session.read",
            Self::SessionTranscript => "session.transcript",
            Self::RunCreate => "run.create",
            Self::RunRead => "run.read",
            Self::RunCancel => "run.cancel",
            Self::EventsRead => "events.read",
            Self::ApprovalStatusRead => "approval.status",
            Self::HealthRead => "health.read",
            Self::MemoryRead => "memory.read",
            Self::MemoryForget => "memory.forget",
            Self::MemorySettingsWrite => "memory.settings.write",
            Self::ChatCreate => "chat.create",
            Self::CommRead => "comm.read",
            Self::CommWrite => "comm.write",
            Self::ModuleRead => "module.read",
            Self::ScheduleRead => "schedule.read",
            Self::UsageRead => "usage.read",
            Self::ConfigRead => "config.read",
            Self::ConsentRevoke => "consent.revoke",
            Self::ModuleRoute => "module.route",
            Self::PresenceRead => "presence.read",
            Self::PresenceDecide => "presence.decide",
        }
    }

    /// Catalog level for this operation. `None` when no route assigns one.
    pub fn assurance_level(self) -> Option<super::route_policy::AssuranceLevel> {
        super::route_policy::assurance_level(self)
    }

    pub fn from_action(action: &str) -> Option<Self> {
        Some(match action {
            "models.admin" => Self::ModelsAdmin,
            "component.install" => Self::ComponentInstall,
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
            "knowledge.host" => Self::KnowledgeHost,
            "idoris.lifecycle" => Self::IdorisLifecycle,
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
            "health.read" => Self::HealthRead,
            "memory.read" => Self::MemoryRead,
            "memory.forget" => Self::MemoryForget,
            "memory.settings.write" => Self::MemorySettingsWrite,
            "chat.create" => Self::ChatCreate,
            "comm.read" => Self::CommRead,
            "comm.write" => Self::CommWrite,
            "module.read" => Self::ModuleRead,
            "module.route" => Self::ModuleRoute,
            "presence.read" => Self::PresenceRead,
            "presence.decide" => Self::PresenceDecide,
            "schedule.read" => Self::ScheduleRead,
            "usage.read" => Self::UsageRead,
            "config.read" => Self::ConfigRead,
            "consent.revoke" => Self::ConsentRevoke,
            _ => return None,
        })
    }
}
