//! Fixed product routes → Operation → assurance level.
//!
//! Templates are axum `MatchedPath` values, including the `/api/v1` prefix.
//! §3 of the enroll design omits that prefix; the rows are otherwise the same
//! table. Module routes are not listed here: [`decide`] maps them only when
//! axum matched a non-fixed route, or the path sits under a namespace that was
//! actually mounted. Delegation records are 2b (R1); this slice only classifies.

use std::collections::BTreeSet;

use axum::http::Method;

use super::Operation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssuranceLevel {
    /// Explicit unauthenticated exception. Health is the only one.
    Public,
    /// 日常.
    Everyday,
    /// 提升.
    Elevated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoutePolicy {
    pub method: &'static str,
    pub template: &'static str,
    pub operation: Operation,
    pub level: AssuranceLevel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteDecision {
    Mapped(RoutePolicy),
    /// Mounted module namespace. Level is everyday; 2a does not enforce delegation.
    Module {
        operation: Operation,
        level: AssuranceLevel,
    },
    UnknownPath,
    MethodNotAllowed,
}

const MODULE_LEVEL: AssuranceLevel = AssuranceLevel::Everyday;

const fn row(
    method: &'static str,
    template: &'static str,
    operation: Operation,
    level: AssuranceLevel,
) -> RoutePolicy {
    RoutePolicy {
        method,
        template,
        operation,
        level,
    }
}

#[rustfmt::skip]
const POLICIES: &[RoutePolicy] = &[
    row("GET", "/api/v1/health", Operation::HealthRead, AssuranceLevel::Public),
    row("GET", "/api/v1/diagnostics/migration", Operation::DiagnosticRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/diagnostics/migration/snapshots", Operation::DiagnosticRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/diagnostics/migration/space", Operation::DiagnosticRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/idoris/status", Operation::IdorisRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/idoris/install", Operation::IdorisLifecycle, AssuranceLevel::Elevated),
    row("POST", "/api/v1/idoris/connect", Operation::IdorisLifecycle, AssuranceLevel::Elevated),
    row("GET", "/api/v1/idoris/omlx/candidates", Operation::IdorisLifecycle, AssuranceLevel::Elevated),
    row("POST", "/api/v1/idoris/start", Operation::IdorisLifecycle, AssuranceLevel::Elevated),
    row("POST", "/api/v1/idoris/stop", Operation::IdorisLifecycle, AssuranceLevel::Elevated),
    row("GET", "/api/v1/idoris/operations/{id}", Operation::IdorisRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/idoris/operations/{id}/cancel", Operation::IdorisLifecycle, AssuranceLevel::Elevated),
    row("POST", "/api/v1/chat", Operation::ChatCreate, AssuranceLevel::Everyday),
    row("GET", "/api/v1/models", Operation::ModelsRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/decide/profile", Operation::ConfigRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/capabilities/creative", Operation::CapabilityMint, AssuranceLevel::Elevated),
    row("POST", "/api/v1/capabilities/{capability_id}/revoke", Operation::CapabilityRevoke, AssuranceLevel::Elevated),
    row("GET", "/api/v1/usage", Operation::UsageRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/timings", Operation::UsageRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/timings/summary", Operation::UsageRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/tools", Operation::ConfigRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/module-consents", Operation::ConfigRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/module-consents/export", Operation::ConfigRead, AssuranceLevel::Everyday),
    row("DELETE", "/api/v1/module-consents/modules/{module}", Operation::ConsentRevoke, AssuranceLevel::Everyday),
    row("DELETE", "/api/v1/module-consents/modules/{module}/tools/{tool}", Operation::ConsentRevoke, AssuranceLevel::Everyday),
    row("POST", "/api/v1/module-consents/modules/{module}/tools/{tool}", Operation::ApprovalDecision, AssuranceLevel::Elevated),
    row("GET", "/api/v1/module-consents/modules/{module}/tools/{tool}/card", Operation::ConfigRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/tool-overrides", Operation::ConfigRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/standing-grants", Operation::ConfigRead, AssuranceLevel::Everyday),
    row("DELETE", "/api/v1/standing-grants/{id}", Operation::GrantManage, AssuranceLevel::Elevated),
    row("PUT", "/api/v1/tool-overrides/{pattern}", Operation::OverrideManage, AssuranceLevel::Elevated),
    row("DELETE", "/api/v1/tool-overrides/{pattern}", Operation::OverrideManage, AssuranceLevel::Elevated),
    row("GET", "/api/v1/memory/assertions", Operation::MemoryRead, AssuranceLevel::Everyday),
    row("DELETE", "/api/v1/memory/assertions/{id}", Operation::MemoryForget, AssuranceLevel::Everyday),
    row("GET", "/api/v1/memory/settings", Operation::MemoryRead, AssuranceLevel::Everyday),
    row("PUT", "/api/v1/memory/settings", Operation::MemorySettingsWrite, AssuranceLevel::Elevated),
    row("GET", "/api/v1/approvals", Operation::ApprovalStatusRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/approvals/{id}", Operation::ApprovalStatusRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/approvals/{id}", Operation::ApprovalDecision, AssuranceLevel::Elevated),
    row("GET", "/api/v1/module-approvals", Operation::ApprovalStatusRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/module-approvals/{id}", Operation::ApprovalStatusRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/module-approvals/{id}", Operation::ApprovalDecision, AssuranceLevel::Elevated),
    row("GET", "/api/v1/schedules", Operation::ScheduleRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/schedules", Operation::ScheduleManage, AssuranceLevel::Elevated),
    row("GET", "/api/v1/schedules/{id}", Operation::ScheduleRead, AssuranceLevel::Everyday),
    row("PATCH", "/api/v1/schedules/{id}", Operation::ScheduleManage, AssuranceLevel::Elevated),
    row("DELETE", "/api/v1/schedules/{id}", Operation::ScheduleManage, AssuranceLevel::Elevated),
    row("POST", "/api/v1/schedules/{id}/run_now", Operation::ScheduleManage, AssuranceLevel::Elevated),
    row("POST", "/api/v1/schedules/{id}/suspend", Operation::ScheduleManage, AssuranceLevel::Elevated),
    row("POST", "/api/v1/schedules/{id}/resume", Operation::ScheduleManage, AssuranceLevel::Elevated),
    row("GET", "/api/v1/events", Operation::EventsRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/attached", Operation::ModuleAdmin, AssuranceLevel::Elevated),
    row("GET", "/api/v1/attached", Operation::ModuleRead, AssuranceLevel::Everyday),
    row("DELETE", "/api/v1/attached/{name}", Operation::ModuleAdmin, AssuranceLevel::Elevated),
    row("PATCH", "/api/v1/attached/{name}", Operation::ModuleAdmin, AssuranceLevel::Elevated),
    row("GET", "/api/v1/os", Operation::ModuleRead, AssuranceLevel::Everyday),
    row("PATCH", "/api/v1/os/{name}", Operation::ModuleAdmin, AssuranceLevel::Elevated),
    row("POST", "/api/v1/os/{name}/delegation", Operation::ModuleAdmin, AssuranceLevel::Elevated),
    row("POST", "/api/v1/os/{name}/stop", Operation::ModuleAdmin, AssuranceLevel::Elevated),
    row("POST", "/api/v1/os/{name}/commands/{command}", Operation::ModuleAdmin, AssuranceLevel::Elevated),
    row("POST", "/api/v1/shutdown", Operation::Shutdown, AssuranceLevel::Elevated),
    row("PUT", "/api/v1/host/knowledge/bindings/{binding_id}", Operation::KnowledgeHost, AssuranceLevel::Elevated),
    row("DELETE", "/api/v1/host/knowledge/bindings/{binding_id}", Operation::KnowledgeHost, AssuranceLevel::Elevated),
    row("PUT", "/api/v1/host/knowledge/bindings/{binding_id}/credential", Operation::KnowledgeHost, AssuranceLevel::Elevated),
    row("DELETE", "/api/v1/host/knowledge/bindings/{binding_id}/credential", Operation::KnowledgeHost, AssuranceLevel::Elevated),
    row("GET", "/api/v1/presence/challenges/next", Operation::PresenceRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/presence/challenges/{id}", Operation::PresenceDecide, AssuranceLevel::Everyday),
    row("GET", "/api/v1/shutdown", Operation::ConfigRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/sessions", Operation::SessionCreate, AssuranceLevel::Everyday),
    row("GET", "/api/v1/sessions", Operation::SessionRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/sessions/{id}", Operation::SessionRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/runs", Operation::RunCreate, AssuranceLevel::Everyday),
    row("GET", "/api/v1/runs", Operation::RunRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/runs/{id}", Operation::RunRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/peer/runs", Operation::RunRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/peer/runs/{id}", Operation::RunRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/runs/{id}/cancel", Operation::RunCancel, AssuranceLevel::Everyday),
    row("POST", "/api/v1/comm/install", Operation::ComponentInstall, AssuranceLevel::Elevated),
    row("GET", "/api/v1/comm/identity", Operation::CommRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/comm/identity", Operation::CommWrite, AssuranceLevel::Elevated),
    row("POST", "/api/v1/comm/identity/default", Operation::CommWrite, AssuranceLevel::Elevated),
    row("POST", "/api/v1/comm/unlock", Operation::CommWrite, AssuranceLevel::Elevated),
    row("GET", "/api/v1/comm/contact", Operation::CommRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/comm/contact", Operation::CommWrite, AssuranceLevel::Elevated),
    row("GET", "/api/v1/comm/relay", Operation::CommRead, AssuranceLevel::Everyday),
    row("PUT", "/api/v1/comm/relay", Operation::CommWrite, AssuranceLevel::Elevated),
    row("POST", "/api/v1/comm/relay/probe", Operation::CommWrite, AssuranceLevel::Elevated),
    row("POST", "/api/v1/comm/import", Operation::CommWrite, AssuranceLevel::Elevated),
    row("GET", "/api/v1/comm/daemon", Operation::CommRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/comm/daemon/start", Operation::CommWrite, AssuranceLevel::Elevated),
    row("POST", "/api/v1/comm/daemon/stop", Operation::CommWrite, AssuranceLevel::Elevated),
    row("POST", "/api/v1/comm/send", Operation::CommWrite, AssuranceLevel::Elevated),
    row("GET", "/api/v1/comm/history", Operation::CommRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/comm/inbox/pull", Operation::CommWrite, AssuranceLevel::Elevated),
    row("GET", "/api/v1/comm/outbox", Operation::CommRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/comm/outbox/{event_id}/retry", Operation::CommWrite, AssuranceLevel::Elevated),
    row("POST", "/api/v1/comm/outbox/clear", Operation::CommWrite, AssuranceLevel::Elevated),
    row("GET", "/api/v1/comm/groupchat", Operation::CommRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/comm/groupchat", Operation::CommWrite, AssuranceLevel::Elevated),
    row("GET", "/api/v1/comm/groupchat/{group_id}/members", Operation::CommRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/comm/groupchat/{group_id}/invites", Operation::CommRead, AssuranceLevel::Everyday),
    row("GET", "/api/v1/comm/groupchat/{group_id}/messages", Operation::CommRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/comm/groupchat/{group_id}/messages", Operation::CommWrite, AssuranceLevel::Elevated),
    row("GET", "/api/v1/comm/groupchat/{group_id}/messages/{logical_id}", Operation::CommRead, AssuranceLevel::Everyday),
    row("POST", "/api/v1/comm/groupchat/{group_id}/messages/{logical_id}/retry", Operation::CommWrite, AssuranceLevel::Elevated),
    row("POST", "/api/v1/comm/groupchat/invites/{invite_id}/accept", Operation::CommWrite, AssuranceLevel::Elevated),
];

pub fn policies() -> &'static [RoutePolicy] {
    POLICIES
}

pub fn assurance_level(operation: Operation) -> Option<AssuranceLevel> {
    if operation == Operation::ModuleRoute {
        return Some(MODULE_LEVEL);
    }
    let mut level = None;
    for policy in POLICIES {
        if policy.operation != operation {
            continue;
        }
        match level {
            None => level = Some(policy.level),
            Some(prev) if prev == policy.level => {}
            Some(_) => return None,
        }
    }
    level
}

/// Table lookup by method and matched template. HEAD uses the GET row.
/// `None` if this method is not on the fixed table (module fallback is [`decide`]).
pub fn required_operation(method: &Method, matched_route_template: &str) -> Option<Operation> {
    find_policy(method, matched_route_template).map(|policy| policy.operation)
}

/// Creative may still call only the route it could call before this mapping.
/// Newly mapped routes stay closed even when their Operation is already on
/// [`Operation::creative_allowlist`] (CORE-2 R5). HEAD is a different request:
/// base answered `HEAD /api/v1/models` with 403, so a live Creative token still
/// stops here after the bearer check.
pub fn creative_route_permitted(method: &Method, template: &str) -> bool {
    method == Method::GET && template == "/api/v1/models"
}

pub fn decide(
    method: &Method,
    matched_template: Option<&str>,
    uri_path: &str,
    module_namespaces: &BTreeSet<String>,
) -> RouteDecision {
    if let Some(template) = matched_template {
        if let Some(policy) = find_policy(method, template) {
            let operation = required_operation(method, template).unwrap_or(policy.operation);
            return RouteDecision::Mapped(RoutePolicy {
                operation,
                ..*policy
            });
        }
        if template_known(template) {
            return RouteDecision::MethodNotAllowed;
        }
        return module_decision();
    }
    if mounted_namespace(uri_path, module_namespaces) {
        return module_decision();
    }
    RouteDecision::UnknownPath
}

fn module_decision() -> RouteDecision {
    RouteDecision::Module {
        operation: Operation::ModuleRoute,
        level: MODULE_LEVEL,
    }
}

fn find_policy(method: &Method, template: &str) -> Option<&'static RoutePolicy> {
    let name = method.as_str();
    POLICIES
        .iter()
        .find(|policy| policy.template == template && policy.method == name)
        .or_else(|| {
            if name == "HEAD" {
                POLICIES
                    .iter()
                    .find(|policy| policy.template == template && policy.method == "GET")
            } else {
                None
            }
        })
}

fn template_known(template: &str) -> bool {
    POLICIES.iter().any(|policy| policy.template == template)
}

fn mounted_namespace(path: &str, namespaces: &BTreeSet<String>) -> bool {
    namespaces.iter().any(|namespace| {
        path.strip_prefix(namespace.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}
