//! T2: the design doc's §3 table is the oracle, not a second copy of the const.

use std::collections::BTreeSet;

use axum::http::Method;

use super::*;

const DESIGN: &str = include_str!("../../../../../../docs/agent/CORE-2-ENROLL-DESIGN.md");

struct DesignRow {
    method: String,
    path: String,
    operation: String,
    level: AssuranceLevel,
}

fn design_tables() -> (Vec<DesignRow>, Vec<DesignRow>) {
    let mut tables = Vec::new();
    let mut current: Option<Vec<DesignRow>> = None;
    for line in DESIGN.lines() {
        let cells: Vec<&str> = line
            .split('|')
            .map(str::trim)
            .filter(|cell| !cell.is_empty())
            .collect();
        if cells.len() == 4 && cells[0] == "方法" {
            if let Some(table) = current.take() {
                tables.push(table);
            }
            current = Some(Vec::new());
            continue;
        }
        let Some(table) = current.as_mut() else {
            continue;
        };
        if cells.len() != 4 || cells[0].starts_with('-') {
            if !line.starts_with('|')
                && let Some(table) = current.take()
            {
                tables.push(table);
            }
            continue;
        }
        let level = if cells[3].starts_with("公开") {
            AssuranceLevel::Public
        } else if cells[3].starts_with("日常") {
            AssuranceLevel::Everyday
        } else if cells[3].starts_with("提升") {
            AssuranceLevel::Elevated
        } else {
            panic!("unknown level {}", cells[3]);
        };
        let operation = cells[2]
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .next()
            .unwrap()
            .to_owned();
        table.push(DesignRow {
            method: cells[0].to_owned(),
            path: cells[1].to_owned(),
            operation,
            level,
        });
    }
    if let Some(table) = current.take() {
        tables.push(table);
    }
    assert_eq!(tables.len(), 2, "§3 has a fixed table and a module table");
    let module = tables.pop().unwrap();
    let fixed = tables.pop().unwrap();
    (fixed, module)
}

fn method(name: &str) -> Method {
    Method::from_bytes(name.as_bytes()).unwrap()
}

#[test]
fn section_3_rows_match_operation_and_level() {
    let (fixed, module) = design_tables();
    assert_eq!(fixed.len(), 93);
    assert_eq!(module.len(), 2);

    let mut catalog = BTreeSet::new();
    for policy in policies() {
        assert!(
            catalog.insert((policy.method, policy.template)),
            "duplicate {} {}",
            policy.method,
            policy.template
        );
    }
    assert_eq!(catalog.len(), fixed.len());

    for row in &fixed {
        let template = format!("/api/v1{}", row.path);
        let policy = policies()
            .iter()
            .find(|policy| policy.method == row.method && policy.template == template)
            .unwrap_or_else(|| panic!("missing {} {}", row.method, template));
        assert_eq!(format!("{:?}", policy.operation), row.operation);
        assert_eq!(policy.level, row.level, "{} {}", row.method, template);
        assert_eq!(
            policy.operation.assurance_level(),
            Some(row.level),
            "{:?}",
            policy.operation
        );
        assert_eq!(
            required_operation(&method(&row.method), &template),
            Some(policy.operation)
        );
        assert_eq!(
            Operation::from_action(policy.operation.action()),
            Some(policy.operation)
        );
    }

    for row in &module {
        assert_eq!(row.operation, "ModuleRoute");
        assert_eq!(row.level, AssuranceLevel::Everyday);
    }
    assert_eq!(
        Operation::ModuleRoute.assurance_level(),
        Some(AssuranceLevel::Everyday)
    );
}

#[test]
fn unmapped_operations_have_no_level_and_only_models_are_global() {
    for operation in [
        Operation::ModelsAdmin,
        Operation::WorkspaceCreate,
        Operation::WorkspaceRelease,
        Operation::HostLease,
        Operation::WorkspaceResolve,
        Operation::SessionTranscript,
    ] {
        assert!(operation.assurance_level().is_none(), "{operation:?}");
        assert!(
            !policies()
                .iter()
                .any(|policy| policy.operation == operation)
        );
    }
    for operation in Operation::creative_allowlist() {
        let global = operation == Operation::ModelsRead;
        assert_eq!(operation.scope() == ResourceScope::Global, global);
    }
    assert_eq!(Operation::ChatCreate.scope(), ResourceScope::Principal);
    assert_eq!(Operation::ModuleRoute.scope(), ResourceScope::Principal);
}

#[test]
fn creative_allowlist_is_unchanged_and_new_routes_stay_closed() {
    assert_eq!(
        Operation::creative_allowlist(),
        BTreeSet::from([
            Operation::ModelsRead,
            Operation::WorkspaceResolve,
            Operation::SessionCreate,
            Operation::SessionRead,
            Operation::SessionTranscript,
            Operation::RunCreate,
            Operation::RunRead,
            Operation::RunCancel,
            Operation::EventsRead,
            Operation::ApprovalStatusRead,
        ])
    );
    for policy in policies() {
        let permitted = policy.method == "GET" && policy.template == "/api/v1/models";
        assert_eq!(
            creative_route_permitted(&method(policy.method), policy.template),
            permitted,
            "{} {}",
            policy.method,
            policy.template
        );
        if policy.method == "GET" {
            assert!(
                !creative_route_permitted(&Method::HEAD, policy.template),
                "HEAD {} must stay closed for Creative",
                policy.template
            );
        }
    }
    for action in [
        "models/read",
        "workspace/resolve",
        "session/new",
        "session/load",
        "session/get",
        "session/transcript",
        "run/new",
        "run/get",
        "run/cancel",
        "events/subscribe",
        "approval/read",
        "chat.create",
        "comm.write",
        "consent.revoke",
    ] {
        assert!(Operation::from_action(action).is_some(), "{action}");
    }
    assert!(Operation::from_action("not.an.operation").is_none());
}

#[test]
fn method_splits_shutdown_and_memory_settings() {
    assert_eq!(
        required_operation(&Method::POST, "/api/v1/shutdown"),
        Some(Operation::Shutdown)
    );
    assert_eq!(
        required_operation(&Method::GET, "/api/v1/shutdown"),
        Some(Operation::ConfigRead)
    );
    assert_eq!(
        required_operation(&Method::HEAD, "/api/v1/shutdown"),
        Some(Operation::ConfigRead)
    );
    assert_eq!(
        Operation::Shutdown.assurance_level(),
        Some(AssuranceLevel::Elevated)
    );
    assert_eq!(
        required_operation(&Method::GET, "/api/v1/memory/settings"),
        Some(Operation::MemoryRead)
    );
    assert_eq!(
        required_operation(&Method::PUT, "/api/v1/memory/settings"),
        Some(Operation::MemorySettingsWrite)
    );
    assert_eq!(
        Operation::MemoryRead.assurance_level(),
        Some(AssuranceLevel::Everyday)
    );
    assert_eq!(
        Operation::MemorySettingsWrite.assurance_level(),
        Some(AssuranceLevel::Elevated)
    );
    assert_eq!(
        required_operation(&Method::POST, "/api/v1/comm/inbox/pull"),
        Some(Operation::CommWrite)
    );
    assert_eq!(
        Operation::CommWrite.assurance_level(),
        Some(AssuranceLevel::Elevated)
    );
    assert_eq!(
        Operation::ConsentRevoke.assurance_level(),
        Some(AssuranceLevel::Everyday)
    );
    assert_eq!(
        Operation::ScheduleManage.assurance_level(),
        Some(AssuranceLevel::Elevated)
    );
    assert_eq!(
        Operation::ApprovalDecision.assurance_level(),
        Some(AssuranceLevel::Elevated)
    );
}

#[test]
fn decide_separates_methods_modules_and_unknown_paths() {
    let namespaces = BTreeSet::from(["/api/v1/probe".to_owned()]);
    assert!(matches!(
        decide(
            &Method::DELETE,
            Some("/api/v1/health"),
            "/api/v1/health",
            &namespaces
        ),
        RouteDecision::MethodNotAllowed
    ));
    assert!(matches!(
        decide(
            &Method::GET,
            Some("/api/v1/probe/secret"),
            "/api/v1/probe/secret",
            &BTreeSet::new()
        ),
        RouteDecision::Module {
            operation: Operation::ModuleRoute,
            level: AssuranceLevel::Everyday
        }
    ));
    assert!(matches!(
        decide(&Method::POST, None, "/api/v1/probe/secret", &namespaces),
        RouteDecision::Module {
            operation: Operation::ModuleRoute,
            ..
        }
    ));
    assert!(matches!(
        decide(&Method::GET, None, "/api/v1/probe", &namespaces),
        RouteDecision::Module { .. }
    ));
    assert_eq!(
        decide(&Method::GET, None, "/api/v1/probe-x", &namespaces),
        RouteDecision::UnknownPath
    );
    assert_eq!(
        decide(&Method::GET, None, "/api/v1/missing", &BTreeSet::new()),
        RouteDecision::UnknownPath
    );
    assert!(matches!(
        decide(&Method::HEAD, Some("/api/v1/models"), "/api/v1/models", &BTreeSet::new()),
        RouteDecision::Mapped(policy) if policy.operation == Operation::ModelsRead
    ));
}
