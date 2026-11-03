#![allow(clippy::unwrap_used)]
use super::*;
use crate::capabilities::first_party::test_peer;
use crate::enroll_api::EnrollAuthority;
use crate::server::{build_router, tests as server_tests};
use axum::http::{Method, header};
use http_body_util::BodyExt;
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
    // C1b: the lock's contract travels in the detail. No supervisor means no health book.
    let hyphae = find(&body, "hyphae");
    assert_eq!(hyphae["state"], "absent");
    assert_eq!(
        hyphae["detail"]["contract"],
        json!({"name": "hyphae-cli", "major": 1, "min_minor": 0})
    );
    assert!(hyphae["detail"].get("health").is_none());
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

fn health_book(degraded: bool, strikes: u32, relays_ok: bool) -> agent24_comm::HealthView {
    use agent24_comm::{CheckView, HealthView};
    let mut checks = std::collections::BTreeMap::new();
    checks.insert(
        "relays".into(),
        CheckView {
            ok: relays_ok,
            consecutive_failures: if relays_ok { 0 } else { strikes },
        },
    );
    checks.insert(
        "keystore".into(),
        CheckView {
            ok: true,
            consecutive_failures: 0,
        },
    );
    HealthView {
        consecutive_failures: if relays_ok { 0 } else { strikes },
        degraded,
        failed_probes: if relays_ok {
            Vec::new()
        } else {
            vec!["relays".into()]
        },
        checks,
    }
}

fn probe_of(
    contract: agent24_comm::ContractView,
    health: Option<agent24_comm::HealthView>,
) -> agent24_comm::ProbeSnapshot {
    agent24_comm::ProbeSnapshot {
        contract,
        above_tested: false,
        health,
    }
}

fn hyphae_with(
    process: &str,
    process_reason: Option<&str>,
    probe: agent24_comm::ProbeSnapshot,
) -> Item {
    let lock = crate::hyphae_install::HyphaeLock::embedded().unwrap();
    let component = lock.component();
    let mut item = Item::new("hyphae", Form::Resident, ComponentState::Absent).with(|item| {
        item.provides = Item::provides_of(component);
        item.version = Some(component.version.clone());
        item.detail = Some(json!({
            "contract": {
                "name": component.contract.name,
                "major": component.contract.major,
                "min_minor": component.contract.min_minor,
            }
        }));
    });
    apply_hyphae_runtime(
        &mut item,
        process,
        process_reason.map(str::to_owned),
        7,
        0,
        &probe,
    );
    item
}

#[test]
fn one_health_failure_stays_ready_until_the_book_says_degraded() {
    use ComponentState as S;
    use agent24_comm::ContractView;

    let untouched = hyphae_with("running", None, probe_of(ContractView::Compatible, None));
    assert_eq!(untouched.state, S::Ready, "no sample yet is still ready");
    assert!(
        render(&[untouched], true)["components"][0]["detail"]
            .get("health")
            .is_none()
    );

    let one = hyphae_with(
        "running",
        None,
        probe_of(ContractView::Compatible, Some(health_book(false, 1, false))),
    );
    assert_eq!(one.state, S::Ready);
    assert!(one.reason.is_none());
    let body = render(&[one], true);
    let health = &body["components"][0]["detail"]["health"];
    assert_eq!(health["degraded"], false);
    assert_eq!(health["consecutive_failures"], 1);
    assert_eq!(health["checks"]["relays"]["ok"], false);
    assert_eq!(health["checks"]["relays"]["consecutive_failures"], 1);
    assert_eq!(health["checks"]["keystore"]["consecutive_failures"], 0);
    assert_eq!(body["components"][0]["detail"]["consecutive_failures"], 0);
    assert!(!body.to_string().contains("wss://"));

    let nth = hyphae_with(
        "running",
        None,
        probe_of(ContractView::Compatible, Some(health_book(true, 3, false))),
    );
    assert_eq!(nth.state, S::Degraded);
    assert_eq!(nth.reason.as_deref(), Some("relays"));

    let mut both = health_book(true, 3, false);
    both.failed_probes = vec!["keystore".into(), "relays".into()];
    both.checks.get_mut("keystore").unwrap().ok = false;
    let named = hyphae_with(
        "running",
        None,
        probe_of(ContractView::Compatible, Some(both)),
    );
    assert_eq!(named.reason.as_deref(), Some("keystore,relays"));

    let back = hyphae_with(
        "running",
        None,
        probe_of(ContractView::Compatible, Some(health_book(false, 0, true))),
    );
    assert_eq!(back.state, S::Ready);
    let body = render(&[back], true);
    assert_eq!(body["components"][0]["detail"]["health"]["degraded"], false);
    assert_eq!(
        body["components"][0]["detail"]["health"]["consecutive_failures"],
        0
    );
    assert_eq!(
        body["components"][0]["detail"]["health"]["checks"]["relays"]["ok"],
        true
    );
}

#[test]
fn contract_mismatch_is_incompatible_even_when_the_process_gave_up() {
    use agent24_comm::ContractView;
    let item = hyphae_with(
        "gave_up",
        Some("contract_mismatch: expected hyphae-cli major 1, got hyphae-cli 2.0"),
        probe_of(ContractView::Mismatch, None),
    );
    assert_eq!(item.state, ComponentState::Incompatible);
    assert_eq!(item.reason.as_deref(), Some("contract_mismatch"));
}

#[test]
fn contract_check_failure_is_failed_without_claiming_incompatible() {
    use agent24_comm::ContractView;
    let item = hyphae_with(
        "gave_up",
        Some("contract_check_failed: timed out"),
        probe_of(ContractView::CheckFailed, None),
    );
    assert_eq!(item.state, ComponentState::Failed);
    assert_eq!(item.reason.as_deref(), Some("contract_check_failed"));
}

#[test]
fn a_stopped_process_ignores_a_stale_degraded_health_book() {
    use agent24_comm::ContractView;
    let item = hyphae_with(
        "stopped",
        None,
        probe_of(ContractView::Compatible, Some(health_book(true, 3, false))),
    );
    assert_eq!(item.state, ComponentState::Stopped);
    assert!(item.reason.is_none());
    assert!(
        render(&[item], true)["components"][0]["detail"]
            .get("health")
            .is_none()
    );
}

#[test]
fn redacted_components_omit_the_health_summary() {
    use agent24_comm::ContractView;
    let item = hyphae_with(
        "running",
        None,
        probe_of(ContractView::Compatible, Some(health_book(true, 3, false))),
    );
    let full = render(std::slice::from_ref(&item), true);
    let redacted = render(&[item], false);
    let text = redacted.to_string();
    assert_eq!(redacted["components"][0]["state"], "degraded");
    assert!(text.contains("nostr-relay"));
    for forbidden in [
        "consecutive_failures",
        "relays",
        "detail",
        "health",
        "contract_mismatch",
        "reason",
        "keystore",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} leaked into the redacted view: {text}"
        );
    }
    assert_eq!(
        full["components"][0]["detail"]["health"]["checks"]["relays"]["consecutive_failures"],
        3
    );
    assert_eq!(full["components"][0]["reason"], "relays");
}
