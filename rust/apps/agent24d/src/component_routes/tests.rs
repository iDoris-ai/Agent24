#![allow(clippy::unwrap_used)]
use std::sync::Arc;

use super::*;
use crate::capabilities::first_party::test_peer;
use crate::domain::MountOutcome;
use crate::enroll_api::EnrollAuthority;
use crate::server::{build_router, tests as server_tests};
use axum::http::{Method, header};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// `(app, host, creative, first_party)` with capability auth.
async fn app() -> (axum::Router, String, String, String) {
    let (state, host, creative) = server_tests::capability_state().await;
    let first_party = state
        .first_party
        .as_ref()
        .unwrap()
        .issue(&test_peer("daemon-test"))
        .unwrap()
        .secret
        .to_string();
    (build_router(state), host, creative, first_party)
}

fn find<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no {id} in {body}"))
}

const GET: &str = "/api/v1/components";
const OD: &str = "/api/v1/components/open-design/report";

#[tokio::test]
async fn the_host_sees_everything_and_open_design_is_absent_until_reported() {
    let (app, host, ..) = app().await;
    let (status, body) = call(&app, Method::GET, GET, Some(&host), "").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<_> = body["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["idoris", "hyphae", "documents", "knowledge", "open-design"]
    );
    let idoris = find(&body, "idoris");
    assert_eq!(idoris["form"], "resident");
    assert_eq!(idoris["provides"][0]["id"], "model-gateway");
    // C1b: the lock's contract travels in the detail, and Hyphae has no `health`.
    let hyphae = find(&body, "hyphae");
    assert_eq!(hyphae["state"], "absent");
    assert_eq!(
        hyphae["detail"]["contract"],
        json!({"name": "hyphae-cli", "major": 1, "min_minor": 0})
    );
    let provided: Vec<_> = hyphae["provides"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        provided,
        ["nostr-dm", "nostr-group", "nostr-identity", "nostr-relay"]
    );
    let od = find(&body, "open-design");
    assert_eq!(
        (
            od["state"].as_str(),
            od["form"].as_str(),
            od["reason"].as_str()
        ),
        (Some("absent"), Some("sidecar"), Some("not_reported"))
    );
    assert_eq!(find(&body, "knowledge")["state"], "stopped");
    assert_eq!(find(&body, "documents")["state"], "absent");
}

#[tokio::test]
async fn other_audiences_get_only_id_version_state_provides() {
    let (app, host, creative, first_party) = app().await;
    // Make the host view carry every sensitive kind of field, so absence below means something.
    let body = r#"{"state":"failed","version":"od1234567","reason":"crash at /Users/x/od ws://relay.example"}"#;
    assert_eq!(
        call(&app, Method::POST, OD, Some(&host), body).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, full) = call(&app, Method::GET, GET, Some(&host), "").await;
    assert_eq!(
        find(&full, "open-design")["reason"],
        "crash at /Users/x/od ws://relay.example"
    );
    assert!(find(&full, "hyphae").get("detail").is_some());

    let (status, redacted) = call(&app, Method::GET, GET, Some(&first_party), "").await;
    assert_eq!(status, StatusCode::OK);
    let text = redacted.to_string();
    for leaked in [
        "detail",
        "reason",
        "form",
        "/Users/x",
        "relay.example",
        "contract",
        "port",
        "pid",
    ] {
        assert!(!text.contains(leaked), "{leaked} leaked: {text}");
    }
    for item in redacted["components"].as_array().unwrap() {
        let mut keys: Vec<_> = item.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["id", "provides", "state", "version"], "{item}");
    }
    let od = find(&redacted, "open-design");
    assert_eq!(
        (od["state"].as_str(), od["version"].as_str()),
        (Some("failed"), Some("od1234567"))
    );
    assert_eq!(
        find(&redacted, "idoris")["provides"][0]["id"],
        "model-gateway"
    );

    // Creative is refused (not on its allowlist); no token is 401.
    assert_eq!(
        call(&app, Method::GET, GET, Some(&creative), "").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, Method::GET, GET, None, "").await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn legacy_mode_reads_the_full_view_but_cannot_report() {
    let app = build_router(server_tests::state().await);
    let (status, body) = call(&app, Method::GET, GET, Some("testtoken"), "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(find(&body, "hyphae").get("detail").is_some());
    let state = r#"{"state":"installed"}"#;
    assert_eq!(
        call(&app, Method::POST, OD, Some("testtoken"), state)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, Method::GET, GET, None, "").await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, body) = call(&app, Method::GET, GET, Some("testtoken"), "").await;
    assert_eq!(
        find(&body, "open-design")["state"],
        "absent",
        "a refused report stores nothing"
    );
}

#[tokio::test]
async fn only_the_host_reports_and_only_for_desktop_owned_components() {
    let (app, host, creative, first_party) = app().await;
    let ok = r#"{"state":"installed","version":"od1234567"}"#;
    for token in [&creative, &first_party] {
        assert_eq!(
            call(&app, Method::POST, OD, Some(token), ok).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(&app, Method::POST, OD, None, ok).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, body) = call(&app, Method::GET, GET, Some(&host), "").await;
    assert_eq!(
        find(&body, "open-design")["state"],
        "absent",
        "refused reports store nothing"
    );

    // 404 comes before the body is looked at
    assert_eq!(
        call(
            &app,
            Method::POST,
            "/api/v1/components/nope/report",
            Some(&host),
            "not json"
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for id in ["idoris", "hyphae", "documents", "knowledge", "nope"] {
        let path = format!("/api/v1/components/{id}/report");
        assert_eq!(
            call(&app, Method::POST, &path, Some(&host), ok).await.0,
            StatusCode::NOT_FOUND,
            "{id}"
        );
    }
    // strict schema
    for bad in [
        r#"{"state":"ready","pid":1}"#,
        r#"{"state":"READY"}"#,
        r#"{"state":"warm"}"#,
        r#"{}"#,
        r#"{"state":"ready","version":"a b"}"#,
        r#"{"state":"ready","reason":"x\u0000"}"#,
    ] {
        assert_eq!(
            call(&app, Method::POST, OD, Some(&host), bad).await.0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    assert_eq!(
        call(&app, Method::POST, OD, Some(&host), ok).await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn any_legal_state_is_stored_and_an_unexpected_edge_is_marked() {
    let (app, host, _, first_party) = app().await;
    let post = |body: &'static str| {
        let (app, host) = (app.clone(), host.clone());
        async move { call(&app, Method::POST, OD, Some(&host), body).await }
    };
    let get = || {
        let (app, host) = (app.clone(), host.clone());
        async move {
            let (_, now) = call(&app, Method::GET, GET, Some(&host), "").await;
            find(&now, "open-design").clone()
        }
    };
    // First report is the baseline, whatever it is.
    assert_eq!(
        post(r#"{"state":"installed","version":"od1234567"}"#)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(get().await.get("detail").is_none());
    // installed -> ready skips `starting`: accepted and shown (never a stale installed),
    // marked for the host, and the version survives a report that omits it.
    assert_eq!(post(r#"{"state":"ready"}"#).await.0, StatusCode::NO_CONTENT);
    let od = get().await;
    assert_eq!(
        (od["state"].as_str(), od["version"].as_str()),
        (Some("ready"), Some("od1234567"))
    );
    assert_eq!(
        od["detail"]["unexpected_transition"],
        json!({"from": "installed", "to": "ready"})
    );
    // a legal edge clears the mark
    assert_eq!(
        post(r#"{"state":"degraded","reason":"probe"}"#).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(get().await.get("detail").is_none());
    // paths missing from the table (stopped -> absent, ready -> stopped) are stored too
    assert_eq!(
        post(r#"{"state":"stopped"}"#).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        get().await["detail"]["unexpected_transition"]["to"],
        "stopped"
    );
    assert_eq!(
        post(r#"{"state":"absent"}"#).await.0,
        StatusCode::NO_CONTENT
    );
    let od = get().await;
    assert_eq!(od["state"], "absent");
    assert_eq!(od["version"], Value::Null, "absent carries no version");
    // the mark is host-only
    assert_eq!(post(r#"{"state":"ready"}"#).await.0, StatusCode::NO_CONTENT);
    let (_, fp) = call(&app, Method::GET, GET, Some(&first_party), "").await;
    assert!(!fp.to_string().contains("unexpected"), "{fp}");
    // the redacted view never shows the mark
}

#[test]
fn a_report_expires_after_the_ttl_and_the_next_one_is_a_baseline() {
    let reports = ComponentReports::default();
    let t0 = Instant::now();
    let body = |state| ReportBody {
        state,
        version: None,
        reason: None,
    };
    reports
        .report("open-design", body(ComponentState::Installed), t0)
        .unwrap();
    assert!(
        reports
            .current("open-design", t0 + REPORT_TTL - Duration::from_secs(1))
            .is_some()
    );
    assert!(
        reports.current("open-design", t0 + REPORT_TTL).is_none(),
        "gone at the TTL"
    );
    // within the TTL installed -> ready is stored but unexpected; after it, a baseline
    let r30 = reports.report(
        "open-design",
        body(ComponentState::Ready),
        t0 + Duration::from_secs(30),
    );
    assert_eq!(
        r30,
        Ok(Some((ComponentState::Installed, ComponentState::Ready)))
    );
    // that report renewed the TTL: it is live at t0+30+TTL-1 and expired at t0+30+TTL
    let live = t0 + Duration::from_secs(30) + REPORT_TTL;
    assert!(
        reports
            .current("open-design", live - Duration::from_secs(1))
            .is_some()
    );
    assert!(reports.current("open-design", live).is_none());
    assert_eq!(
        reports.report("open-design", body(ComponentState::Stopped), live),
        Ok(None),
        "nothing live to compare to"
    );
    let t0 = live;
    // a heartbeat renews it
    reports
        .report(
            "open-design",
            body(ComponentState::Ready),
            t0 + REPORT_TTL - Duration::from_secs(1),
        )
        .unwrap();
    assert!(
        reports
            .current("open-design", t0 + REPORT_TTL + Duration::from_secs(30))
            .is_some()
    );
    assert_eq!(
        reports.report("idoris", body(ComponentState::Ready), t0),
        Err(ReportError::UnknownComponent)
    );
}

#[test]
fn only_a_host_or_legacy_authorization_sees_the_full_view() {
    use crate::capabilities::{CapabilityStore, Resource};
    let store = CapabilityStore::new("daemon-test");
    let now = crate::capabilities::unix_now();
    let host = store
        .mint_product_host("h", Duration::from_secs(60), now)
        .unwrap();
    let (bearer, _, _) = host.into_bearer_parts();
    let auth = store
        .validate_bearer(
            &bearer,
            crate::capabilities::Operation::ModelsRead,
            &Resource::global(),
            now,
        )
        .unwrap();
    assert!(sees_everything(AuthMode::Capabilities, Some(&auth)));
    let mut shell = auth.clone();
    shell.claims.audience = Audience::FirstPartyShell;
    assert!(!sees_everything(AuthMode::Capabilities, Some(&shell)));
    shell.claims.audience = Audience::CreativeRuntime;
    assert!(!sees_everything(AuthMode::Capabilities, Some(&shell)));
    assert!(
        !sees_everything(AuthMode::Capabilities, None),
        "fail closed"
    );
    assert!(sees_everything(AuthMode::LegacySingleToken, None));
}

#[test]
fn creative_may_neither_read_nor_report_components() {
    use crate::capabilities::Operation;
    let allowed = Operation::creative_allowlist();
    assert!(!allowed.contains(&Operation::ComponentsRead));
    assert!(!allowed.contains(&Operation::ComponentsReport));
    assert_eq!(
        Operation::ComponentsRead.assurance_level(),
        Some(crate::capabilities::AssuranceLevel::Everyday)
    );
}

#[test]
fn the_mapping_rows() {
    use ComponentState as S;
    // D5 table: DrainState
    assert_eq!(from_drain(DrainState::Starting), S::Starting);
    assert_eq!(from_drain(DrainState::Running), S::Ready);
    assert_eq!(from_drain(DrainState::Draining), S::Stopping);
    assert_eq!(from_drain(DrainState::Revoked), S::Stopping);
    // attach subset: a revoked generation is a dropped connection
    assert_eq!(attach_state(false, None).0, S::Stopped);
    assert_eq!(attach_state(false, Some(DrainState::Revoked)).0, S::Stopped);
    assert_eq!(
        attach_state(false, Some(DrainState::Starting)).0,
        S::Starting
    );
    assert_eq!(attach_state(false, Some(DrainState::Running)).0, S::Ready);
    assert_eq!(
        attach_state(false, Some(DrainState::Draining)).0,
        S::Stopping
    );
    assert_eq!(
        attach_state(true, Some(DrainState::Running)),
        (S::Stopped, Some("disabled"))
    );
    // Hyphae: pid liveness only, never degraded
    for (process, want) in [
        ("running", S::Ready),
        ("stopped", S::Stopped),
        ("not_configured", S::Stopped),
        ("backoff", S::Failed),
        ("gave_up", S::Failed),
        ("locked", S::Failed),
    ] {
        let (mapped, _) = from_hyphae(process);
        assert_eq!(mapped, want, "{process}");
        assert_ne!(mapped, S::Degraded);
    }
    // iDoris
    for (state, want) in [
        (IdorisState::NotInstalled, S::Absent),
        (IdorisState::NotConfigured, S::Installed),
        (IdorisState::Stopped, S::Stopped),
        (IdorisState::Starting, S::Starting),
        (IdorisState::Running, S::Ready),
        (IdorisState::Failed, S::Failed),
        (IdorisState::InstanceConflict, S::Failed),
    ] {
        assert_eq!(from_idoris(state, false).0, want);
        assert_eq!(
            from_idoris(state, true).0,
            S::Installing,
            "an install in flight wins"
        );
    }
    // supervised packages
    assert_eq!(from_supervised(&Supervised::Running).0, S::Ready);
    assert_eq!(from_supervised(&Supervised::Stopped).0, S::Stopped);
    assert_eq!(from_supervised(&Supervised::Stopping).0, S::Stopping);
    assert_eq!(from_supervised(&Supervised::Panicked).0, S::Failed);
}

#[test]
fn knowledge_is_ready_only_while_a_binding_is_active() {
    use crate::knowledge_host::Change;
    let registry = crate::knowledge_host::KnowledgeHostRegistry::default();
    assert!(!registry.any_active());
    registry.apply("host", "b", 1, Change::Activate).unwrap();
    assert!(registry.any_active());
    registry.apply("host", "b", 2, Change::Deactivate).unwrap();
    assert!(!registry.any_active());
}

#[test]
fn attached_entries_named_like_a_builtin_component_are_not_listed() {
    let modules = [
        "open-design",
        "agentear",
        "knowledge",
        "hyphae",
        "documents",
        "idoris",
    ]
    .map(|n| (n.to_owned(), false, Some(DrainState::Running)))
    .to_vec();
    let items = attach_items_from(modules);
    let ids: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(
        ids,
        ["agentear"],
        "a forged entry is dropped, a real one stays"
    );
}

/// Many threads report through one `ComponentReports`. `ready` and `degraded` follow each other
/// by D5 edges in both directions, so whatever the interleaving no report may be flagged
/// unexpected, and what is read afterwards is a state somebody reported. `report` reads the
/// previous entry and writes the new one under a single guard, so there is no window between
/// the check and the store for another report to slip into.
#[test]
fn concurrent_reports_are_serialised_and_never_misflagged() {
    use std::sync::Arc;
    let reports = Arc::new(ComponentReports::default());
    let t0 = Instant::now();
    reports
        .report(
            "open-design",
            ReportBody {
                state: ComponentState::Ready,
                version: None,
                reason: None,
            },
            t0,
        )
        .unwrap();
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let reports = Arc::clone(&reports);
            std::thread::spawn(move || {
                let mut flagged = 0;
                for n in 0..500 {
                    let state = if (n + i) % 2 == 0 {
                        ComponentState::Ready
                    } else {
                        ComponentState::Degraded
                    };
                    let now = t0 + Duration::from_millis(n);
                    if reports
                        .report(
                            "open-design",
                            ReportBody {
                                state,
                                version: None,
                                reason: None,
                            },
                            now,
                        )
                        .unwrap()
                        .is_some()
                    {
                        flagged += 1;
                    }
                }
                flagged
            })
        })
        .collect();
    let flagged: u32 = threads.into_iter().map(|t| t.join().unwrap()).sum();
    assert_eq!(flagged, 0);
    let last = reports
        .current("open-design", t0 + Duration::from_millis(500))
        .unwrap();
    assert!(matches!(
        last.state,
        ComponentState::Ready | ComponentState::Degraded
    ));
}

fn os_report(
    name: &str,
    outcome: crate::domain::MountOutcome,
    d9: crate::component_manifest::OsD9,
) -> crate::domain::MountReport {
    crate::domain::MountReport {
        name: name.to_owned(),
        namespace: format!("/api/v1/{name}"),
        version: "0.1.0".to_owned(),
        enabled_at_start: Some(!matches!(outcome, crate::domain::MountOutcome::Disabled)),
        outcome,
        granted: Vec::new(),
        resources: crate::domain::ResourceStatus::NotChecked,
        d9: Some(d9),
    }
}

fn d9(
    name: &str,
    in_process: bool,
    provide: &str,
    kernel: &[&str],
) -> crate::component_manifest::OsD9 {
    crate::component_manifest::OsD9 {
        name: name.to_owned(),
        in_process,
        provides: vec![crate::component_manifest::CapProvide {
            id: provide.to_owned(),
            tools: vec![format!("{name}.ping")],
        }],
        requires_kernel: kernel.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[tokio::test]
async fn domain_os_and_mcp_show_up_with_a_list_and_a_d5_state() {
    use agent24_os_proto::supervisor::Status;
    let (mut state, host, _) = server_tests::capability_state().await;
    let (_tx, rx) = tokio::sync::watch::channel(Status::Stopped);
    state.module_status = Arc::new(std::collections::HashMap::from([("sin90".to_owned(), rx)]));
    state.os_reports = Arc::new(vec![
        os_report(
            "documents",
            MountOutcome::Mounted,
            d9("documents", false, "should-not-leak", &[]),
        ),
        os_report(
            "demo",
            MountOutcome::Mounted,
            d9("demo", true, "demo-tasks", &["events"]),
        ),
        os_report(
            "sin90",
            MountOutcome::Mounted,
            d9("sin90", false, "sin90-tasks", &["events"]),
        ),
        os_report(
            "cos72",
            MountOutcome::Disabled,
            d9("cos72", false, "cos72-tasks", &[]),
        ),
        os_report(
            "broken",
            MountOutcome::Degraded("/tmp/secret-os-path".into()),
            d9("broken", false, "broken-tasks", &["memory"]),
        ),
    ]);
    state.mcp_roster = Arc::new(crate::mcp::McpRoster::from_records(vec![
        crate::mcp::McpRecord {
            server: "files".to_owned(),
            id: "mcp-files".to_owned(),
            state: ComponentState::Ready,
            reason: None,
            capability_id: Some("mcp-files".to_owned()),
            tools: vec!["mcp_files_echo".to_owned()],
            protocol_version: Some("2024-11-05".to_owned()),
        },
        crate::mcp::McpRecord {
            server: "!!!".to_owned(),
            id: "mcp-bad-00000001".to_owned(),
            state: ComponentState::Failed,
            reason: Some("invalid_capability_id".to_owned()),
            capability_id: None,
            tools: Vec::new(),
            protocol_version: None,
        },
    ]));
    let app = build_router(state);
    let (status, body) = call(&app, Method::GET, GET, Some(&host), "").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<_> = body["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "idoris",
            "hyphae",
            "documents",
            "broken",
            "cos72",
            "demo",
            "sin90",
            "knowledge",
            "open-design",
            "mcp-bad-00000001",
            "mcp-files",
        ]
    );
    assert_eq!(
        body["components"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["id"] == "documents")
            .count(),
        1
    );
    assert!(
        find(&body, "documents")["provides"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let demo = find(&body, "demo");
    assert_eq!(demo["state"], "ready");
    assert_eq!(demo["form"], "resident");
    assert_eq!(demo["version"], "0.1.0");
    assert_eq!(demo["detail"]["binding"], "in_process");
    assert_eq!(demo["detail"]["requires"]["kernel"], json!(["events"]));
    assert_eq!(demo["provides"][0]["id"], "demo-tasks");
    assert_eq!(demo["provides"][0]["tools"], json!(["demo.ping"]));

    let sin90 = find(&body, "sin90");
    assert_eq!(sin90["state"], "stopped");
    assert_eq!(sin90["form"], "resident");
    assert_eq!(sin90["detail"]["binding"], "package");
    assert_eq!(sin90["provides"][0]["id"], "sin90-tasks");

    let cos72 = find(&body, "cos72");
    assert_eq!(cos72["state"], "stopped");
    assert_eq!(cos72["reason"], "disabled");

    let broken = find(&body, "broken");
    assert_eq!(broken["state"], "failed");
    assert_eq!(broken["reason"], "degraded");
    assert_eq!(broken["detail"]["message"], "/tmp/secret-os-path");

    let files = find(&body, "mcp-files");
    assert_eq!(files["form"], "mcp");
    assert_eq!(files["state"], "ready");
    assert_eq!(files["provides"][0]["id"], "mcp-files");
    assert_eq!(files["provides"][0]["tools"], json!(["mcp_files_echo"]));
    assert_eq!(files["detail"]["protocol_version"], "2024-11-05");
    assert_eq!(files["detail"]["requires"]["operations"], json!([]));

    let bad = find(&body, "mcp-bad-00000001");
    assert_eq!(bad["state"], "failed");
    assert_eq!(bad["reason"], "invalid_capability_id");
    assert!(bad["provides"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn os_and_mcp_detail_stays_off_the_redacted_view() {
    let (mut state, host, _) = server_tests::capability_state().await;
    let first_party = state
        .first_party
        .as_ref()
        .unwrap()
        .issue(&test_peer("daemon-test"))
        .unwrap()
        .secret
        .to_string();
    state.os_reports = Arc::new(vec![os_report(
        "broken",
        MountOutcome::Degraded("/tmp/secret-os-path".into()),
        d9("broken", false, "broken-tasks", &["memory"]),
    )]);
    state.mcp_roster = Arc::new(crate::mcp::McpRoster::from_records(vec![
        crate::mcp::McpRecord {
            server: "files".to_owned(),
            id: "mcp-files".to_owned(),
            state: ComponentState::Failed,
            reason: Some("spawn_failed".to_owned()),
            capability_id: Some("mcp-files".to_owned()),
            tools: vec!["mcp_files_echo".to_owned()],
            protocol_version: Some("2024-11-05".to_owned()),
        },
    ]));
    let app = build_router(state);
    let (_, full) = call(&app, Method::GET, GET, Some(&host), "").await;
    assert_eq!(
        find(&full, "broken")["detail"]["message"],
        "/tmp/secret-os-path"
    );
    let (status, redacted) = call(&app, Method::GET, GET, Some(&first_party), "").await;
    assert_eq!(status, StatusCode::OK);
    let text = redacted.to_string();
    for leaked in [
        "detail",
        "reason",
        "form",
        "/tmp/secret-os-path",
        "spawn_failed",
        "protocol_version",
        "2024-11-05",
        "requires",
        "binding",
        "in_process",
        "memory",
    ] {
        assert!(!text.contains(leaked), "{leaked} leaked: {text}");
    }
    assert_eq!(find(&redacted, "broken")["state"], "failed");
    assert_eq!(
        find(&redacted, "broken")["provides"][0]["id"],
        "broken-tasks"
    );
    assert_eq!(
        find(&redacted, "mcp-files")["provides"][0]["tools"],
        json!(["mcp_files_echo"])
    );
    for item in redacted["components"].as_array().unwrap() {
        let mut keys: Vec<_> = item.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["id", "provides", "state", "version"]);
    }
}

fn registered_ids(markdown: &str) -> std::collections::BTreeSet<String> {
    markdown
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with('|') {
                return None;
            }
            let cell = line.split('|').nth(1)?.trim().trim_matches('`');
            crate::component_manifest::capability_id_ok(cell).then(|| cell.to_owned())
        })
        .collect()
}

fn mcp_exempt(id: &str) -> bool {
    id.strip_prefix("mcp-")
        .is_some_and(|body| crate::mcp::mcp_capability_id(body).as_deref() == Some(id))
}

fn covered(id: &str, registered: &std::collections::BTreeSet<String>) -> bool {
    registered.contains(id) || mcp_exempt(id)
}

fn provide_ids(body: &Value) -> Vec<String> {
    body["components"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["provides"].as_array().unwrap().iter())
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn registry_lists_every_capability_id_the_components_emit() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let registry_path = root.join("docs/capabilities/REGISTRY.md");
    let markdown = std::fs::read_to_string(&registry_path).unwrap_or_else(|e| {
        panic!("reading {}: {e}", registry_path.display());
    });
    let registered = registered_ids(&markdown);
    for id in [
        "model-gateway",
        "nostr-dm",
        "nostr-group",
        "nostr-identity",
        "nostr-relay",
        "documents",
    ] {
        assert!(registered.contains(id), "{id} is not in REGISTRY.md");
    }
    assert!(
        !registered.contains("mcp-files"),
        "MCP ids are generated, not registered"
    );
    assert!(mcp_exempt("mcp-files"));
    assert!(!mcp_exempt("model-gateway"));
    assert!(!mcp_exempt("mcp-"));

    let mut locks = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "target" || name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk(&path, out);
            } else if name.ends_with(".lock.json") {
                out.push(path);
            }
        }
    }
    walk(&root.join("rust"), &mut locks);
    assert!(!locks.is_empty(), "expected component locks under rust/");
    for path in &locks {
        let text = std::fs::read_to_string(path).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let Some(provides) = value.get("provides").and_then(|p| p.as_array()) else {
            continue;
        };
        for provide in provides {
            let id = provide["id"].as_str().unwrap();
            assert!(
                registered.contains(id),
                "{} provides {id}, which REGISTRY.md does not list",
                path.display()
            );
        }
    }

    fn walk_manifests(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "target" || name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk_manifests(&path, out);
            } else if name == "domain-os.yml" {
                out.push(path);
            }
        }
    }
    let mut manifests = Vec::new();
    walk_manifests(&root, &mut manifests);
    for path in manifests {
        let text = std::fs::read_to_string(&path).unwrap();
        if !text
            .lines()
            .any(|line| line.trim().starts_with("provides:"))
        {
            continue;
        }
        let manifest = agent24_domain::DomainOsManifest::from_yaml(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for provide in manifest.provides() {
            assert!(
                registered.contains(provide.id()),
                "{} provides {}, which REGISTRY.md does not list",
                path.display(),
                provide.id()
            );
        }
    }

    let app = build_router(server_tests::state().await);
    let (_, stock) = call(&app, Method::GET, GET, Some("testtoken"), "").await;
    for id in provide_ids(&stock) {
        assert!(
            covered(&id, &registered),
            "GET /components provides {id}, which is neither registered nor mcp-<server>"
        );
    }

    let mut state = server_tests::state().await;
    state.mcp_roster = Arc::new(crate::mcp::McpRoster::from_records(vec![
        crate::mcp::McpRecord {
            server: "files".to_owned(),
            id: "mcp-files".to_owned(),
            state: ComponentState::Ready,
            reason: None,
            capability_id: Some("mcp-files".to_owned()),
            tools: vec!["mcp_files_echo".to_owned()],
            protocol_version: None,
        },
    ]));
    let app = build_router(state);
    let (_, with_mcp) = call(&app, Method::GET, GET, Some("testtoken"), "").await;
    let ids = provide_ids(&with_mcp);
    assert!(ids.iter().any(|id| id == "mcp-files"));
    for id in ids {
        assert!(
            covered(&id, &registered),
            "GET /components provides {id}, which is neither registered nor mcp-<server>"
        );
    }
}
