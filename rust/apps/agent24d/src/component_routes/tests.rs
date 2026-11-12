#![allow(clippy::unwrap_used)]
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::capabilities::first_party::test_peer;
use crate::domain::MountOutcome;
use crate::enroll_api::EnrollAuthority;
use crate::server::{build_router, tests as server_tests};
use axum::http::{Method, header};
use http_body_util::BodyExt;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;
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

/// The row copies `MountReport.granted`. The resolver would also grant
/// `models` (it is in the out-of-process ceiling and `no_channel` is empty
/// there); a mount with no model callback leaves it out of `granted`. Reading
/// `resolution.granted` instead of `report.granted` makes `kernel_grants`
/// include `models` and this test fails.
#[tokio::test]
async fn components_show_the_mount_grant_not_the_resolver() {
    use crate::capabilities::requires::{self, Consent, GrantForm};

    let (mut state, host, _) = server_tests::capability_state().await;
    let first_party = state
        .first_party
        .as_ref()
        .unwrap()
        .issue(&test_peer("daemon-test"))
        .unwrap()
        .secret
        .to_string();
    let mut report = os_report(
        "modelnodeps",
        MountOutcome::Mounted,
        d9(
            "modelnodeps",
            false,
            "modelnodeps-tasks",
            &["events", "models"],
        ),
    );
    report.granted = vec!["events".into()];
    let spec = requires::RequiresSpec::parse(&["events", "models"], &[] as &[&str]);
    let resolution = requires::resolve(
        &spec,
        GrantForm::OutOfProcess.host_cap(),
        Consent::FirstPartyEnabled,
    );
    assert!(
        resolution.granted.has(agent24_domain::Capability::Models),
        "the resolver grants models; the row must not copy that"
    );
    assert!(resolution.granted.has(agent24_domain::Capability::Events));
    state.os_reports = Arc::new(vec![report]);
    let app = build_router(state);
    let (_, full) = call(&app, Method::GET, GET, Some(&host), "").await;
    let row = find(&full, "modelnodeps");
    assert_eq!(row["kernel_grants"], json!(["events"]));
    assert_eq!(
        row["declared"],
        json!({"kernel": ["events", "models"], "operations": []})
    );
    let denied = row["denied"].as_array().unwrap();
    assert!(
        denied.iter().all(|item| item["item"] != "events"),
        "events was mounted: {denied:?}"
    );
    assert!(
        denied
            .iter()
            .any(|item| item["item"] == "models" && item["code"] == "no_channel"),
        "withheld models is no_channel: {denied:?}"
    );

    let (_, redacted) = call(&app, Method::GET, GET, Some(&first_party), "").await;
    let row = find(&redacted, "modelnodeps");
    assert!(row.get("denied").is_none());
    assert!(row.get("kernel_grants").is_none());
    assert!(!redacted.to_string().contains("no_channel"));
}

fn denial_code<'a>(row: &'a Value, item: &str) -> Option<&'a str> {
    row["denied"]
        .as_array()
        .unwrap()
        .iter()
        .find(|denial| denial["item"] == item)
        .and_then(|denial| denial["code"].as_str())
}

fn has_no_channel(row: &Value) -> bool {
    row["denied"]
        .as_array()
        .unwrap()
        .iter()
        .any(|denial| denial["code"] == "no_channel")
}

/// `no_channel` is a mounted-channel fact. Disabled, refused, degraded, and
/// an uninstalled Documents row keep it out. An unread manifest (no D9) does
/// not grow an out-of-process denial list, and `declared` is null.
#[tokio::test]
async fn no_channel_tracks_the_effective_outcome_not_the_startup_report() {
    let (mut state, host, _) = server_tests::capability_state().await;
    let mut was_mounted = os_report(
        "wasmounted",
        MountOutcome::Mounted,
        d9(
            "wasmounted",
            false,
            "wasmounted-tasks",
            &["events", "models"],
        ),
    );
    was_mounted.granted = vec!["events".into()];
    let now_mounted = os_report(
        "nowmounted",
        MountOutcome::Disabled,
        d9("nowmounted", false, "nowmounted-tasks", &["events"]),
    );
    let refused = os_report(
        "refusedmod",
        MountOutcome::Refused("dup".into()),
        d9("refusedmod", false, "refusedmod-tasks", &["events"]),
    );
    let degraded = os_report(
        "degradedmod",
        MountOutcome::Degraded("disk".into()),
        d9("degradedmod", false, "degradedmod-tasks", &["memory"]),
    );
    let mut unread = os_report(
        "unread",
        MountOutcome::Mounted,
        d9("unread", false, "unread-tasks", &["events", "models"]),
    );
    unread.d9 = None;
    unread.granted = vec!["events".into()];
    state.set_os_outcome("wasmounted", MountOutcome::Disabled);
    state.set_os_outcome("nowmounted", MountOutcome::Mounted);
    state.os_reports = Arc::new(vec![was_mounted, now_mounted, refused, degraded, unread]);
    let app = build_router(state);
    let (_, body) = call(&app, Method::GET, GET, Some(&host), "").await;

    let was = find(&body, "wasmounted");
    assert_eq!(was["state"], "stopped");
    assert_eq!(was["kernel_grants"], json!(["events"]));
    assert!(!has_no_channel(was), "startup Mounted must not win: {was}");
    assert_eq!(denial_code(was, "memory"), Some("not_declared"));

    let now = find(&body, "nowmounted");
    assert_eq!(now["kernel_grants"], json!([]));
    assert_eq!(
        denial_code(now, "events"),
        Some("no_channel"),
        "the override is Mounted, so the empty grant is no_channel: {now}"
    );

    for id in ["refusedmod", "degradedmod", "documents"] {
        let row = find(&body, id);
        assert!(!has_no_channel(row), "{id} is not mounted: {row}");
    }
    assert_eq!(find(&body, "documents")["state"], "absent");
    assert!(
        find(&body, "documents")["denied"]
            .as_array()
            .unwrap()
            .is_empty(),
        "uninstalled Documents does not invent denials"
    );

    let unread = find(&body, "unread");
    assert!(unread["declared"].is_null(), "{unread}");
    assert_eq!(unread["denied"], json!([]));
    assert_eq!(unread["kernel_grants"], json!(["events"]));
    assert!(
        !unread.to_string().contains("not_declared"),
        "an unread manifest does not take the out-of-process ceiling: {unread}"
    );
}

#[test]
fn documents_that_are_not_mounted_keep_static_denials_only() {
    for outcome in [
        MountOutcome::Disabled,
        MountOutcome::Refused("dup".into()),
        MountOutcome::Degraded("disk".into()),
    ] {
        let report = report_with(outcome.clone(), "0.2.0", vec![]);
        let item = documents_shown(
            Some(&report),
            Some(&outcome),
            None,
            documents_catalog::DocumentsCatalog::embedded(),
        );
        let value = serde_json::to_value(&item).unwrap();
        assert!(!has_no_channel(&value), "{outcome:?} {value}");
        assert_eq!(denial_code(&value, "memory"), Some("not_declared"));
    }

    let mut startup = report_with(MountOutcome::Mounted, "0.2.0", vec![]);
    startup.granted = vec!["events".into()];
    let stopped = documents_shown(
        Some(&startup),
        Some(&MountOutcome::Disabled),
        None,
        documents_catalog::DocumentsCatalog::embedded(),
    );
    let stopped = serde_json::to_value(&stopped).unwrap();
    assert!(!has_no_channel(&stopped), "{stopped}");

    let absent = documents_from(None, None, documents_catalog::DocumentsCatalog::embedded());
    let absent = serde_json::to_value(&absent).unwrap();
    assert_eq!(absent["declared"]["kernel"], json!(["events", "models"]));
    assert_eq!(absent["denied"], json!([]));
}

#[test]
fn hyphae_denials_list_each_item_once() {
    let view = kernel_without_channel(&["events".into(), "events".into()], &[]);
    assert_eq!(view.denied.len(), 1);
    assert_eq!(view.denied[0].item, "events");
    assert_eq!(view.denied[0].code, DenyReason::NoChannel);

    let invalid = kernel_without_channel(&["nope".into(), "nope".into()], &[]);
    assert_eq!(invalid.denied.len(), 1);
    assert_eq!(invalid.denied[0].item, "nope");
    assert_eq!(invalid.denied[0].code, DenyReason::RequiresInvalid);
}

#[tokio::test]
async fn an_attached_module_shows_the_admission_grant() {
    use agent24_attach::attach_registry::AttachGrantBuilder;
    let manifest = agent24_domain::DomainOsManifest::from_yaml(
        "name: agentear\nversion: \"1\"\nroute_namespace: /api/v1/agentear\n\
         event_module: agentear\ndata_dir: ~/.agent24/os/agentear/\n\
         impl_kind: attached_process\nkernel_capabilities: [events, models, memory]\n",
    )
    .unwrap();
    for with_models in [false, true] {
        let builder = admission_builder(with_models).await;
        let held = builder.build("agentear", &manifest).kernel_grants.clone();
        let registry = agent24_attach::attach_registry::AttachRegistry::new(
            agent24_attach::attach_registry::AttachDeps {
                grants: Arc::new(builder),
            },
        );
        registry.on_change(&agent24_attach::attached::Change::Registered {
            name: "agentear",
            rotated: false,
            manifest: &manifest,
            manifest_digest: "sha256:deadbeef",
            token_sha256_hex: &"ab".repeat(32),
            token_id: "tok_agentear",
            disabled: false,
        });
        let snap = registry.snapshot();
        assert_eq!(snap[0].kernel_grants, held);
        assert_eq!(snap[0].declared_kernel, ["events", "models", "memory"]);
        let row = serde_json::to_value(&attach_items_from(snap)[0]).unwrap();
        assert_eq!(row["kernel_grants"], json!(held));
        assert_eq!(
            row["declared"],
            json!({"kernel": ["events", "models", "memory"], "operations": []})
        );
        if with_models {
            assert_eq!(row["kernel_grants"], json!(["events", "models"]));
            assert!(denial_code(&row, "models").is_none(), "{row}");
        } else {
            assert_eq!(row["kernel_grants"], json!(["events"]));
            assert_eq!(denial_code(&row, "models"), Some("no_channel"), "{row}");
        }
        assert_eq!(denial_code(&row, "memory"), Some("above_ceiling"), "{row}");
    }
}

struct NoopTrigger;

#[async_trait::async_trait]
impl agent24_scheduler::RunTrigger for NoopTrigger {
    async fn trigger(
        &self,
        _invocation: &agent24_scheduler::ScheduleInvocation,
    ) -> agent24_scheduler::FireOutcome {
        agent24_scheduler::FireOutcome::Deferred {
            reason: agent24_scheduler::DeferReason::MountPending,
        }
    }
}

async fn admission_builder(with_models: bool) -> crate::attach_grant::DaemonAttachGrantBuilder {
    let store = agent24_store::Store::open_memory().await.unwrap();
    let events = crate::events::EventsHub::default();
    crate::attach_grant::DaemonAttachGrantBuilder {
        scheduler: agent24_scheduler::Scheduler::new(
            store.clone(),
            Arc::new(NoopTrigger),
            Arc::new(|_| {}),
        ),
        models: with_models.then(|| crate::model_callback::ModelCallbackDeps {
            router: Arc::new(agent24_models::router::ModelRouter::with_defaults(vec![])),
            usage: Arc::new(crate::model_callback::MemoryUsageSink::default()),
            cancel_root: CancellationToken::new(),
            admission: crate::model_callback::ModelAdmission::new(
                crate::model_callback::MODEL_MAX_IN_FLIGHT_GLOBAL,
                crate::model_callback::MODEL_MAX_IN_FLIGHT_PER_MODULE,
            ),
            events: events.clone(),
            timings: Arc::new(crate::timing_recorder::MemoryTimingSink::default()),
        }),
        approval_broker: crate::module_approval_broker::ModuleApprovalBroker::new(
            store,
            events.clone(),
        ),
        events,
    }
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
    // Hyphae declares `events` and has no kernel channel. The host sees that
    // denial; the grant list stays empty because nothing was mounted.
    let hyphae = find(&full, "hyphae");
    assert_eq!(hyphae["kernel_grants"], json!([]));
    assert_eq!(hyphae["declared"]["kernel"], json!(["events"]));
    assert_eq!(
        hyphae["denied"],
        json!([{"item": "events", "code": "no_channel"}])
    );

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
        "denied",
        "no_channel",
        "kernel_grants",
        "declared",
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
        ("locked", S::Stopped),
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
        assert_eq!(from_idoris(state, false, None).0, want);
        assert_eq!(
            from_idoris(state, true, None).0,
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
fn attached_entries_named_like_a_builtin_component_are_not_listed() {
    let modules = [
        "open-design",
        "agentear",
        "knowledge",
        "hyphae",
        "documents",
        "idoris",
    ]
    .into_iter()
    .map(|n| agent24_attach::attach_registry::AttachSnap {
        name: n.to_owned(),
        version: String::new(),
        disabled: false,
        drain: Some(DrainState::Running),
        mismatch: None,
        kernel_grants: Vec::new(),
        declared_kernel: Vec::new(),
    })
    .collect();
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
            tools: vec!["mcp_--_echo".to_owned()],
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
    // C6 (#1570) fills Documents' provides from its own D9 catalog.
    assert!(
        find(&body, "documents")["provides"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "documents")
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
    assert!(
        bad["provides"].as_array().unwrap().is_empty(),
        "tools on the record are not provides without a capability id"
    );
}

/// A domain package named `mcp-files` and the MCP server `files` would both
/// otherwise show up as component id `mcp-files`. The later row takes a
/// `mcp-bad-<hash>` id. The package keeps the name it was installed under.
#[tokio::test]
async fn a_package_named_mcp_files_does_not_share_its_id_with_the_mcp_server() {
    let (mut state, host, _) = server_tests::capability_state().await;
    state.os_reports = Arc::new(vec![os_report(
        "mcp-files",
        MountOutcome::Mounted,
        d9("mcp-files", false, "pkg-tasks", &[]),
    )]);
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
    ]));
    let app = build_router(state);
    let (status, body) = call(&app, Method::GET, GET, Some(&host), "").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<_> = body["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "duplicate component ids: {ids:?}");
    let package = find(&body, "mcp-files");
    assert_eq!(package["form"], "resident");
    assert_eq!(package["provides"][0]["id"], "pkg-tasks");
    let rewritten = crate::mcp::fallback_component_id("mcp-files");
    let mcp = find(&body, &rewritten);
    assert_eq!(mcp["form"], "mcp");
    assert_eq!(mcp["reason"], "duplicate");
    assert_eq!(mcp["detail"]["server"], "files");
    assert_eq!(mcp["provides"][0]["id"], "mcp-files");
    assert_eq!(mcp["provides"][0]["tools"], json!(["mcp_files_echo"]));
    assert_eq!(
        ids.iter().filter(|id| id.as_str() == "mcp-files").count(),
        1
    );
}

/// Component ids are unique across every source at once: the five builtins, an
/// attached module, domain packages and MCP servers. Each collision below is a
/// different pair: attach/os (`agentear`), os/mcp (`mcp-files`), an os row that
/// sits on the fallback id the MCP row would take, and a package named like a
/// builtin (which is not listed at all).
#[tokio::test]
async fn component_ids_are_unique_across_builtin_attach_os_and_mcp() {
    let mut state = ear().await.state;
    let taken = crate::mcp::fallback_component_id("mcp-files");
    state.os_reports = Arc::new(vec![
        os_report(
            "agentear",
            MountOutcome::Mounted,
            d9("agentear", false, "pkg-ear", &[]),
        ),
        os_report(
            "mcp-files",
            MountOutcome::Mounted,
            d9("mcp-files", false, "pkg-files", &[]),
        ),
        os_report(
            &taken,
            MountOutcome::Mounted,
            d9(&taken, false, "pkg-taken", &[]),
        ),
        os_report(
            "idoris",
            MountOutcome::Mounted,
            d9("idoris", false, "pkg-idoris", &[]),
        ),
        os_report(
            "knowledge",
            MountOutcome::Mounted,
            d9("knowledge", false, "pkg-know", &[]),
        ),
    ]);
    state.mcp_roster = Arc::new(crate::mcp::McpRoster::from_records(vec![
        crate::mcp::McpRecord {
            server: "files".to_owned(),
            id: "mcp-files".to_owned(),
            state: ComponentState::Ready,
            reason: None,
            capability_id: Some("mcp-files".to_owned()),
            tools: Vec::new(),
            protocol_version: None,
        },
    ]));
    let app = build_router(state);
    let (status, body) = call(&app, Method::GET, GET, Some("testtoken"), "").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<String> = body["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len(), "duplicate component ids: {ids:?}");
    // The sources are all present, so the uniqueness above is not vacuous.
    for id in [
        "idoris",
        "hyphae",
        "documents",
        "knowledge",
        "open-design",
        "agentear",
        "mcp-files",
        taken.as_str(),
    ] {
        assert!(ids.iter().any(|i| i == id), "{id} missing from {ids:?}");
    }
    // 5 builtins + attach + 3 os rows (the two builtin-named packages are
    // dropped) + 1 mcp row = 10. Nothing was swallowed by the dedupe.
    assert_eq!(ids.len(), 10, "{ids:?}");
    assert_eq!(find(&body, "agentear")["form"], "attach");
}

/// A refused package is `failed` with the reason of its refusal and lists no
/// provider; a plain refusal keeps `refused`.
#[tokio::test]
async fn a_refused_package_is_failed_with_its_own_reason() {
    use crate::domain::RefusalReason::{DuplicateCapabilityId, ReservedCapabilityId};
    let (mut state, host, _) = server_tests::capability_state().await;
    let bad = d9("squatter", false, "model-gateway", &[]);
    state.os_reports = Arc::new(vec![
        os_report(
            "squatter",
            MountOutcome::RefusedFor(ReservedCapabilityId, "x".into()),
            bad.without_provides(),
        ),
        os_report(
            "second",
            MountOutcome::RefusedFor(DuplicateCapabilityId, "y".into()),
            d9("second", false, "taken", &[]).without_provides(),
        ),
        os_report(
            "other",
            MountOutcome::Refused("duplicate".into()),
            d9("other", false, "other-tasks", &[]),
        ),
    ]);
    let app = build_router(state);
    let (_, body) = call(&app, Method::GET, GET, Some(&host), "").await;
    let squatter = find(&body, "squatter");
    assert_eq!(squatter["state"], "failed");
    assert_eq!(squatter["reason"], "reserved_capability_id");
    assert_eq!(squatter["provides"], json!([]));
    assert_eq!(find(&body, "second")["reason"], "duplicate_capability_id");
    assert_eq!(find(&body, "other")["reason"], "refused");
}

/// A package named like a builtin is refused, but it stays on `/components`
/// under a fallback id, so it is not invisible, and it does not take over the
/// builtin's own row.
#[tokio::test]
async fn a_package_squatting_a_builtin_name_is_listed_under_a_fallback_id() {
    use crate::domain::RefusalReason::ReservedComponentName;
    let (mut state, host, _) = server_tests::capability_state().await;
    let reports: Vec<_> = ["hyphae", "documents"]
        .into_iter()
        .map(|name| {
            os_report(
                name,
                MountOutcome::RefusedFor(ReservedComponentName, "builtin name".into()),
                d9(name, false, "nostr-dm", &[]).without_provides(),
            )
        })
        .collect();
    state.os_reports = Arc::new(reports);
    let app = build_router(state);
    let (_, body) = call(&app, Method::GET, GET, Some(&host), "").await;
    let rows = body["components"].as_array().unwrap();
    let ids: Vec<_> = rows.iter().map(|c| c["id"].as_str().unwrap()).collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len(), "{ids:?}");
    let squatters: Vec<_> = rows
        .iter()
        .filter(|c| c["reason"] == "reserved_component_name")
        .collect();
    assert_eq!(squatters.len(), 2, "{ids:?}");
    for row in &squatters {
        assert_eq!(row["state"], "failed");
        assert_eq!(row["provides"], json!([]));
        assert_ne!(row["id"], row["detail"]["package"], "{row}");
        assert!(row["id"].as_str().unwrap().starts_with("mcp-bad-"), "{row}");
    }
    // The builtins keep their own rows and are not described by the squatters.
    assert_ne!(find(&body, "hyphae")["reason"], "reserved_component_name");
    assert_ne!(
        find(&body, "documents")["reason"],
        "reserved_component_name"
    );
}

/// F2 (#1578): two directories named `hyphae`. The first is refused for its
/// name, the second runs into the duplicate-name check and gets a plain
/// `Refused`. Both stay on `/components` under unique ids. A second package
/// named `documents` (plain `Refused`) is listed too, without displacing the
/// real Documents row.
#[tokio::test]
async fn every_refused_package_has_a_row_even_with_a_builtin_name() {
    use crate::domain::RefusalReason::ReservedComponentName;
    let (mut state, host, _) = server_tests::capability_state().await;
    state.os_reports = Arc::new(vec![
        os_report(
            "hyphae",
            MountOutcome::RefusedFor(ReservedComponentName, "builtin name".into()),
            d9("hyphae", false, "nostr-dm", &[]).without_provides(),
        ),
        os_report(
            "hyphae",
            MountOutcome::Refused("another module already claims the name \"hyphae\"".into()),
            d9("hyphae", false, "nostr-dm", &[]),
        ),
        os_report(
            "documents",
            MountOutcome::Mounted,
            d9("documents", false, "documents", &[]),
        ),
        os_report(
            "documents",
            MountOutcome::Refused("another module already claims the name \"documents\"".into()),
            d9("documents", false, "documents", &[]),
        ),
    ]);
    let app = build_router(state);
    let (_, body) = call(&app, Method::GET, GET, Some(&host), "").await;
    let rows = body["components"].as_array().unwrap();
    let ids: Vec<_> = rows.iter().map(|c| c["id"].as_str().unwrap()).collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len(), "{ids:?}");
    let package_rows = |name: &str| -> Vec<&Value> {
        rows.iter()
            .filter(|c| c["detail"]["package"] == name)
            .collect()
    };
    let hyphae = package_rows("hyphae");
    assert_eq!(hyphae.len(), 2, "{ids:?}");
    let mut reasons: Vec<_> = hyphae
        .iter()
        .map(|r| r["reason"].as_str().unwrap())
        .collect();
    reasons.sort_unstable();
    assert_eq!(reasons, ["refused", "reserved_component_name"]);
    for row in &hyphae {
        assert_eq!(row["state"], "failed");
        assert_ne!(row["id"], "hyphae");
    }
    let documents = package_rows("documents");
    assert_eq!(documents.len(), 1, "{ids:?}");
    assert_eq!(documents[0]["reason"], "refused");
    // The real Documents row is still the mounted one.
    assert_eq!(find(&body, "documents")["form"], "resident");
    assert_ne!(find(&body, "documents")["state"], "failed");
}

/// The Documents package refused for its own reasons is the Documents row, not
/// a second extra row beside it.
#[tokio::test]
async fn a_refused_documents_package_is_not_listed_twice() {
    use crate::domain::RefusalReason::ReservedCapabilityId;
    let (mut state, host, _) = server_tests::capability_state().await;
    state.os_reports = Arc::new(vec![os_report(
        "documents",
        MountOutcome::RefusedFor(ReservedCapabilityId, "x".into()),
        d9("documents", false, "nostr-dm", &[]).without_provides(),
    )]);
    let app = build_router(state);
    let (_, body) = call(&app, Method::GET, GET, Some(&host), "").await;
    let rows = body["components"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .filter(|c| c["detail"]["package"] == "documents")
            .count(),
        0
    );
    assert_eq!(find(&body, "documents")["reason"], "reserved_capability_id");
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
        "denied",
        "no_channel",
        "kernel_grants",
        "declared",
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

/// `requires` names kernel capabilities and canonical operations, never a
/// REGISTRY capability id (C9-A). A kernel name that is also a registry id,
/// or an operation that is only a slash alias, turns this red.
fn assert_requires_tokens(
    requires: &Value,
    registered: &std::collections::BTreeSet<String>,
    where_: &str,
) {
    let kernel = requires["kernel"]
        .as_array()
        .unwrap_or_else(|| panic!("{where_} requires.kernel is missing"));
    for name in kernel {
        let name = name.as_str().unwrap();
        assert!(
            agent24_domain::Capability::parse(name).is_ok(),
            "{where_} requires.kernel {name} is not a kernel capability"
        );
        assert!(
            !registered.contains(name),
            "{where_} requires.kernel {name} is a REGISTRY capability id"
        );
    }
    let operations = requires["operations"]
        .as_array()
        .unwrap_or_else(|| panic!("{where_} requires.operations is missing"));
    for op in operations {
        let op = op.as_str().unwrap();
        assert!(
            crate::capabilities::Operation::canonical(op).is_some(),
            "{where_} requires.operations {op} is not a canonical Operation name"
        );
    }
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
    let adr040 = std::fs::read_to_string(root.join("docs/adr/ADR-040-skill-contract-v1.md"))
        .unwrap_or_else(|e| panic!("reading ADR-040: {e}"));
    assert!(
        !adr040.contains("尚不存在"),
        "ADR-040 still says REGISTRY.md does not exist"
    );
    assert!(
        adr040.contains("docs/capabilities/REGISTRY.md") && adr040.contains("已由 ADR-038 C7 建立"),
        "ADR-040 should say the C7 registry exists"
    );
    let registered = registered_ids(&markdown);
    for cap in agent24_domain::ALL_CAPABILITIES {
        assert!(
            !registered.contains(cap.as_str()),
            "kernel capability {} collides with a REGISTRY capability id",
            cap.as_str()
        );
    }
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
        if let Some(requires) = value.get("requires") {
            assert_requires_tokens(requires, &registered, &path.display().to_string());
        }
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
    assert!(
        manifests
            .iter()
            .any(|p| p.ends_with("agent24-documents/domain-os.yml")),
        "documents domain-os.yml must be part of the requires drift check"
    );
    for path in manifests {
        let text = std::fs::read_to_string(&path).unwrap();
        let has_provides = text
            .lines()
            .any(|line| line.trim().starts_with("provides:"));
        let manifest = agent24_domain::DomainOsManifest::from_yaml(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if has_provides {
            for provide in manifest.provides() {
                assert!(
                    registered.contains(provide.id()),
                    "{} provides {}, which REGISTRY.md does not list",
                    path.display(),
                    provide.id()
                );
            }
        }
        for cap in manifest.kernel_capabilities() {
            assert!(
                agent24_domain::Capability::parse(cap.as_str()).is_ok(),
                "{} kernel capability {} is not a kernel capability",
                path.display(),
                cap.as_str()
            );
            assert!(
                !registered.contains(cap.as_str()),
                "{} kernel capability {} is a REGISTRY capability id",
                path.display(),
                cap.as_str()
            );
        }
    }

    let mut catalogs = Vec::new();
    fn walk_catalogs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
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
                walk_catalogs(&path, out);
            } else if name == "component.json" {
                out.push(path);
            }
        }
    }
    walk_catalogs(&root.join("rust"), &mut catalogs);
    assert!(
        catalogs
            .iter()
            .any(|p| p.ends_with("agent24-documents/component.json")),
        "documents component.json must be part of the requires drift check"
    );
    for path in catalogs {
        let text = std::fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let Some(requires) = value.get("requires") else {
            continue;
        };
        assert_requires_tokens(requires, &registered, &path.display().to_string());
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
        // These books model a run that has already passed once.
        confirmed: true,
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
        health_armed: false,
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

fn armed_probe_of(health: Option<agent24_comm::HealthView>) -> agent24_comm::ProbeSnapshot {
    agent24_comm::ProbeSnapshot {
        health_armed: true,
        ..probe_of(agent24_comm::ContractView::Compatible, health)
    }
}

/// ADR-038 D5: `starting -> ready` is the first passing health probe.
#[test]
fn armed_probe_holds_starting_until_the_first_passing_sample() {
    use ComponentState as S;

    // Running, armed, no sample yet.
    let none = hyphae_with("running", None, armed_probe_of(None));
    assert_eq!(none.state, S::Starting);
    assert!(none.reason.is_none());

    // First sample passed.
    let mut passed = health_book(false, 0, true);
    passed.confirmed = true;
    let ready = hyphae_with("running", None, armed_probe_of(Some(passed)));
    assert_eq!(ready.state, S::Ready);

    // A failure after a pass is the ordinary N-strike rule, still ready.
    let after = hyphae_with(
        "running",
        None,
        armed_probe_of(Some(health_book(false, 1, false))),
    );
    assert_eq!(after.state, S::Ready);
}

#[test]
fn failed_first_samples_stay_starting_then_degrade_on_the_nth() {
    use ComponentState as S;

    let mut one = health_book(false, 1, false);
    one.confirmed = false;
    let item = hyphae_with("running", None, armed_probe_of(Some(one)));
    assert_eq!(item.state, S::Starting, "one failed first sample");
    let body = render(&[item], true);
    assert_eq!(
        body["components"][0]["detail"]["health"]["consecutive_failures"], 1,
        "the host view still shows the strike"
    );

    let mut n = health_book(true, 3, false);
    n.confirmed = false;
    let item = hyphae_with("running", None, armed_probe_of(Some(n)));
    assert_eq!(item.state, S::Degraded);
    assert_eq!(item.reason.as_deref(), Some("relays"));
}

#[test]
fn unarmed_probe_keeps_running_as_ready() {
    use ComponentState as S;
    use agent24_comm::ContractView;

    let item = hyphae_with("running", None, probe_of(ContractView::Compatible, None));
    assert_eq!(item.state, S::Ready, "fixtures never arm a probe");
    // Armed but the process is not running: the process mapping wins.
    let stopped = hyphae_with("stopped", None, armed_probe_of(None));
    assert_eq!(stopped.state, S::Stopped);
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
    assert_eq!(item.reason.as_deref(), Some("component_contract_mismatch"));
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

// ---- Knowledge capabilities probe (ADR-038 C5) -----------------------------------------

use crate::capabilities::unix_now;
use agent24_knowledge::host::{Change, Credential, Endpoint, KnowledgeHostRegistry};
use agent24_knowledge::probe::{KNOWLEDGE_PROBE_TIMEOUT, KNOWLEDGE_PROBE_TTL};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

fn shown(item: &Item) -> Value {
    render(std::slice::from_ref(item), true)["components"][0].clone()
}

fn assert_no_canary(item: &Item, canaries: &[&str]) {
    let host = render(std::slice::from_ref(item), true).to_string();
    let redacted = render(std::slice::from_ref(item), false).to_string();
    for canary in canaries {
        assert!(!host.contains(canary), "{canary} in host view {host}");
        assert!(
            !redacted.contains(canary),
            "{canary} in redacted view {redacted}"
        );
    }
    for leaked in [
        "detail",
        "reason",
        "probed_at",
        "contract_version",
        "missing_capabilities",
        "missing_capability",
        "basis",
    ] {
        assert!(
            !redacted.contains(leaked),
            "{leaked} in redacted view {redacted}"
        );
    }
}

fn configured(base: &str) -> KnowledgeHostRegistry {
    let registry = KnowledgeHostRegistry::default();
    registry
        .activate(
            "host",
            "binding",
            1,
            Endpoint {
                base_url: base.to_owned(),
                knowledge_base_ids: vec!["kb".into()],
            },
        )
        .unwrap();
    registry
}

/// `limits` from `protocol/knowledge/fixtures/v1/capabilities.integration.json`
/// (the object `capabilities.schema.json` requires next to `server_version`).
const SCHEMA_LIMITS: &str = r#""limits":{"max_results":50,"max_concurrency":4,"max_deadline_ms":30000,"max_read_bytes":262144,"max_page_limit":100,"max_snippet_bytes":2048}"#;

fn ready_body() -> String {
    format!(
        r#"{{"code":0,"msg":"LEAK-msg-9f3a","data":{{"edition":"lite","x_unknown":1,"integration":{{"other":{{}},"agent24_knowledge":{{"protocol_versions":[1,2],"mode":"integration","supported_operations":["search","read","ingest"],"server_version":"0.8.2",{limits},"future_field":true}}}}}}}}"#,
        limits = SCHEMA_LIMITS,
    )
}

#[derive(Clone)]
enum Script {
    Reply(u16, String),
    /// 500 if the request carries a credential header, else `body`.
    NoAuth(String),
    Stall,
    /// 500 until the flag is set, then `body`.
    Switch(Arc<AtomicBool>, String),
    Hold(Arc<AtomicBool>, String),
    Bearer {
        secret: String,
        ok: String,
        anon: String,
    },
    /// Hold the response until `go`, then answer from the bearer token.
    HoldAuth {
        go: Arc<AtomicBool>,
        pairs: Vec<(String, String)>,
        fallback: String,
    },
}

/// What the fake answers on `POST .../agent24/knowledge/ready` (§8).
#[derive(Clone)]
enum ReadyMode {
    /// `live` and both operations `ready`, echoing the request id.
    Ready,
    /// `live`, but `operations.search` is `not_ready`.
    SearchNotReady,
    /// `live` is false.
    NotLive,
    /// `ready` for search and read, with another request id.
    WrongEcho,
    /// A non-2xx answer.
    Status(u16),
    Garbage,
    Stall,
}

/// The ready endpoint's counters. Its requests do not count in `Fake::hits`.
#[derive(Clone)]
struct ReadyCtl {
    mode: Arc<std::sync::Mutex<ReadyMode>>,
    hits: Arc<AtomicUsize>,
    /// Header block and body of every ready request.
    seen: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

struct Fake {
    base: String,
    hits: Arc<AtomicUsize>,
    ready: ReadyCtl,
}

impl Fake {
    fn ready_mode(&self, mode: ReadyMode) {
        *self.ready.mode.lock().unwrap() = mode;
    }
    fn ready_hits(&self) -> usize {
        self.ready.hits.load(Ordering::SeqCst)
    }
}

async fn serve(script: Script) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let ready = ReadyCtl {
        mode: Arc::new(std::sync::Mutex::new(ReadyMode::Ready)),
        hits: Arc::new(AtomicUsize::new(0)),
        seen: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let ctl = ready.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let script = script.clone();
            let counter = Arc::clone(&counter);
            let ctl = ctl.clone();
            tokio::spawn(async move {
                let _ = answer(stream, script, &counter, &ctl).await;
            });
        }
    });
    Fake {
        base: format!("http://{addr}/api/v1"),
        hits,
        ready,
    }
}

async fn answer(
    mut stream: TcpStream,
    script: Script,
    hits: &AtomicUsize,
    ready: &ReadyCtl,
) -> std::io::Result<()> {
    let (head, body) = match read_request(&mut stream).await {
        Some(request) => request,
        None => return Ok(()),
    };
    if head
        .to_ascii_lowercase()
        .contains("agent24/knowledge/ready")
    {
        ready.hits.fetch_add(1, Ordering::SeqCst);
        ready.seen.lock().unwrap().push((head, body.clone()));
        return answer_ready(&mut stream, ready, &body).await;
    }
    hits.fetch_add(1, Ordering::SeqCst);
    if !head.to_ascii_lowercase().contains("system/capabilities") {
        write_response(&mut stream, 404, "not-found").await?;
        return Ok(());
    }
    match script {
        Script::Reply(status, body) => write_response(&mut stream, status, &body).await?,
        Script::NoAuth(body) => {
            let lower = head.to_ascii_lowercase();
            if lower.contains("authorization:") || lower.contains("x-api-key:") {
                write_response(&mut stream, 500, "saw-auth-LEAK").await?;
            } else {
                write_response(&mut stream, 200, &body).await?;
            }
        }
        Script::Stall => std::future::pending::<()>().await,
        Script::Switch(up, body) => {
            if up.load(Ordering::SeqCst) {
                write_response(&mut stream, 200, &body).await?;
            } else {
                write_response(&mut stream, 500, "down").await?;
            }
        }
        Script::Hold(go, body) => {
            while !go.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            write_response(&mut stream, 200, &body).await?;
        }
        Script::Bearer { secret, ok, anon } => {
            let needle = format!("authorization: bearer {}", secret.to_ascii_lowercase());
            let body = if head.to_ascii_lowercase().contains(&needle) {
                ok
            } else {
                anon
            };
            write_response(&mut stream, 200, &body).await?;
        }
        Script::HoldAuth {
            go,
            pairs,
            fallback,
        } => {
            let body = bearer_body(&head, &pairs, &fallback);
            while !go.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            write_response(&mut stream, 200, &body).await?;
        }
    }
    Ok(())
}

fn bearer_body(head: &str, pairs: &[(String, String)], fallback: &str) -> String {
    let lower = head.to_ascii_lowercase();
    for (secret, body) in pairs {
        let needle = format!("authorization: bearer {}", secret.to_ascii_lowercase());
        if lower.contains(&needle) {
            return body.clone();
        }
    }
    fallback.to_owned()
}

/// Header block and body. The body is read up to `content-length`.
async fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 1024];
    let split = loop {
        if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if data.len() > 16 * 1024 {
            return None;
        }
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8(data[..split].to_vec()).ok()?;
    let wanted = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while data.len() < split + wanted {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    let body = String::from_utf8(data[split..].to_vec()).ok()?;
    Some((head, body))
}

async fn answer_ready(
    stream: &mut TcpStream,
    ready: &ReadyCtl,
    request: &str,
) -> std::io::Result<()> {
    let mode = ready.mode.lock().unwrap().clone();
    let id = serde_json::from_str::<Value>(request)
        .ok()
        .and_then(|v| v["request_id"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let body = |id: &str, live: bool, search: &str| {
        json!({
            "protocol_version": 1, "request_id": id, "generation": 1, "live": live,
            "operations": {
                "search": {"state": search}, "read": {"state": "ready"},
                "ingest": {"state": "disabled", "reason": "read_only_v1"},
            },
        })
        .to_string()
    };
    match mode {
        ReadyMode::Ready => write_response(stream, 200, &body(&id, true, "ready")).await,
        ReadyMode::SearchNotReady => {
            write_response(stream, 200, &body(&id, true, "not_ready")).await
        }
        ReadyMode::NotLive => write_response(stream, 200, &body(&id, false, "ready")).await,
        ReadyMode::WrongEcho => {
            write_response(stream, 200, &body("another-request", true, "ready")).await
        }
        ReadyMode::Status(status) => write_response(stream, status, "LEAK-ready-body").await,
        ReadyMode::Garbage => write_response(stream, 200, "not json LEAK-ready-body").await,
        ReadyMode::Stall => std::future::pending().await,
    }
}

async fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let bytes = format!(
        "HTTP/1.1 {status} S\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(bytes.as_bytes()).await
}

#[tokio::test]
async fn knowledge_without_an_endpoint_is_stopped_and_does_not_probe() {
    let fake = serve(Script::Reply(200, ready_body())).await;
    let registry = KnowledgeHostRegistry::default();
    let item = knowledge_item(&registry, Instant::now()).await;
    assert_eq!(item.state, ComponentState::Stopped);
    assert_eq!(item.reason.as_deref(), Some("not_configured"));
    assert!(item.detail.is_none());

    // Active, but the activation named no endpoint: still not configured.
    registry
        .apply("host", "binding", 1, Change::Activate)
        .unwrap();
    let item = knowledge_item(&registry, Instant::now()).await;
    assert_eq!(item.state, ComponentState::Stopped);
    assert_eq!(item.reason.as_deref(), Some("not_configured"));
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_complete_capabilities_document_is_ready() {
    let fake = serve(Script::NoAuth(ready_body())).await;
    let registry = configured(&fake.base);
    let item = knowledge_item(&registry, Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "ready");
    assert_eq!(host["version"], "0.8.2");
    assert!(host.get("reason").is_none());
    assert_eq!(host["detail"]["basis"], "capabilities");
    assert_eq!(host["detail"]["missing_capabilities"], json!([]));
    assert!(host["detail"]["probed_at"].as_u64().unwrap() > 0);
    assert_eq!(host["detail"]["contract_version"]["expected"], "1");
    assert_eq!(host["detail"]["contract_version"]["actual"], json!([1, 2]));
    assert_eq!(host["detail"]["contract_version"]["mode"], "integration");
    assert_no_canary(&item, &["LEAK-msg-9f3a", "future_field", "saw-auth"]);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_missing_required_operation_is_degraded() {
    let body = format!(
        r#"{{"code":0,"msg":"LEAK-degraded-msg","data":{{"integration":{{"agent24_knowledge":{{"protocol_versions":[1],"mode":"integration","supported_operations":["search","ingest"],"server_version":"1.2.3",{limits}}}}}}}}}"#,
        limits = SCHEMA_LIMITS,
    );
    let fake = serve(Script::Reply(200, body.to_owned())).await;
    let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "degraded");
    assert_eq!(host["reason"], "missing_capability:read");
    assert_eq!(host["detail"]["missing_capabilities"], json!(["read"]));
    assert_eq!(host["version"], "1.2.3");
    assert_no_canary(&item, &["LEAK-degraded-msg", "ingest"]);
    let redacted = render(std::slice::from_ref(&item), false);
    assert_eq!(redacted["components"][0]["state"], "degraded");
}

#[tokio::test]
async fn a_closed_port_is_failed_without_the_os_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let item = knowledge_item(
        &configured(&format!("http://{addr}/api/v1")),
        Instant::now(),
    )
    .await;
    let host = shown(&item);
    assert_eq!(host["state"], "failed");
    assert_eq!(host["reason"], "unreachable");
    assert!(host["detail"]["contract_version"].is_null());
    assert_eq!(host["detail"]["missing_capabilities"], json!([]));
    let text = host.to_string();
    for leaked in ["refused", "Connection", "127.0.0.1", "timed out", "Os"] {
        assert!(!text.contains(leaked), "{leaked} in {text}");
    }
}

#[tokio::test]
async fn an_http_error_body_is_not_copied() {
    let body = "database password is LEAK-http-body at /Users/secret";
    let fake = serve(Script::Reply(500, body.to_owned())).await;
    let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "failed");
    assert_eq!(host["reason"], "http_500");
    assert_no_canary(
        &item,
        &["LEAK-http-body", "/Users/secret", "password", "database"],
    );
}

#[tokio::test]
async fn an_upstream_error_code_is_kept_and_its_message_is_not() {
    let body = r#"{"code":401,"msg":"LEAK-upstream-token","data":null}"#;
    let fake = serve(Script::Reply(200, body.to_owned())).await;
    let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "failed");
    assert_eq!(host["reason"], "upstream_401");
    assert_no_canary(&item, &["LEAK-upstream-token"]);
}

#[tokio::test]
async fn a_different_contract_major_is_incompatible() {
    let body = format!(
        r#"{{"data":{{"integration":{{"agent24_knowledge":{{"protocol_versions":[2,3],"mode":"integration","supported_operations":["search","read"],"server_version":"9.9.9",{limits}}}}}}}}}"#,
        limits = SCHEMA_LIMITS,
    );
    let fake = serve(Script::Reply(200, body.to_owned())).await;
    let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "incompatible");
    assert_eq!(host["reason"], "component_contract_mismatch");
    assert_eq!(host["detail"]["contract_version"]["expected"], "1");
    assert_eq!(host["detail"]["contract_version"]["actual"], json!([2, 3]));
    assert_eq!(host["detail"]["missing_capabilities"], json!([]));
    assert_eq!(host["version"], "9.9.9");
    let redacted = render(std::slice::from_ref(&item), false);
    assert_eq!(redacted["components"][0]["state"], "incompatible");
    assert_no_canary(&item, &[]);
    assert!(
        !redacted.to_string().contains("component_contract_mismatch"),
        "{redacted}"
    );
}

#[tokio::test]
async fn standalone_mode_is_incompatible_and_its_version_text_is_not_copied() {
    let body = format!(
        r#"{{"code":0,"msg":"LEAK-mode-msg","data":{{"integration":{{"agent24_knowledge":{{"protocol_versions":[1],"mode":"standalone","supported_operations":["search","read"],"server_version":"1.0 LEAK-ver-77",{limits}}}}}}}}}"#,
        limits = SCHEMA_LIMITS,
    );
    let fake = serve(Script::Reply(200, body.to_owned())).await;
    let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "incompatible");
    assert_eq!(host["reason"], "component_contract_mismatch");
    assert_eq!(host["detail"]["contract_version"]["mode"], "standalone");
    assert!(host.get("version").is_none() || host["version"].is_null());
    assert_no_canary(&item, &["LEAK-mode-msg", "LEAK-ver-77"]);
}

#[tokio::test]
async fn a_probe_is_reused_until_the_ttl_and_a_deactivation_is_not() {
    let fake = serve(Script::Reply(200, ready_body())).await;
    let registry = configured(&fake.base);
    let t0 = Instant::now();
    let first = knowledge_item(&registry, t0).await;
    assert_eq!(first.state, ComponentState::Ready);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);
    let second = knowledge_item(
        &registry,
        t0 + KNOWLEDGE_PROBE_TTL - Duration::from_millis(1),
    )
    .await;
    assert_eq!(second.state, ComponentState::Ready);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        shown(&first)["detail"]["probed_at"],
        shown(&second)["detail"]["probed_at"]
    );
    let third = knowledge_item(&registry, t0 + KNOWLEDGE_PROBE_TTL).await;
    assert_eq!(third.state, ComponentState::Ready);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 2);

    registry
        .apply("host", "binding", 2, Change::Deactivate)
        .unwrap();
    let stopped = knowledge_item(&registry, t0 + KNOWLEDGE_PROBE_TTL).await;
    assert_eq!(stopped.state, ComponentState::Stopped);
    assert_eq!(stopped.reason.as_deref(), Some("not_configured"));
    assert_eq!(fake.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_component_reads_probe_once() {
    let go = Arc::new(AtomicBool::new(false));
    let fake = serve(Script::Hold(Arc::clone(&go), ready_body())).await;
    let registry = Arc::new(configured(&fake.base));
    let t0 = Instant::now();
    let started = Arc::new(AtomicUsize::new(0));
    let mut joins = Vec::new();
    for _ in 0..8 {
        let registry = Arc::clone(&registry);
        let started = Arc::clone(&started);
        joins.push(tokio::spawn(async move {
            started.fetch_add(1, Ordering::SeqCst);
            knowledge_item(&registry, t0).await
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while started.load(Ordering::SeqCst) < 8 {
        if Instant::now() > deadline {
            panic!("tasks did not start");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        fake.hits.load(Ordering::SeqCst),
        1,
        "concurrent reads shared one probe"
    );
    go.store(true, Ordering::SeqCst);
    for join in joins {
        let item = join.await.unwrap();
        assert_eq!(item.state, ComponentState::Ready);
    }
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stall_fails_as_timeout_without_the_client_error_text() {
    let fake = serve(Script::Stall).await;
    let started = Instant::now();
    let item = tokio::time::timeout(
        KNOWLEDGE_PROBE_TIMEOUT + Duration::from_secs(2),
        knowledge_item(&configured(&fake.base), Instant::now()),
    )
    .await
    .unwrap();
    assert!(started.elapsed() + Duration::from_millis(250) >= KNOWLEDGE_PROBE_TIMEOUT);
    let host = shown(&item);
    assert_eq!(host["state"], "failed");
    assert_eq!(host["reason"], "timeout");
    let text = host.to_string();
    for leaked in ["deadline", "exceeded", "transport:", fake.base.as_str()] {
        assert!(!text.contains(leaked), "{leaked} in {text}");
    }
}

#[tokio::test]
async fn rotating_the_credential_probes_again_and_the_secret_is_not_shown() {
    let secret = "probe-secret-value";
    let anon = format!(
        r#"{{"data":{{"integration":{{"agent24_knowledge":{{"protocol_versions":[1],"mode":"integration","supported_operations":["search"],"server_version":"0.1.0",{limits}}}}}}}}}"#,
        limits = SCHEMA_LIMITS,
    );
    let ok = ready_body();
    let fake = serve(Script::Bearer {
        secret: secret.to_owned(),
        ok,
        anon: anon.to_owned(),
    })
    .await;
    let registry = configured(&fake.base);
    let t0 = Instant::now();
    let first = knowledge_item(&registry, t0).await;
    assert_eq!(shown(&first)["state"], "degraded");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);
    let cached = knowledge_item(&registry, t0 + Duration::from_millis(1)).await;
    assert_eq!(shown(&cached)["reason"], "missing_capability:read");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);

    registry
        .push_credential(
            "host",
            "binding",
            2,
            Credential::Bearer(secret.to_owned()),
            unix_now() + 600,
        )
        .unwrap();
    let authed = knowledge_item(&registry, t0 + Duration::from_millis(2)).await;
    let host = shown(&authed);
    assert_eq!(host["state"], "ready");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 2);
    assert_no_canary(&authed, &[secret]);
}

/// Each `parse_data` rejection (missing declaration or a malformed field).
/// A guard that instead accepts the document turns that row `ready`.
#[tokio::test]
async fn an_unusable_capabilities_declaration_is_not_ready() {
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "missing_agent",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{}}}"#,
            "incompatible",
            "component_contract_mismatch",
        ),
        (
            "agent_not_object",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":[]}}}"#,
            "failed",
            "malformed",
        ),
        (
            "empty_versions",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0"}}}}"#,
            "incompatible",
            "component_contract_mismatch",
        ),
        (
            "zero_version",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[0],"mode":"integration","supported_operations":["search","read"]}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "versions_not_array",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":"1","mode":"integration","supported_operations":["search","read"]}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "versions_absent",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0"}}}}"#,
            "incompatible",
            "component_contract_mismatch",
        ),
        (
            "mode_not_string",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":1,"supported_operations":["search","read"]}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "mode_absent",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"supported_operations":["search","read"],"server_version":"1.0.0"}}}}"#,
            "incompatible",
            "component_contract_mismatch",
        ),
        (
            "op_not_string",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":[1]}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "ops_not_array",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":"search"}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "ops_absent",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","server_version":"1.0.0"}}}}"#,
            "incompatible",
            "component_contract_mismatch",
        ),
        (
            "version_not_string",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":1}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "server_version_absent",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"limits":{"max_results":50,"max_concurrency":4,"max_deadline_ms":30000,"max_read_bytes":262144,"max_page_limit":100,"max_snippet_bytes":2048}}}}}"#,
            "incompatible",
            "component_contract_mismatch",
        ),
        (
            "limits_absent",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0"}}}}"#,
            "incompatible",
            "component_contract_mismatch",
        ),
        (
            "limits_not_object",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0","limits":1}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "limits_empty",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0","limits":{}}}}}"#,
            "incompatible",
            "component_contract_mismatch",
        ),
        (
            "limits_zero",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0","limits":{"max_results":0,"max_concurrency":4,"max_deadline_ms":30000,"max_read_bytes":262144,"max_page_limit":100,"max_snippet_bytes":2048}}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "limits_fraction",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0","limits":{"max_results":1.5,"max_concurrency":4,"max_deadline_ms":30000,"max_read_bytes":262144,"max_page_limit":100,"max_snippet_bytes":2048}}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "limits_negative",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0","limits":{"max_results":-1,"max_concurrency":4,"max_deadline_ms":30000,"max_read_bytes":262144,"max_page_limit":100,"max_snippet_bytes":2048}}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "limits_string",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0","limits":{"max_results":"50","max_concurrency":4,"max_deadline_ms":30000,"max_read_bytes":262144,"max_page_limit":100,"max_snippet_bytes":2048}}}}}"#,
            "failed",
            "malformed",
        ),
        (
            "limits_missing_key",
            r#"{"code":0,"msg":"LEAK-parse","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0","limits":{"max_results":50,"max_concurrency":4,"max_deadline_ms":30000,"max_read_bytes":262144,"max_page_limit":100}}}}}"#,
            "incompatible",
            "component_contract_mismatch",
        ),
    ];
    for (label, body, state, reason) in cases {
        let fake = serve(Script::Reply(200, (*body).to_owned())).await;
        let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
        let host = shown(&item);
        assert_eq!(host["state"], *state, "{label}");
        assert_eq!(host["reason"], *reason, "{label}");
        assert!(
            !host.to_string().contains("LEAK-parse"),
            "{label} copied the message"
        );
        let redacted = render(std::slice::from_ref(&item), false).to_string();
        assert!(!redacted.contains("LEAK-parse"), "{label}");
        assert!(!redacted.contains(*reason), "{label} reason in redacted");
    }
}

/// HTTP 200 with a non-success envelope code. `data` is an object, so parsing
/// it would miss the upstream failure.
#[tokio::test]
async fn an_upstream_code_with_a_data_object_is_not_a_declaration() {
    let body = r#"{"code":401,"msg":"LEAK-data-object","data":{}}"#;
    let fake = serve(Script::Reply(200, body.to_owned())).await;
    let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "failed");
    assert_eq!(host["reason"], "upstream_401");
    assert_no_canary(&item, &["LEAK-data-object"]);
}

/// The task that became the probe leader is aborted while the remote is held.
/// The waiter must probe again and observe the remote, not a synthetic failure.
#[tokio::test(flavor = "multi_thread")]
async fn an_aborted_probe_leader_lets_the_waiter_reach_ready() {
    let go = Arc::new(AtomicBool::new(false));
    let fake = serve(Script::Hold(Arc::clone(&go), ready_body())).await;
    let registry = Arc::new(configured(&fake.base));
    let t0 = Instant::now();

    let leader = {
        let registry = Arc::clone(&registry);
        tokio::spawn(async move { knowledge_item(&registry, t0).await })
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while fake.hits.load(Ordering::SeqCst) < 1 {
        assert!(Instant::now() < deadline, "leader did not reach the remote");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut waiter = {
        let registry = Arc::clone(&registry);
        tokio::spawn(async move { knowledge_item(&registry, t0).await })
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while registry.inflight_receivers() < 1 {
        assert!(
            Instant::now() < deadline,
            "waiter did not join the flight, hits={}",
            fake.hits.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        fake.hits.load(Ordering::SeqCst),
        1,
        "waiter probed before the leader was cancelled"
    );

    leader.abort();
    match leader.await {
        Err(err) => assert!(err.is_cancelled(), "{err}"),
        Ok(_) => panic!("leader finished instead of being cancelled"),
    }

    let second = async {
        while fake.hits.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    // `&mut waiter` so a losing branch does not abort the task.
    tokio::select! {
        biased;
        joined = &mut waiter => {
            panic!("waiter finished before probing again: {:?}", joined.map(|item| item.state));
        }
        () = second => {}
        () = tokio::time::sleep(Duration::from_secs(2)) => {
            panic!(
                "waiter did not probe again, hits={}",
                fake.hits.load(Ordering::SeqCst)
            );
        }
    }

    go.store(true, Ordering::SeqCst);
    let item = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, ComponentState::Ready);
    assert!(item.reason.is_none());
}

fn bind(registry: &KnowledgeHostRegistry, binding: &str, seq: u64, base: &str) {
    registry
        .activate(
            "host",
            binding,
            seq,
            Endpoint {
                base_url: base.to_owned(),
                knowledge_base_ids: vec!["kb".into()],
            },
        )
        .unwrap();
}

fn push_bearer(registry: &KnowledgeHostRegistry, binding: &str, seq: u64, secret: &str) {
    registry
        .push_credential(
            "host",
            binding,
            seq,
            Credential::Bearer(secret.to_owned()),
            unix_now() + 600,
        )
        .unwrap();
}

fn document_with_ops(ops: &str) -> String {
    format!(
        r#"{{"code":0,"msg":"LEAK-union","data":{{"integration":{{"agent24_knowledge":{{"protocol_versions":[1],"mode":"integration","supported_operations":[{ops}],"server_version":"1.0.0",{limits}}}}}}}}}"#,
        limits = SCHEMA_LIMITS,
    )
}

/// Same URL, same credential sequence, two bindings, two secrets. The second
/// must not reuse the first binding's cached answer.
#[tokio::test]
async fn the_same_url_and_seq_on_another_binding_does_not_reuse_the_probe() {
    let secret_a = "LEAK-secret-binding-a";
    let secret_b = "LEAK-secret-binding-b";
    let degraded = document_with_ops(r#""search""#);
    let ready = ready_body();
    let fake = serve(Script::HoldAuth {
        go: Arc::new(AtomicBool::new(true)),
        pairs: vec![
            (secret_a.to_owned(), degraded),
            (secret_b.to_owned(), ready),
        ],
        fallback: document_with_ops(r#""search""#),
    })
    .await;
    let registry = KnowledgeHostRegistry::default();
    bind(&registry, "binding-a", 1, &fake.base);
    push_bearer(&registry, "binding-a", 2, secret_a);
    let t0 = Instant::now();
    let first = knowledge_item(&registry, t0).await;
    assert_eq!(shown(&first)["state"], "degraded");
    assert_eq!(shown(&first)["reason"], "missing_capability:read");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);
    assert_no_canary(&first, &[secret_a, secret_b]);

    registry
        .apply("host", "binding-a", 3, Change::Deactivate)
        .unwrap();
    bind(&registry, "binding-b", 1, &fake.base);
    push_bearer(&registry, "binding-b", 2, secret_b);
    let second = knowledge_item(&registry, t0 + Duration::from_millis(1)).await;
    let host = shown(&second);
    assert_eq!(host["state"], "ready", "binding B reused binding A's probe");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 2);
    assert_no_canary(&second, &[secret_a, secret_b]);
}

/// While binding A's probe is in flight, binding B with the same URL and
/// sequence must start its own probe. Joining A's flight would return A's body.
#[tokio::test(flavor = "multi_thread")]
async fn an_inflight_probe_does_not_cover_another_binding_with_the_same_seq() {
    let secret_a = "LEAK-secret-flight-a";
    let secret_b = "LEAK-secret-flight-b";
    let go = Arc::new(AtomicBool::new(false));
    let fake = serve(Script::HoldAuth {
        go: Arc::clone(&go),
        pairs: vec![
            (secret_a.to_owned(), document_with_ops(r#""search""#)),
            (secret_b.to_owned(), ready_body()),
        ],
        fallback: document_with_ops(r#""search""#),
    })
    .await;
    let registry = Arc::new(KnowledgeHostRegistry::default());
    bind(&registry, "binding-a", 1, &fake.base);
    push_bearer(&registry, "binding-a", 2, secret_a);
    let t0 = Instant::now();
    let first = {
        let registry = Arc::clone(&registry);
        tokio::spawn(async move { knowledge_item(&registry, t0).await })
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while fake.hits.load(Ordering::SeqCst) < 1 {
        assert!(Instant::now() < deadline, "binding A did not probe");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    registry
        .apply("host", "binding-a", 3, Change::Deactivate)
        .unwrap();
    bind(&registry, "binding-b", 1, &fake.base);
    push_bearer(&registry, "binding-b", 2, secret_b);
    let second = {
        let registry = Arc::clone(&registry);
        tokio::spawn(async move { knowledge_item(&registry, t0).await })
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while fake.hits.load(Ordering::SeqCst) < 2 {
        assert!(
            Instant::now() < deadline,
            "binding B joined binding A's flight, hits={}",
            fake.hits.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    go.store(true, Ordering::SeqCst);
    let first = first.await.unwrap();
    let second = second.await.unwrap();
    assert_eq!(shown(&first)["state"], "degraded");
    assert_eq!(shown(&second)["state"], "ready");
    assert_no_canary(&first, &[secret_a, secret_b]);
    assert_no_canary(&second, &[secret_a, secret_b]);
}

#[tokio::test]
async fn one_ready_endpoint_and_one_closed_port_is_failed() {
    let good = serve(Script::NoAuth(ready_body())).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let registry = KnowledgeHostRegistry::default();
    bind(&registry, "good", 1, &good.base);
    bind(&registry, "bad", 1, &format!("http://{addr}/api/v1"));
    let item = knowledge_item(&registry, Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "failed");
    assert_eq!(host["reason"], "unreachable");
    assert_eq!(host["detail"]["basis"], "capabilities");
    assert_eq!(good.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_operations_across_endpoints_are_unioned() {
    let search_only = serve(Script::Reply(200, document_with_ops(r#""search""#))).await;
    let read_only = serve(Script::Reply(200, document_with_ops(r#""read""#))).await;
    let registry = KnowledgeHostRegistry::default();
    bind(&registry, "search-only", 1, &search_only.base);
    bind(&registry, "read-only", 1, &read_only.base);
    let item = knowledge_item(&registry, Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "degraded");
    assert_eq!(host["reason"], "missing_capability:read");
    assert_eq!(
        host["detail"]["missing_capabilities"],
        json!(["read", "search"])
    );
    assert_eq!(search_only.hits.load(Ordering::SeqCst), 1);
    assert_eq!(read_only.hits.load(Ordering::SeqCst), 1);
}

/// The list handler may already have snapshotted C. Revoking it before the
/// request is sent must not put that key on the wire.
#[tokio::test]
async fn a_revoked_credential_is_not_sent_from_the_snapshotted_target() {
    let secret = "LEAK-stale-snapshot-secret";
    let fake = serve(Script::NoAuth(ready_body())).await;
    let registry = KnowledgeHostRegistry::default();
    bind(&registry, "binding", 1, &fake.base);
    push_bearer(&registry, "binding", 2, secret);
    let targets = registry.probe_targets(unix_now());
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].credential_epoch, 2);
    registry
        .apply("host", "binding", 3, Change::RevokeCredential)
        .unwrap();
    // The old target is not sent (the fake answers 500 to any credential
    // header), and its answer is not used: the snapshot is taken again, now
    // anonymous, and that one request is the only one the server sees.
    let view = registry.assess_snapshot(&targets, Instant::now()).await;
    assert_eq!(view.state, ComponentState::Ready, "{view:?}");
    assert_eq!(
        fake.hits.load(Ordering::SeqCst),
        1,
        "the revoked credential was sent, or the retake did not probe"
    );
    assert_eq!(fake.ready_hits(), 0, "no credential, no §8 probe");

    let fresh = knowledge_item(&registry, Instant::now()).await;
    assert_eq!(fresh.state, ComponentState::Ready);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1, "served from the cache");
    assert_no_canary(&fresh, &[secret]);
}

#[tokio::test]
async fn two_stalled_endpoints_share_one_probe_deadline() {
    let left = serve(Script::Stall).await;
    let right = serve(Script::Stall).await;
    let registry = KnowledgeHostRegistry::default();
    bind(&registry, "left", 1, &left.base);
    bind(&registry, "right", 1, &right.base);
    let started = Instant::now();
    let item = tokio::time::timeout(
        KNOWLEDGE_PROBE_TIMEOUT + Duration::from_secs(2),
        knowledge_item(&registry, Instant::now()),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < KNOWLEDGE_PROBE_TIMEOUT + Duration::from_millis(1500),
        "probes did not share one deadline: {elapsed:?}"
    );
    assert!(elapsed + Duration::from_millis(250) >= KNOWLEDGE_PROBE_TIMEOUT);
    let host = shown(&item);
    assert_eq!(host["state"], "failed");
    assert_eq!(host["reason"], "timeout");
    assert_eq!(left.hits.load(Ordering::SeqCst), 1);
    assert_eq!(right.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_numeric_code_that_is_not_exactly_zero_is_upstream_error() {
    let limits = SCHEMA_LIMITS;
    let declaration = format!(
        r#""protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0",{limits}"#
    );
    for (code, label) in [
        ("1.5", "one-and-a-half"),
        ("-0.5", "neg-half"),
        // A JSON integer, but past 2^53 a float names no single integer.
        ("1e30", "past-exact-float"),
    ] {
        let body = format!(
            r#"{{"code":{code},"msg":"LEAK-num-{label}","data":{{"integration":{{"agent24_knowledge":{{{declaration}}}}}}}}}"#
        );
        let fake = serve(Script::Reply(200, body)).await;
        let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
        let host = shown(&item);
        assert_eq!(host["state"], "failed", "{label}");
        assert_eq!(host["reason"], "upstream_error", "{label}");
        assert_no_canary(&item, &[&format!("LEAK-num-{label}")]);
    }
}

#[tokio::test]
async fn an_unknown_limits_key_is_still_ready() {
    let body = r#"{"code":0,"msg":"LEAK-extra-limit","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0","limits":{"max_results":50,"max_concurrency":4,"max_deadline_ms":30000,"max_read_bytes":262144,"max_page_limit":100,"max_snippet_bytes":2048,"future_limit":9}}}}}"#;
    let fake = serve(Script::Reply(200, body.to_owned())).await;
    let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "ready");
    assert_eq!(host["detail"]["basis"], "capabilities");
    assert_no_canary(&item, &["LEAK-extra-limit", "future_limit"]);
}

#[test]
fn adr_038_d5_names_timeout_and_http_errors_as_failed() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../docs/adr/ADR-038-component-contract-v1.md");
    let text = std::fs::read_to_string(&path).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains("远端服务（Knowledge）"))
        .unwrap();
    assert!(line.contains("超时"), "{line}");
    assert!(line.contains("非 2xx"), "{line}");
    assert!(line.contains("`failed`"), "{line}");
    assert!(line.contains("`degraded`"), "{line}");
    assert!(line.contains("search"), "{line}");
    assert!(line.contains("read"), "{line}");
    assert!(line.contains("capabilities"), "{line}");
    assert!(line.contains("R9.3"), "{line}");
    assert!(line.contains("不代表依赖健康"), "{line}");
    assert!(line.contains("§8"), "{line}");
    assert!(line.contains("not_ready"), "{line}");
    assert!(line.contains("`detail.basis` 为 `ready`"), "{line}");
    assert!(line.contains("D3"), "{line}");
    assert!(
        !line.contains("探测失败"),
        "probe failure is not the degraded bucket: {line}"
    );
    let c5 = text
        .lines()
        .find(|line| line.starts_with("| C5 |"))
        .unwrap();
    assert!(c5.contains("detail.basis"), "{c5}");
    assert!(c5.contains("capabilities"), "{c5}");
    assert!(c5.contains("R9.3"), "{c5}");
    assert!(c5.contains("§8"), "{c5}");
    assert!(c5.contains("not_ready"), "{c5}");
    assert!(c5.contains("D3"), "{c5}");
    assert!(!c5.contains("遗留：不探测"), "§8 is probed now: {c5}");
    for needle in ["审计", "R10", "4 秒", "逐 URL"] {
        assert!(c5.contains(needle), "{needle} not in {c5}");
    }
    let c6 = text
        .lines()
        .find(|line| line.starts_with("| C6 |"))
        .unwrap();
    for needle in [
        "expected_version",
        "component_contract_mismatch",
        "unavailable",
        "update_available",
        "已落地",
    ] {
        assert!(c6.contains(needle), "{needle} not in {c6}");
    }
}

// ---- ADR-038 C4: AgentEar on GET /api/v1/components, through the real route ----

const LEGACY: &str = "testtoken";
const MANIFEST: &str = "\
name: agentear
version: \"0.26.1\"
route_namespace: /api/v1/agentear
event_module: agentear
data_dir: ~/.agent24/os/agentear/
impl_kind: attached_process
kernel_capabilities: [events]
host_commands: [speak]
";

struct Ear {
    state: crate::server::AppState,
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    token: String,
    digest: String,
}

struct Link {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    task: tokio::task::JoinHandle<()>,
}

impl Ear {
    fn apply(&mut self, outcome: agent24_attach::attached::RegisterOutcome) {
        let response = match outcome {
            agent24_attach::attached::RegisterOutcome::Created(response)
            | agent24_attach::attached::RegisterOutcome::Rotated { response, .. } => response,
        };
        self.token = response.token;
        self.digest = response.manifest_digest;
    }

    fn rotate(&mut self) {
        let registry = std::sync::Arc::clone(&self.state.attach_registry);
        let outcome = agent24_attach::attached::register(
            &self.path,
            MANIFEST,
            false,
            |_| false,
            |change| {
                registry.on_change(change);
            },
        )
        .unwrap();
        self.apply(outcome);
    }

    fn set_disabled(&self, disabled: bool) {
        let registry = std::sync::Arc::clone(&self.state.attach_registry);
        let found =
            agent24_attach::attached::set_disabled(&self.path, "agentear", disabled, |change| {
                registry.on_change(change);
            })
            .unwrap();
        assert!(found);
    }
}

async fn ear() -> Ear {
    let state = server_tests::state().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("attached.json");
    let registry = std::sync::Arc::clone(&state.attach_registry);
    let outcome = agent24_attach::attached::register(
        &path,
        MANIFEST,
        false,
        |_| false,
        |change| {
            registry.on_change(change);
        },
    )
    .unwrap();
    let mut ear = Ear {
        state,
        _dir: dir,
        path,
        token: String::new(),
        digest: String::new(),
    };
    ear.apply(outcome);
    ear
}

async fn components(state: &crate::server::AppState) -> Value {
    let app = build_router(state.clone());
    let (status, body) = call(&app, Method::GET, GET, Some(LEGACY), "").await;
    assert_eq!(status, StatusCode::OK);
    body
}

async fn speak(state: &crate::server::AppState) -> (StatusCode, Value) {
    let app = build_router(state.clone());
    call(
        &app,
        Method::POST,
        "/api/v1/os/agentear/commands/speak",
        Some(LEGACY),
        r#"{"text":"hi"}"#,
    )
    .await
}

fn assert_agentear(body: &Value, state: &str) -> Value {
    let item = find(body, "agentear").clone();
    assert_eq!(item["id"], "agentear");
    assert_eq!(item["form"], "attach");
    assert_eq!(item["version"], "0.26.1");
    assert_eq!(item["provides"], json!([]));
    assert_eq!(item["state"], state);
    for banned in ["absent", "installing", "installed"] {
        assert_ne!(item["state"], banned);
    }
    item
}

async fn open(ear: &Ear, versions: Option<Value>) -> Link {
    let (kernel, mut module) = UnixStream::pair().unwrap();
    let registry = std::sync::Arc::clone(&ear.state.attach_registry);
    let task = tokio::spawn(async move {
        agent24_attach::attach_listener::handle_connection(
            kernel,
            &registry,
            CancellationToken::new(),
        )
        .await;
    });
    let mut params = json!({
        "module": "agentear",
        "manifest_digest": ear.digest,
        "auth_token": ear.token,
        "capabilities": ["events"],
    });
    if let Some(versions) = versions {
        params["protocol_versions"] = versions;
    }
    let mut frame = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": "1",
        "method": "initialize",
        "params": params,
    }))
    .unwrap();
    frame.push(b'\n');
    module.write_all(&frame).await.unwrap();
    let (read, writer) = module.into_split();
    Link {
        reader: BufReader::new(read),
        writer,
        task,
    }
}

async fn next_frame(link: &mut Link) -> Option<Value> {
    let mut line = String::new();
    let n = link.reader.read_line(&mut line).await.unwrap();
    if n == 0 {
        None
    } else {
        Some(serde_json::from_str(line.trim()).unwrap())
    }
}

impl Link {
    async fn finish(mut self) {
        let _ = self.writer.shutdown().await;
        drop(self.reader);
        match tokio::time::timeout(Duration::from_secs(2), self.task).await {
            Ok(Ok(())) => {}
            other => panic!("attach connection did not end: {other:?}"),
        }
    }

    async fn reply(&mut self, frame: &Value) {
        let mut line = serde_json::to_vec(frame).unwrap();
        line.push(b'\n');
        self.writer.write_all(&line).await.unwrap();
    }
}

async fn handshake(ear: &Ear) -> Link {
    let mut link = open(ear, Some(json!({"min": 1, "max": 1}))).await;
    let ok = next_frame(&mut link).await.unwrap();
    assert!(ok.get("result").is_some(), "handshake refused: {ok}");
    assert!(ok.get("error").is_none(), "{ok}");
    link
}

/// D5 attach subset, through register + the real listener + `GET /components`.
/// Not connected is `stopped`; a finished handshake is `ready`; dropping the
/// connection (the generation is revoked) is `stopped` again. A second
/// connection that fails the range check does not cover the live one.
#[tokio::test]
async fn agentear_lists_stopped_ready_and_stopped_again() {
    let ear = ear().await;
    let item = assert_agentear(&components(&ear.state).await, "stopped");
    assert!(item.get("reason").is_none());
    assert!(item.get("detail").is_none());

    let live = handshake(&ear).await;
    let item = assert_agentear(&components(&ear.state).await, "ready");
    assert!(item.get("reason").is_none());

    let mut second = open(&ear, Some(json!({"min": 0, "max": 0}))).await;
    let err = next_frame(&mut second).await.unwrap();
    assert_eq!(err["error"]["data"]["kind"], "version_mismatch");
    assert!(next_frame(&mut second).await.is_none(), "{err}");
    second.finish().await;
    assert_agentear(&components(&ear.state).await, "ready");

    live.finish().await;
    let item = assert_agentear(&components(&ear.state).await, "stopped");
    assert_ne!(item["state"], "stopping");
    assert_ne!(item["reason"], "component_contract_mismatch");
}

/// §3 case 2. The peer's protocol range does not overlap (both directions,
/// and a missing range). State becomes `incompatible`, the wire error stays
/// `version_mismatch`, and the socket closes with no business frame. A
/// command route answers `503` and does not retry. Disable wins over the
/// mismatch. A later good handshake, and a re-registration, clear it.
#[tokio::test]
async fn adr038_case2_protocol_mismatch_is_incompatible_and_sends_no_call() {
    let mut ear = ear().await;
    let hi = agent24_domain::DAEMON_PROTOCOL_VERSION + 1;
    let expected = agent24_os_proto::version::kernel_range().to_string();
    let cases = [
        (Some(json!({"min": 0, "max": 0})), "[0, 0]".to_owned()),
        (Some(json!({"min": hi, "max": hi})), format!("[{hi}, {hi}]")),
        (None, "not_declared".to_owned()),
    ];
    for (versions, actual) in cases {
        let mut link = open(&ear, versions).await;
        let err = next_frame(&mut link).await.unwrap();
        assert_eq!(err["error"]["data"]["kind"], "version_mismatch", "{err}");
        assert!(
            next_frame(&mut link).await.is_none(),
            "a business frame followed the handshake error: {err}"
        );
        link.finish().await;
        let item = assert_agentear(&components(&ear.state).await, "incompatible");
        assert_eq!(item["reason"], "component_contract_mismatch");
        assert_eq!(item["detail"]["id"], "agentear");
        assert_eq!(item["detail"]["expected"], expected);
        assert_eq!(item["detail"]["actual"], actual);
        let (status, body) = speak(&ear.state).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body["error"]["code"], "module_not_ready");
    }

    ear.set_disabled(true);
    let item = assert_agentear(&components(&ear.state).await, "stopped");
    assert_eq!(item["reason"], "disabled");
    assert!(item.get("detail").is_none());
    ear.set_disabled(false);
    assert_agentear(&components(&ear.state).await, "incompatible");

    let live = handshake(&ear).await;
    assert_agentear(&components(&ear.state).await, "ready");
    live.finish().await;
    let item = assert_agentear(&components(&ear.state).await, "stopped");
    assert!(item.get("reason").is_none(), "{item}");

    let mut again = open(&ear, Some(json!({"min": 0, "max": 0}))).await;
    let err = next_frame(&mut again).await.unwrap();
    assert_eq!(err["error"]["data"]["kind"], "version_mismatch");
    again.finish().await;
    assert_agentear(&components(&ear.state).await, "incompatible");
    ear.rotate();
    let item = assert_agentear(&components(&ear.state).await, "stopped");
    assert!(item.get("reason").is_none(), "{item}");
}

/// A range that already negotiated must not leave an earlier mismatch in
/// place when `commit` refuses because the module is disabled. Re-enabling
/// then shows `stopped`, with no `component_contract_mismatch`.
#[tokio::test]
async fn a_legal_handshake_while_disabled_clears_the_contract_mismatch() {
    let ear = ear().await;
    let mut bad = open(&ear, Some(json!({"min": 0, "max": 0}))).await;
    let err = next_frame(&mut bad).await.unwrap();
    assert_eq!(err["error"]["data"]["kind"], "version_mismatch", "{err}");
    bad.finish().await;
    let item = assert_agentear(&components(&ear.state).await, "incompatible");
    assert_eq!(item["reason"], "component_contract_mismatch");

    ear.set_disabled(true);
    let mut legal = open(&ear, Some(json!({"min": 1, "max": 1}))).await;
    let err = next_frame(&mut legal).await.unwrap();
    assert_eq!(err["error"]["data"]["kind"], "forbidden", "{err}");
    assert!(
        next_frame(&mut legal).await.is_none(),
        "a business frame followed forbidden: {err}"
    );
    legal.finish().await;

    ear.set_disabled(false);
    let item = assert_agentear(&components(&ear.state).await, "stopped");
    assert_ne!(item["reason"], "component_contract_mismatch");
    assert!(item.get("reason").is_none(), "{item}");
    assert!(item.get("detail").is_none(), "{item}");
}

/// §3 case 3. Unknown fields in the module's command result (and an extra
/// envelope field) are ignored. The call succeeds and the component stays
/// `ready`.
#[tokio::test]
async fn adr038_case3_unknown_fields_in_the_command_result_still_work() {
    let ear = ear().await;
    let mut link = handshake(&ear).await;
    let state = ear.state.clone();
    let http = tokio::spawn(async move { speak(&state).await });
    let invoke = next_frame(&mut link).await.unwrap();
    assert_eq!(invoke["method"], "_a24/command/invoke", "{invoke}");
    link.reply(&json!({
        "jsonrpc": "2.0",
        "id": invoke["id"],
        "result": {"accepted": true, "future_field": 1},
        "vendor": "x",
    }))
    .await;
    let (status, body) = http.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["accepted"], true);
    assert_eq!(body["result"]["future_field"], 1);
    assert_agentear(&components(&ear.state).await, "ready");
    link.finish().await;
}

/// §3 case 4, JSON-RPC binding (D6): one invoke, then one `$/cancelRequest`
/// notification, no retry. The call is `504 timeout` and the component stays
/// `ready` — the outcome is unknown, not `failed`.
#[tokio::test]
async fn adr038_case4_timeout_sends_cancel_and_does_not_retry() {
    let ear = ear().await;
    let mut link = handshake(&ear).await;
    let state = ear.state.clone();
    let http = tokio::spawn(async move { speak(&state).await });
    let invoke = next_frame(&mut link).await.unwrap();
    assert_eq!(invoke["method"], "_a24/command/invoke", "{invoke}");
    let cancel = tokio::time::timeout(Duration::from_secs(8), next_frame(&mut link))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancel["method"], "$/cancelRequest", "{cancel}");
    assert!(
        cancel.get("id").is_none(),
        "cancel is a notification: {cancel}"
    );
    assert_eq!(cancel["params"]["id"], invoke["id"]);
    let extra = tokio::time::timeout(Duration::from_millis(300), next_frame(&mut link)).await;
    assert!(extra.is_err(), "a second frame was written: {extra:?}");
    let (status, body) = http.await.unwrap();
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert_eq!(body["error"]["code"], "timeout");
    assert_agentear(&components(&ear.state).await, "ready");
    link.finish().await;
    assert_agentear(&components(&ear.state).await, "stopped");
}

/// DOC-1-06b §3.1, the seven tools `POST /_a24/tools/{op}` actually dispatches.
const DOC_TOOLS: &[&str] = &[
    "documents.extract",
    "documents.extraction_get",
    "documents.find",
    "documents.get",
    "documents.job_get",
    "documents.list",
    "documents.read_range",
];

fn sorted_tools(names: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut names: Vec<String> = names.into_iter().map(|n| n.as_ref().to_owned()).collect();
    names.sort();
    names
}

/// Quoted match arms of `dispatch`'s `match op` in `agent24-documents`.
///
/// A small scanner, not line matching: it skips comments, strings and char
/// literals while it counts braces, takes only the string patterns that sit
/// directly in the `match op { .. }` block (depth 1, so a nested `match`'s arms
/// are not ours), and lets `=>` follow on a later line or after `|` alternatives.
fn dispatched_tool_names() -> Vec<String> {
    let src = include_str!("../../../agent24-documents/src/tools.rs");
    sorted_tools(match_op_arms(src))
}

fn match_op_arms(src: &str) -> Vec<String> {
    let start = src.find("match op {").unwrap() + "match op {".len();
    let bytes = src.as_bytes();
    let mut i = start;
    let mut depth = 1usize;
    // Strings seen at depth 1 since the last `=>` or `,`: one arm's patterns.
    let mut pending: Vec<String> = Vec::new();
    let mut names = Vec::new();
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..].find("*/").unwrap();
                i += end + 4;
            }
            b'"' => {
                let mut j = i + 1;
                let mut text = String::new();
                while bytes[j] != b'"' {
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    text.push(bytes[j] as char);
                    j += 1;
                }
                if depth == 1 {
                    pending.push(text);
                }
                i = j + 1;
            }
            b'\'' => {
                // A char literal ('x', '\n'), not a lifetime ('a).
                let rest = &bytes[i + 1..];
                let len = match rest {
                    [b'\\', _, b'\'', ..] => 4,
                    [_, b'\'', ..] => 3,
                    _ => 1,
                };
                i += len;
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
            }
            b'=' if depth == 1 && bytes.get(i + 1) == Some(&b'>') => {
                names.extend(pending.drain(..).map(|name| format!("documents.{name}")));
                i += 2;
            }
            b',' if depth == 1 => {
                pending.clear();
                i += 1;
            }
            _ => i += 1,
        }
    }
    names
}

fn mount_report(outcome: MountOutcome) -> crate::domain::MountReport {
    mount_report_at(outcome, "9.9.9")
}

fn mount_report_at(outcome: MountOutcome, version: &str) -> crate::domain::MountReport {
    crate::domain::MountReport {
        name: "documents".into(),
        namespace: "/api/v1/documents".into(),
        version: version.into(),
        enabled_at_start: Some(true),
        outcome,
        granted: Vec::new(),
        resources: crate::domain::ResourceStatus::NotChecked,
        d9: None,
    }
}

fn failure() -> agent24_os_proto::failure::RunFailure {
    agent24_os_proto::failure::RunFailure::new(
        agent24_os_proto::failure::FailureKind::Exited,
        "gone",
    )
}

#[test]
fn documents_d9_catalog_parses_the_module_tools() {
    let catalog = documents_catalog::DocumentsCatalog::embedded().unwrap();
    assert_eq!(catalog.id, "documents");
    assert_eq!(catalog.version, "0.2.0");
    assert_eq!(catalog.name.en, "Documents");
    assert_eq!(catalog.name.zh, "文档");
    assert!(!catalog.name.th.is_empty());
    assert!(catalog.assurance.is_empty());
    assert_eq!(catalog.requires.kernel, ["events", "models"]);
    assert!(catalog.requires.operations.is_empty());
    let mut with_op: Value =
        serde_json::from_str(include_str!("../../../agent24-documents/component.json")).unwrap();
    with_op["requires"]["operations"] = json!(["models.read"]);
    assert!(
        documents_catalog::DocumentsCatalog::load(Some(&with_op.to_string())).is_ok(),
        "a canonical Operation name is a valid requires.operations entry"
    );
    with_op["requires"]["operations"] = json!(["models/read"]);
    assert!(
        documents_catalog::DocumentsCatalog::load(Some(&with_op.to_string())).is_err(),
        "a slash alias is not a canonical operation name"
    );
    // domain-os.yml is parsed, not searched: a comment or another key that
    // happens to contain the text cannot satisfy it.
    let manifest: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../../../agent24-documents/domain-os.yml")).unwrap();
    assert_eq!(
        manifest["version"].as_str(),
        Some(catalog.version.as_str()),
        "catalog version drifts from domain-os.yml"
    );
    let mut kernel: Vec<&str> = manifest["kernel_capabilities"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    kernel.sort_unstable();
    let mut wanted: Vec<&str> = catalog.requires.kernel.iter().map(String::as_str).collect();
    wanted.sort_unstable();
    assert_eq!(
        kernel, wanted,
        "requires.kernel drifts from kernel_capabilities"
    );
    assert_eq!(sorted_tools(catalog.tool_names()), DOC_TOOLS);
    assert_eq!(dispatched_tool_names(), DOC_TOOLS);
    let item = documents_from(None, None, Ok(catalog));
    assert_eq!(item.form, Form::Resident);
    assert_eq!(item.state, ComponentState::Absent);
    assert_eq!(item.version.as_deref(), Some("0.2.0"));
    assert!(item.reason.is_none());
    assert_eq!(item.provides.len(), 1);
    assert_eq!(item.provides[0].id, "documents");
    assert_eq!(sorted_tools(item.provides[0].tools.iter()), DOC_TOOLS);
    let raw: Value =
        serde_json::from_str(include_str!("../../../agent24-documents/component.json")).unwrap();
    assert_eq!(
        item.provides[0].summary.as_ref(),
        Some(&raw["provides"][0]["summary"]),
        "summary is the catalog's, all three languages"
    );
    assert!(
        item.provides[0].summary.as_ref().unwrap()["th"]
            .as_str()
            .is_some_and(|t| !t.is_empty())
    );
    assert!(item.form.allows(item.state));
}

#[test]
fn documents_d9_missing_or_broken_catalog_is_an_error() {
    use documents_catalog::DocumentsCatalog;
    assert!(matches!(
        DocumentsCatalog::load(None),
        Err(documents_catalog::CatalogError::Missing)
    ));
    for raw in ["", "   ", "{", "[]", "null", "{}"] {
        assert!(DocumentsCatalog::load(Some(raw)).is_err(), "{raw}");
    }
    let raw = include_str!("../../../agent24-documents/component.json");
    let mut value: Value = serde_json::from_str(raw).unwrap();
    let provides = value.as_object_mut().unwrap().remove("provides").unwrap();
    value["capabilities"] = provides;
    assert!(
        DocumentsCatalog::load(Some(&value.to_string())).is_err(),
        "capabilities is not a catalog field"
    );
    let mut value: Value = serde_json::from_str(raw).unwrap();
    value["provides"][0]["id"] = json!("A");
    assert!(DocumentsCatalog::load(Some(&value.to_string())).is_err());
    let mut value: Value = serde_json::from_str(raw).unwrap();
    value["provides"][0]["tools"] = json!([]);
    assert!(DocumentsCatalog::load(Some(&value.to_string())).is_err());
    let mut value: Value = serde_json::from_str(raw).unwrap();
    value["skills"] = json!([{"id": "later"}]);
    assert!(DocumentsCatalog::load(Some(&value.to_string())).is_err());
    let mut value: Value = serde_json::from_str(raw).unwrap();
    value["extra"] = json!(1);
    assert!(DocumentsCatalog::load(Some(&value.to_string())).is_err());

    // One mutation per validate() guard. Dropping that guard makes this case Ok.
    type CatalogCase = (&'static str, fn(&mut Value));
    let cases: &[CatalogCase] = &[
        ("id", |v| {
            v["id"] = json!("docs");
            for tool in v["provides"][0]["tools"].as_array_mut().unwrap() {
                *tool = json!(tool.as_str().unwrap().replacen("documents.", "docs.", 1));
            }
        }),
        ("version", |v| v["version"] = json!("0.1.0 ")),
        ("name", |v| v["name"]["en"] = json!("")),
        ("provides", |v| v["provides"] = json!([])),
        ("provides.id", |v| {
            let mut second = v["provides"][0].clone();
            second["tools"] = json!(["documents.other_op"]);
            v["provides"].as_array_mut().unwrap().push(second);
        }),
        ("provides.name", |v| {
            v["provides"][0]["name"]["en"] = json!("")
        }),
        ("provides.summary", |v| {
            v["provides"][0]["summary"]["zh"] = json!("");
        }),
        ("tool-shape", |v| {
            v["provides"][0]["tools"][0] = json!("documents.Bad")
        }),
        ("tool-unique", |v| {
            let mut second = v["provides"][0].clone();
            second["id"] = json!("docs-read");
            v["provides"].as_array_mut().unwrap().push(second);
        }),
        ("kernel", |v| v["requires"]["kernel"] = json!(["network"])),
        ("kernel-unique", |v| {
            v["requires"]["kernel"] = json!(["events", "events"]);
        }),
        ("operations", |v| {
            v["requires"]["operations"] = json!(["list"])
        }),
        ("operations-unique", |v| {
            v["requires"]["operations"] = json!(["models.read", "models.read"]);
        }),
        // Well formed, but not `<this component's id>.<operation>`.
        ("tool-component", |v| {
            v["provides"][0]["tools"][0] = json!("sin90.list");
        }),
        ("tool-no-dot", |v| {
            v["provides"][0]["tools"][0] = json!("list");
        }),
        ("assurance", |v| {
            v["assurance"] = json!([{"id": "", "source": "x"}]);
        }),
    ];
    for (label, mutate) in cases {
        let mut value: Value = serde_json::from_str(raw).unwrap();
        mutate(&mut value);
        assert!(
            DocumentsCatalog::load(Some(&value.to_string())).is_err(),
            "{label} accepted: {value}"
        );
    }
}

#[test]
fn documents_d9_broken_catalog_degrades_without_panic() {
    let broken = documents_catalog::DocumentsCatalog::load(None);
    let absent = documents_from(None, None, broken);
    assert_eq!(absent.state, ComponentState::Absent);
    assert!(absent.provides.is_empty());
    assert!(absent.version.is_none());
    assert_eq!(absent.reason.as_deref(), Some("catalog_invalid"));
    assert_eq!(absent.form, Form::Resident);
    assert!(absent.form.allows(absent.state));

    let report = mount_report(MountOutcome::Mounted);
    let running = documents_from(
        Some(&report),
        Some(&Supervised::Running),
        documents_catalog::DocumentsCatalog::load(Some("{")),
    );
    assert_eq!(running.state, ComponentState::Ready);
    assert_eq!(running.version.as_deref(), Some("9.9.9"));
    assert!(running.provides.is_empty());
    assert_eq!(running.reason.as_deref(), Some("catalog_invalid"));
    assert!(running.form.allows(running.state));

    let backoff = Supervised::Backoff {
        failures: 2,
        delay: Duration::from_secs(1),
        last: failure(),
    };
    let retrying = documents_from(
        Some(&report),
        Some(&backoff),
        documents_catalog::DocumentsCatalog::load(Some("")),
    );
    assert_eq!(retrying.state, ComponentState::Failed);
    assert_eq!(retrying.reason.as_deref(), Some("restarting"));
    assert!(retrying.provides.is_empty());
}

#[test]
fn documents_d9_states_stay_in_the_resident_subset() {
    use std::time::Duration as Wait;
    let gone = failure();
    let cases = [
        (
            MountOutcome::Mounted,
            Some(Supervised::Starting {
                attempt: 1,
                after: None,
            }),
            ComponentState::Starting,
            None,
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Running),
            ComponentState::Ready,
            None,
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Stopping),
            ComponentState::Stopping,
            None,
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Stopped),
            ComponentState::Stopped,
            None,
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Backoff {
                failures: 1,
                delay: Wait::from_secs(1),
                last: gone.clone(),
            }),
            ComponentState::Failed,
            Some("restarting"),
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::GaveUp {
                failures: 3,
                within: Wait::from_secs(60),
                last: gone,
            }),
            ComponentState::Failed,
            Some("gave_up"),
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::PackageChanged {
                reason: "digest".into(),
                code: None,
            }),
            ComponentState::Failed,
            Some("package_changed"),
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::StopFailed {
                error: "busy".into(),
            }),
            ComponentState::Failed,
            Some("stop_failed"),
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Panicked),
            ComponentState::Failed,
            Some("panicked"),
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Killed),
            ComponentState::Failed,
            Some("killed"),
        ),
        (MountOutcome::Mounted, None, ComponentState::Ready, None),
        (
            MountOutcome::Disabled,
            Some(Supervised::Running),
            ComponentState::Stopped,
            Some("disabled"),
        ),
        (
            MountOutcome::Degraded("disk".into()),
            None,
            ComponentState::Failed,
            Some("degraded"),
        ),
        (
            MountOutcome::Refused("duplicate".into()),
            None,
            ComponentState::Failed,
            Some("refused"),
        ),
    ];
    assert_eq!(documents_form(), Form::Resident);
    for (outcome, live, want, reason) in cases {
        let (state, got, detail) = documents_mapped(&outcome, live.as_ref());
        assert_eq!(state, want, "{outcome:?}");
        assert_eq!(got, reason, "{outcome:?}");
        assert!(
            documents_form().allows(state),
            "{state:?} is outside the resident subset"
        );
        if matches!(
            outcome,
            MountOutcome::Degraded(_) | MountOutcome::Refused(_)
        ) {
            assert!(detail.is_some());
        }
    }
    let report = mount_report_at(MountOutcome::Mounted, "0.2.0");
    let item = documents_from(
        Some(&report),
        Some(&Supervised::Running),
        documents_catalog::DocumentsCatalog::embedded(),
    );
    assert_eq!(item.state, ComponentState::Ready);
    assert_eq!(item.version.as_deref(), Some("0.2.0"));
    assert_eq!(item.form, Form::Resident);
    assert!(!item.provides.is_empty());
    assert!(item.form.allows(item.state));
}

#[tokio::test]
async fn documents_d9_entry_lists_provides_and_redacts() {
    let (app, host, _, first_party) = app().await;
    let (status, full) = call(&app, Method::GET, GET, Some(&host), "").await;
    assert_eq!(status, StatusCode::OK);
    let doc = find(&full, "documents");
    assert_eq!(doc["state"], "absent");
    assert_eq!(doc["form"], "resident");
    assert_eq!(doc["version"], "0.2.0");
    assert!(doc.get("reason").is_none());
    assert_eq!(doc["provides"][0]["id"], "documents");
    assert_eq!(doc["provides"][0]["name"]["en"], "Local documents");
    let tools = sorted_tools(
        doc["provides"][0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap()),
    );
    assert_eq!(tools, DOC_TOOLS);

    let (status, redacted) = call(&app, Method::GET, GET, Some(&first_party), "").await;
    assert_eq!(status, StatusCode::OK);
    let doc = find(&redacted, "documents");
    let mut keys: Vec<_> = doc.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["id", "provides", "state", "version"]);
    assert_eq!(doc["provides"][0]["id"], "documents");
    assert_eq!(
        sorted_tools(
            doc["provides"][0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap())
        ),
        DOC_TOOLS
    );
    let text = doc.to_string();
    for leaked in ["reason", "form", "detail", "catalog_invalid", "port", "pid"] {
        assert!(!text.contains(leaked), "{leaked} leaked: {text}");
    }
}

// ---- L3: §8 readiness, stale snapshots, JSON number values, D3 gate ----

fn limits_with(value: &str) -> String {
    SCHEMA_LIMITS.replacen("\"max_results\":50", &format!("\"max_results\":{value}"), 1)
}

fn document_with_limits(limits: &str) -> String {
    format!(
        r#"{{"code":0,"msg":"LEAK-num","data":{{"integration":{{"agent24_knowledge":{{"protocol_versions":[1],"mode":"integration","supported_operations":["search","read"],"server_version":"1.0.0",{limits}}}}}}}}}"#
    )
}

fn with_code(code: &str) -> String {
    document_with_ops(r#""search","read""#).replacen("\"code\":0", &format!("\"code\":{code}"), 1)
}

/// JSON Schema counts `-0`, `0.0` and `0e5` as the integer 0: still success.
#[tokio::test]
async fn an_envelope_code_zero_is_success_in_any_integer_spelling() {
    for code in ["0", "-0", "0.0", "-0.0", "0e5", "0E0"] {
        let fake = serve(Script::Reply(200, with_code(code))).await;
        let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
        assert_eq!(shown(&item)["state"], "ready", "code {code}");
    }
    // Not zero, however it is written.
    for (code, reason) in [
        ("1.0", "upstream_1"),
        ("1e3", "upstream_1000"),
        ("-2.0", "upstream_-2"),
    ] {
        let fake = serve(Script::Reply(200, with_code(code))).await;
        let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
        let host = shown(&item);
        assert_eq!(host["state"], "failed", "code {code}");
        assert_eq!(host["reason"], reason, "code {code}");
    }
}

/// `positive_int` is an integer value >= 1: `1e3` and `1.0` pass, `0.0`, `-0`
/// and `0.5` do not.
#[tokio::test]
async fn limits_are_positive_integers_by_value() {
    for value in ["1e3", "1.0", "50.0", "2E1"] {
        let fake = serve(Script::Reply(
            200,
            document_with_limits(&limits_with(value)),
        ))
        .await;
        let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
        assert_eq!(shown(&item)["state"], "ready", "max_results {value}");
    }
    for value in ["0.0", "-0", "0e3", "0.5", "-1.0"] {
        let fake = serve(Script::Reply(
            200,
            document_with_limits(&limits_with(value)),
        ))
        .await;
        let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
        let host = shown(&item);
        assert_eq!(host["state"], "failed", "max_results {value}");
        assert_eq!(host["reason"], "malformed", "max_results {value}");
    }
}

fn with_credential(base: &str, secret: &str) -> KnowledgeHostRegistry {
    let registry = configured(base);
    push_bearer(&registry, "binding", 2, secret);
    registry
}

#[tokio::test]
async fn a_credential_and_a_ready_service_is_ready_on_the_ready_probe() {
    let secret = "LEAK-ready-secret";
    let fake = serve(Script::Reply(200, ready_body())).await;
    let registry = with_credential(&fake.base, secret);
    let item = knowledge_item(&registry, Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "ready");
    assert_eq!(host["detail"]["basis"], "ready");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1, "capabilities once");
    assert_eq!(fake.ready_hits(), 1);
    let seen = fake.ready.seen.lock().unwrap().clone();
    let (head, body) = &seen[0];
    assert!(
        head.starts_with("POST /api/v1/agent24/knowledge/ready "),
        "{head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains(&format!("bearer {}", secret.to_ascii_lowercase()))
    );
    let body: Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["protocol_version"], 1);
    assert!(body["request_id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(body.get("scope").is_none(), "R8.2: scope is optional");
    assert_no_canary(&item, &[secret]);
}

#[tokio::test]
async fn no_credential_means_no_ready_probe_and_the_basis_stays_capabilities() {
    let fake = serve(Script::Reply(200, ready_body())).await;
    let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
    let host = shown(&item);
    assert_eq!(host["state"], "ready");
    assert_eq!(host["detail"]["basis"], "capabilities");
    assert_eq!(fake.ready_hits(), 0);
}

/// Not ready is `degraded`/`not_ready` for every way the answer can fail to say
/// "live and search and read ready"; the body text never reaches the view.
#[tokio::test]
async fn a_ready_probe_that_does_not_say_ready_is_degraded_not_ready() {
    let modes = [
        ("search not_ready", ReadyMode::SearchNotReady),
        ("not live", ReadyMode::NotLive),
        ("wrong echo", ReadyMode::WrongEcho),
        ("http 503", ReadyMode::Status(503)),
        ("http 401", ReadyMode::Status(401)),
        ("garbage", ReadyMode::Garbage),
    ];
    for (label, mode) in modes {
        let fake = serve(Script::Reply(200, ready_body())).await;
        fake.ready_mode(mode);
        let registry = with_credential(&fake.base, "LEAK-nr-secret");
        let item = knowledge_item(&registry, Instant::now()).await;
        let host = shown(&item);
        assert_eq!(host["state"], "degraded", "{label}");
        assert_eq!(host["reason"], "not_ready", "{label}");
        assert_eq!(host["detail"]["basis"], "ready", "{label}");
        assert_eq!(fake.ready_hits(), 1, "{label}");
        assert_no_canary(&item, &["LEAK-ready-body", "LEAK-nr-secret"]);
    }
}

#[tokio::test]
async fn a_ready_probe_that_never_answers_is_degraded_inside_the_shared_deadline() {
    let fake = serve(Script::Reply(200, ready_body())).await;
    fake.ready_mode(ReadyMode::Stall);
    let registry = with_credential(&fake.base, "LEAK-stall-secret");
    let started = Instant::now();
    let item = tokio::time::timeout(
        KNOWLEDGE_PROBE_TIMEOUT + Duration::from_secs(2),
        knowledge_item(&registry, Instant::now()),
    )
    .await
    .unwrap();
    assert!(started.elapsed() < KNOWLEDGE_PROBE_TIMEOUT + Duration::from_millis(1500));
    let host = shown(&item);
    assert_eq!(host["state"], "degraded");
    assert_eq!(host["reason"], "not_ready");
}

/// "Capabilities first": an incompatible or incomplete declaration is the
/// answer, and the service is not asked about readiness.
#[tokio::test]
async fn the_ready_probe_waits_for_a_compatible_declaration() {
    let incompatible =
        ready_body().replace("\"protocol_versions\":[1,2]", "\"protocol_versions\":[2]");
    let fake = serve(Script::Reply(200, incompatible)).await;
    let item = knowledge_item(&with_credential(&fake.base, "LEAK-s1"), Instant::now()).await;
    assert_eq!(shown(&item)["state"], "incompatible");
    assert_eq!(shown(&item)["detail"]["basis"], "capabilities");
    assert_eq!(fake.ready_hits(), 0);

    let fake = serve(Script::Reply(200, document_with_ops(r#""search""#))).await;
    let item = knowledge_item(&with_credential(&fake.base, "LEAK-s2"), Instant::now()).await;
    assert_eq!(shown(&item)["reason"], "missing_capability:read");
    assert_eq!(shown(&item)["detail"]["basis"], "capabilities");
    assert_eq!(fake.ready_hits(), 0);

    let fake = serve(Script::Reply(500, "x".into())).await;
    let item = knowledge_item(&with_credential(&fake.base, "LEAK-s3"), Instant::now()).await;
    assert_eq!(shown(&item)["state"], "failed");
    assert_eq!(fake.ready_hits(), 0);
}

/// A credential that moves between the capabilities answer and the §8 request
/// is not sent again: the stale answer is dropped and the snapshot retaken.
#[tokio::test]
async fn the_ready_probe_rechecks_the_credential_before_sending() {
    use agent24_knowledge::probe::KnowledgeProbeCache;
    let fake = serve(Script::Reply(200, ready_body())).await;
    let registry = with_credential(&fake.base, "LEAK-moves-secret");
    let targets = registry.probe_targets(unix_now());
    let cache = KnowledgeProbeCache::default();
    // Valid for the capabilities request, revoked by the time §8 would go.
    let calls = AtomicUsize::new(0);
    let current =
        |_: &agent24_knowledge::probe::ProbeTarget| calls.fetch_add(1, Ordering::SeqCst) == 0;
    let assessed = cache.assess(&targets, Instant::now(), &current).await;
    assert!(assessed.stale);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        fake.ready_hits(),
        0,
        "the key went out again after it moved"
    );
}

/// N1 of #1571. B's credential moved after the snapshot, so B's target was
/// dropped and the answer was A's `ready`. B is a failing endpoint: the answer
/// must be B's, from a new snapshot.
#[tokio::test]
async fn a_target_that_failed_the_recheck_is_probed_again_not_dropped() {
    let a = serve(Script::Reply(200, ready_body())).await;
    let b = serve(Script::Reply(500, "boom".into())).await;
    let registry = KnowledgeHostRegistry::default();
    bind(&registry, "a", 1, &a.base);
    bind(&registry, "b", 1, &b.base);
    push_bearer(&registry, "b", 2, "LEAK-b-old");
    let snapshot = registry.probe_targets(unix_now());
    assert_eq!(snapshot.len(), 2);
    push_bearer(&registry, "b", 3, "LEAK-b-new");
    let view = registry.assess_snapshot(&snapshot, Instant::now()).await;
    assert_eq!(view.state, ComponentState::Failed, "{view:?}");
    assert_eq!(view.reason.as_deref(), Some("http_500"));
    assert_eq!(
        b.hits.load(Ordering::SeqCst),
        1,
        "only the retaken snapshot sent"
    );
    assert_eq!(
        b.ready_hits(),
        0,
        "a 500 capabilities answer is not asked about §8"
    );
}

#[tokio::test]
async fn a_stale_answer_is_not_cached_and_bindings_that_keep_moving_fail_closed() {
    use agent24_knowledge::host::assess_until_fresh;
    use agent24_knowledge::probe::KnowledgeProbeCache;
    let fake = serve(Script::Reply(200, ready_body())).await;
    let registry = configured(&fake.base);
    let targets = registry.probe_targets(unix_now());
    let cache = KnowledgeProbeCache::default();

    let stale = cache.assess(&targets, Instant::now(), &|_| false).await;
    assert!(stale.stale);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
    let fresh = cache.assess(&targets, Instant::now(), &|_| true).await;
    assert!(!fresh.stale);
    assert_eq!(fresh.view.state, ComponentState::Ready);
    assert_eq!(
        fake.hits.load(Ordering::SeqCst),
        1,
        "the stale answer was cached"
    );

    let retakes = AtomicUsize::new(0);
    let view = assess_until_fresh(
        &KnowledgeProbeCache::default(),
        &targets,
        Instant::now(),
        &|_| false,
        &|| {
            retakes.fetch_add(1, Ordering::SeqCst);
            registry.probe_targets(unix_now())
        },
        None,
    )
    .await
    .view;
    assert_eq!(view.state, ComponentState::Failed);
    assert_eq!(view.reason.as_deref(), Some("binding_changed"));
    assert_eq!(
        retakes.load(Ordering::SeqCst),
        2,
        "first try uses the given snapshot"
    );
}

#[tokio::test]
async fn the_contract_gate_reads_the_probe_cache_and_reprobes_after_the_ttl() {
    use agent24_knowledge::host::ContractGate;
    let fake = serve(Script::Reply(200, ready_body())).await;
    let registry = with_credential(&fake.base, "LEAK-gate-secret");
    let url = fake.base.as_str();
    let t0 = Instant::now();
    assert_eq!(
        registry.contract_gate(url, "search", t0).await,
        ContractGate::Open
    );
    assert_eq!(
        registry.contract_gate(url, "read", t0).await,
        ContractGate::Open
    );
    assert_eq!(
        fake.hits.load(Ordering::SeqCst),
        1,
        "the second call is a cache hit"
    );
    // The components list shares that entry.
    let item = knowledge_item(&registry, t0).await;
    assert_eq!(item.state, ComponentState::Ready);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);
    let later = t0 + KNOWLEDGE_PROBE_TTL + Duration::from_secs(1);
    assert_eq!(
        registry.contract_gate(url, "search", later).await,
        ContractGate::Open
    );
    assert_eq!(
        fake.hits.load(Ordering::SeqCst),
        2,
        "an expired entry probes once"
    );
}

#[tokio::test]
async fn the_contract_gate_refuses_incompatible_missing_and_unknown() {
    use agent24_knowledge::host::ContractGate;
    let t0 = Instant::now();
    let incompatible =
        ready_body().replace("\"protocol_versions\":[1,2]", "\"protocol_versions\":[2]");
    let fake = serve(Script::Reply(200, incompatible)).await;
    let registry = configured(&fake.base);
    assert_eq!(
        registry.contract_gate(&fake.base, "search", t0).await,
        ContractGate::Incompatible
    );

    // Declaration lacks `read`: read is refused, search is not.
    let fake = serve(Script::Reply(200, document_with_ops(r#""search""#))).await;
    let registry = configured(&fake.base);
    assert_eq!(
        registry.contract_gate(&fake.base, "read", t0).await,
        ContractGate::Incompatible
    );
    assert_eq!(
        registry.contract_gate(&fake.base, "search", t0).await,
        ContractGate::Open
    );

    // No declaration yet: unreachable, or not a declaration at all.
    let fake = serve(Script::Reply(500, "x".into())).await;
    let registry = configured(&fake.base);
    assert_eq!(
        registry.contract_gate(&fake.base, "search", t0).await,
        ContractGate::NoDeclaration
    );
    // Nothing configured, or an endpoint that is not one of the bindings'.
    assert_eq!(
        KnowledgeHostRegistry::default()
            .contract_gate("http://unconfigured/api/v1", "search", t0)
            .await,
        ContractGate::NoDeclaration
    );
    assert_eq!(
        registry
            .contract_gate("http://other/api/v1", "search", t0)
            .await,
        ContractGate::NoDeclaration
    );

    // A service that answers but is not ready is still a compatible contract.
    let fake = serve(Script::Reply(200, ready_body())).await;
    fake.ready_mode(ReadyMode::SearchNotReady);
    let registry = with_credential(&fake.base, "LEAK-gate-2");
    assert_eq!(
        registry.contract_gate(&fake.base, "search", t0).await,
        ContractGate::Open
    );
}

/// H1: one binding's failed or incompatible endpoint must not refuse a call that
/// goes to another binding's good endpoint, and its own calls are still refused.
#[tokio::test]
async fn the_gate_looks_only_at_the_endpoint_the_call_goes_to() {
    use agent24_knowledge::host::ContractGate;
    let t0 = Instant::now();
    let good = serve(Script::Reply(200, ready_body())).await;
    let down = serve(Script::Reply(500, "x".into())).await;
    let incompatible = serve(Script::Reply(
        200,
        ready_body().replace("\"protocol_versions\":[1,2]", "\"protocol_versions\":[2]"),
    ))
    .await;
    let registry = KnowledgeHostRegistry::default();
    bind(&registry, "good", 1, &good.base);
    bind(&registry, "down", 1, &down.base);
    bind(&registry, "wrong", 1, &incompatible.base);
    // The folded view the list shows is the worst of the three.
    assert_eq!(
        knowledge_item(&registry, t0).await.state,
        ComponentState::Incompatible
    );
    for op in ["search", "read"] {
        assert_eq!(
            registry.contract_gate(&good.base, op, t0).await,
            ContractGate::Open,
            "{op}"
        );
        assert_eq!(
            registry.contract_gate(&down.base, op, t0).await,
            ContractGate::NoDeclaration,
            "{op}"
        );
        assert_eq!(
            registry.contract_gate(&incompatible.base, op, t0).await,
            ContractGate::Incompatible,
            "{op}"
        );
    }
    // One probe round served all of that.
    assert_eq!(good.hits.load(Ordering::SeqCst), 1);
}

/// M2: a failed reading closes the gate for a few seconds, not for a whole TTL.
#[tokio::test]
async fn a_failed_reading_is_reprobed_after_a_short_ttl_not_the_full_one() {
    use agent24_knowledge::host::ContractGate;
    use agent24_knowledge::probe::KNOWLEDGE_PROBE_FAILED_TTL;
    let up = Arc::new(AtomicBool::new(false));
    let fake = serve(Script::Switch(Arc::clone(&up), ready_body())).await;
    let registry = configured(&fake.base);
    let t0 = Instant::now();
    assert_eq!(
        registry.contract_gate(&fake.base, "search", t0).await,
        ContractGate::NoDeclaration
    );
    up.store(true, Ordering::SeqCst);
    let soon = t0 + KNOWLEDGE_PROBE_FAILED_TTL - Duration::from_secs(1);
    assert_eq!(
        registry.contract_gate(&fake.base, "search", soon).await,
        ContractGate::NoDeclaration
    );
    assert_eq!(
        fake.hits.load(Ordering::SeqCst),
        1,
        "inside the short window: cached"
    );
    let after = t0 + KNOWLEDGE_PROBE_FAILED_TTL + Duration::from_secs(1);
    assert!(after < t0 + KNOWLEDGE_PROBE_TTL);
    assert_eq!(
        registry.contract_gate(&fake.base, "search", after).await,
        ContractGate::Open
    );
    assert_eq!(
        fake.hits.load(Ordering::SeqCst),
        2,
        "reprobed, and it recovered"
    );
    // The list is unchanged by this: it keeps the full TTL for a failure.
    let down = serve(Script::Reply(500, "x".into())).await;
    let registry = configured(&down.base);
    knowledge_item(&registry, t0).await;
    knowledge_item(&registry, after).await;
    assert_eq!(down.hits.load(Ordering::SeqCst), 1);
}

// ---- L3: D9 expected-vs-actual for Documents (ADR-038 C6) ----

fn embedded_tools() -> Vec<String> {
    DOC_TOOLS.iter().map(|t| (*t).to_owned()).collect()
}

fn report_with(
    outcome: MountOutcome,
    version: &str,
    provides: Vec<crate::component_manifest::CapProvide>,
) -> crate::domain::MountReport {
    let mut report = mount_report_at(outcome, version);
    report.d9 = Some(crate::component_manifest::OsD9 {
        name: "documents".into(),
        in_process: false,
        provides,
        requires_kernel: vec!["events".into(), "models".into()],
    });
    report
}

fn declared(id: &str, tools: Vec<String>) -> crate::component_manifest::CapProvide {
    crate::component_manifest::CapProvide {
        id: id.into(),
        tools,
    }
}

fn running(report: &crate::domain::MountReport) -> Item {
    documents_from(
        Some(report),
        Some(&Supervised::Running),
        documents_catalog::DocumentsCatalog::embedded(),
    )
}

/// `version` is what runs; the catalog's goes to `detail.expected_version`.
#[test]
fn documents_version_is_the_runtime_and_the_catalog_is_the_expectation() {
    let ok = running(&mount_report_at(MountOutcome::Mounted, "0.2.0"));
    assert_eq!(ok.state, ComponentState::Ready);
    assert_eq!(ok.version.as_deref(), Some("0.2.0"));
    assert!(ok.reason.is_none());
    assert_eq!(ok.detail.as_ref().unwrap()["expected_version"], "0.2.0");

    let drifted = running(&mount_report_at(MountOutcome::Mounted, "9.9.9"));
    // A different version is not a contract mismatch (M4 of the L3 review): a
    // package fetched on demand can be ahead of or behind this daemon.
    assert_eq!(drifted.state, ComponentState::Ready);
    assert!(drifted.reason.is_none());
    assert_eq!(
        drifted.version.as_deref(),
        Some("9.9.9"),
        "the runtime's, not the catalog's"
    );
    let detail = drifted.detail.as_ref().unwrap();
    assert_eq!(detail["expected_version"], "0.2.0");
    assert_eq!(detail["actual_version"], "9.9.9");
    assert_eq!(detail["update_available"], true);
    assert!(detail.get("expected").is_none() && detail.get("kind").is_none());
    assert!(
        ok.detail
            .as_ref()
            .unwrap()
            .get("update_available")
            .is_none()
    );
    assert!(drifted.form.allows(drifted.state));

    // No report: nothing is running to compare. The catalog's version is shown
    // and the state stays `absent`.
    let none = documents_from(None, None, documents_catalog::DocumentsCatalog::embedded());
    assert_eq!(none.state, ComponentState::Absent);
    assert_eq!(none.version.as_deref(), Some("0.2.0"));
    assert_eq!(none.detail.as_ref().unwrap()["expected_version"], "0.2.0");
}

/// The tool mismatch overrides a live `ready` or `degraded` only (L4): a package
/// that is off, never mounted, starting or restarting keeps the state that says
/// what it is doing.
#[test]
fn a_documents_tool_mismatch_overrides_only_a_live_reading() {
    let cases: Vec<(MountOutcome, Option<Supervised>, ComponentState)> = vec![
        (MountOutcome::Disabled, None, ComponentState::Stopped),
        (
            MountOutcome::Degraded("disk".into()),
            None,
            ComponentState::Failed,
        ),
        (
            MountOutcome::Refused("dup".into()),
            None,
            ComponentState::Failed,
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Starting {
                attempt: 1,
                after: None,
            }),
            ComponentState::Starting,
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Stopping),
            ComponentState::Stopping,
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Stopped),
            ComponentState::Stopped,
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Panicked),
            ComponentState::Failed,
        ),
        (
            MountOutcome::Mounted,
            Some(Supervised::Running),
            ComponentState::Incompatible,
        ),
        (MountOutcome::Mounted, None, ComponentState::Incompatible),
    ];
    for (outcome, live, want) in cases {
        let report = report_with(
            outcome.clone(),
            "0.2.0",
            vec![declared("documents", vec![])],
        );
        let item = documents_from(
            Some(&report),
            live.as_ref(),
            documents_catalog::DocumentsCatalog::embedded(),
        );
        assert_eq!(item.state, want, "{outcome:?} {live:?}");
        let flagged = item.reason.as_deref() == Some("component_contract_mismatch");
        assert_eq!(
            flagged,
            want == ComponentState::Incompatible,
            "{outcome:?} {live:?}"
        );
    }
}

/// A package that is off, or never mounted, keeps its state under a version drift.
#[test]
fn a_documents_version_drift_does_not_override_disabled_degraded_or_refused() {
    for (outcome, state, reason) in [
        (MountOutcome::Disabled, ComponentState::Stopped, "disabled"),
        (
            MountOutcome::Degraded("disk".into()),
            ComponentState::Failed,
            "degraded",
        ),
        (
            MountOutcome::Refused("dup".into()),
            ComponentState::Failed,
            "refused",
        ),
    ] {
        let item = running(&mount_report_at(outcome.clone(), "9.9.9"));
        assert_eq!(item.state, state, "{outcome:?}");
        assert_eq!(item.reason.as_deref(), Some(reason), "{outcome:?}");
        assert_eq!(item.version.as_deref(), Some("9.9.9"));
        assert_eq!(item.detail.as_ref().unwrap()["update_available"], true);
    }
}

#[test]
fn a_capability_the_manifest_omits_is_unavailable_and_extras_are_ignored() {
    // The manifest lists another capability only: ours is `unavailable`, the
    // state does not change (D9: actual is a subset of expected).
    let report = report_with(
        MountOutcome::Mounted,
        "0.2.0",
        vec![declared("other-cap", vec!["documents.list".into()])],
    );
    let item = running(&report);
    assert_eq!(item.state, ComponentState::Ready);
    assert!(item.reason.is_none());
    assert_eq!(item.provides[0].availability, Some("unavailable"));
    assert_eq!(
        serde_json::to_value(&item.provides[0]).unwrap()["availability"],
        "unavailable"
    );

    // Everything expected is declared, plus an extra: nothing is flagged and
    // the extra is not shown.
    let report = report_with(
        MountOutcome::Mounted,
        "0.2.0",
        vec![
            declared("documents", embedded_tools()),
            declared("extra-cap", vec![]),
        ],
    );
    let item = running(&report);
    assert_eq!(item.state, ComponentState::Ready);
    assert_eq!(item.provides.len(), 1);
    assert_eq!(item.provides[0].availability, None);
    assert!(
        serde_json::to_value(&item.provides[0])
            .unwrap()
            .get("availability")
            .is_none()
    );

    // A package that declares no capabilities says nothing either way.
    let item = running(&report_with(MountOutcome::Mounted, "0.2.0", vec![]));
    assert_eq!(item.provides[0].availability, None);
    assert_eq!(item.state, ComponentState::Ready);
}

#[test]
fn a_declared_capability_without_an_expected_tool_is_a_contract_mismatch() {
    let mut tools = embedded_tools();
    tools.retain(|t| t != "documents.find");
    let report = report_with(
        MountOutcome::Mounted,
        "0.2.0",
        vec![declared("documents", tools)],
    );
    let item = running(&report);
    assert_eq!(item.state, ComponentState::Incompatible);
    assert_eq!(item.reason.as_deref(), Some("component_contract_mismatch"));
    let detail = item.detail.as_ref().unwrap();
    assert_eq!(detail["kind"], "tools");
    assert_eq!(detail["capability"], "documents");
    assert_eq!(detail["missing"], json!(["documents.find"]));
    assert_eq!(
        item.provides[0].availability, None,
        "the capability is there, its contract is not"
    );

    // A drifted version alongside it is only added to the detail.
    let report = report_with(
        MountOutcome::Mounted,
        "9.9.9",
        vec![declared("documents", vec![])],
    );
    let item = running(&report);
    let detail = item.detail.as_ref().unwrap();
    assert_eq!(item.state, ComponentState::Incompatible);
    assert_eq!(detail["kind"], "tools");
    assert_eq!(detail["actual_version"], "9.9.9");
    assert_eq!(detail["update_available"], true);
}

/// The package we ship agrees with the catalog compiled into the daemon:
/// same version, and nothing flagged against its own manifest.
#[test]
fn the_shipped_documents_manifest_matches_the_catalog() {
    let manifest = agent24_domain::DomainOsManifest::from_yaml(include_str!(
        "../../../agent24-documents/domain-os.yml"
    ))
    .unwrap();
    let d9 = crate::component_manifest::OsD9::from_manifest(&manifest);
    let catalog = documents_catalog::DocumentsCatalog::embedded().unwrap();
    let checked = catalog.check(Some(manifest.version()), Some(&d9.provides));
    assert_eq!(checked.mismatch, None);
    assert!(checked.unavailable.is_empty());
}

#[tokio::test]
async fn documents_expected_vs_actual_reaches_the_list_and_hides_detail_from_others() {
    let (mut state, host, _) = server_tests::capability_state().await;
    let first_party = state
        .first_party
        .as_ref()
        .unwrap()
        .issue(&test_peer("daemon-test"))
        .unwrap()
        .secret
        .to_string();
    state.os_reports = Arc::new(vec![report_with(
        MountOutcome::Mounted,
        "9.9.9",
        vec![declared("other-cap", vec![])],
    )]);
    let app = build_router(state);
    let (status, full) = call(&app, Method::GET, GET, Some(&host), "").await;
    assert_eq!(status, StatusCode::OK);
    let doc = find(&full, "documents");
    assert_eq!(doc["state"], "ready");
    assert!(doc.get("reason").is_none());
    assert_eq!(doc["version"], "9.9.9");
    assert_eq!(doc["detail"]["expected_version"], "0.2.0");
    assert_eq!(doc["detail"]["update_available"], true);
    assert_eq!(doc["provides"][0]["availability"], "unavailable");

    let (_, redacted) = call(&app, Method::GET, GET, Some(&first_party), "").await;
    let doc = find(&redacted, "documents");
    assert_eq!(doc["state"], "ready");
    assert_eq!(doc["version"], "9.9.9");
    let text = doc.to_string();
    for leaked in [
        "expected_version",
        "actual_version",
        "update_available",
        "detail",
        "reason",
    ] {
        assert!(!text.contains(leaked), "{leaked} in {text}");
    }
}

/// The scanner that reads `dispatch`'s arms: the shapes the old line matcher
/// missed.
#[test]
fn the_match_op_scanner_handles_wrapped_arrows_alternatives_and_nested_matches() {
    let src = r#"
        fn d(op: &str) {
            match op {
                // "commented" => 1,
                "a" => x(),
                "b"
                    => y(),
                "c" | "d" => {
                    match inner { "nested" => 1, _ => 2 }
                    z("not-an-arm")
                }
                "e" /* c */ => w('"'),
                _ => v(),
            }
            match other { "outside" => 1, _ => 2 }
        }
    "#;
    assert_eq!(
        sorted_tools(match_op_arms(src)),
        sorted_tools([
            "documents.a",
            "documents.b",
            "documents.c",
            "documents.d",
            "documents.e"
        ])
    );
}

// ---- L3 review round: L1, L2 ----

/// L1: 2^53 itself is not a safe integer for a float; the bound is strict.
#[tokio::test]
async fn a_float_at_two_to_the_53_is_not_read_as_an_integer() {
    for (value, ready) in [
        ("9007199254740991.0", true),
        ("9007199254740992.0", false),
        ("1e16", false),
    ] {
        let fake = serve(Script::Reply(
            200,
            document_with_limits(&limits_with(value)),
        ))
        .await;
        let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
        let host = shown(&item);
        if ready {
            assert_eq!(host["state"], "ready", "{value}");
        } else {
            assert_eq!(host["reason"], "malformed", "{value}");
        }
    }
}

/// L2: the key moves while the §8 request is out. Its answer must not be used.
#[tokio::test]
async fn a_credential_that_moves_during_the_ready_request_makes_the_answer_stale() {
    use agent24_knowledge::probe::KnowledgeProbeCache;
    let fake = serve(Script::Reply(200, ready_body())).await;
    let registry = with_credential(&fake.base, "LEAK-during-secret");
    let targets = registry.probe_targets(unix_now());
    let cache = KnowledgeProbeCache::default();
    // Valid before the capabilities request and before §8; revoked after it.
    let calls = AtomicUsize::new(0);
    let current =
        |_: &agent24_knowledge::probe::ProbeTarget| calls.fetch_add(1, Ordering::SeqCst) < 2;
    let assessed = cache.assess(&targets, Instant::now(), &current).await;
    assert_eq!(fake.ready_hits(), 1, "§8 went out");
    assert!(assessed.stale, "its answer was used although the key moved");
}

// ---- #1579 review: F2 ----

/// `1e-400` underflows to `0.0` as a float, but its text is not zero: it is a
/// fraction, not the integer 0, so it is not success. A zero mantissa still is.
#[tokio::test]
async fn an_envelope_code_that_underflows_to_zero_is_not_success() {
    for code in ["1e-400", "-1e-400", "0.000001e-400", "5E-999"] {
        let fake = serve(Script::Reply(200, with_code(code))).await;
        let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
        let host = shown(&item);
        assert_eq!(host["state"], "failed", "code {code}");
        assert_eq!(host["reason"], "upstream_error", "code {code}");
        assert_no_canary(&item, &["LEAK-union"]);
    }
    for code in ["0e-400", "0.0e-400", "-0.0E999"] {
        let fake = serve(Script::Reply(200, with_code(code))).await;
        let item = knowledge_item(&configured(&fake.base), Instant::now()).await;
        assert_eq!(shown(&item)["state"], "ready", "code {code}");
    }
}

#[test]
fn s1_4_idoris_contract_mismatch_is_incompatible() {
    use agent24_idoris::idoris_host::Code;
    assert_eq!(
        from_idoris(IdorisState::Failed, false, Some(Code::ContractMismatch)),
        (
            ComponentState::Incompatible,
            Some("component_contract_mismatch")
        )
    );
    assert_eq!(
        from_idoris(IdorisState::Failed, true, Some(Code::ContractMismatch)).0,
        ComponentState::Installing
    );
    assert_eq!(
        from_idoris(IdorisState::Stopped, false, Some(Code::ContractMismatch)).0,
        ComponentState::Stopped
    );
}

#[test]
fn s1_4_components_render_hyphae_lifecycle() {
    for (process, expected, reason) in [
        ("starting", "starting", None),
        ("locked", "stopped", Some("locked")),
        ("backoff", "failed", Some("backoff")),
    ] {
        let (state, why) = from_hyphae(process);
        let item = Item::new("hyphae", Form::Resident, state).with(|i| i.reason = why);
        let payload = render(&[item], true);
        assert_eq!(payload["components"][0]["state"], expected, "{process}");
        assert_eq!(
            payload["components"][0]["reason"],
            json!(reason),
            "{process}"
        );
    }
}

fn unsealed_package(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
             spawn:\n  command: bin/mod\n"
        ),
    )
    .unwrap();
}

/// T6. A fresh scan refuses an old package that has no integrity section and
/// keeps a sealed sibling. Restoring the optional "no integrity means pass"
/// path puts `oldmod` in `found` and this test goes red.
#[test]
fn t6_old_package_is_failed_and_a_sealed_sibling_stays_available() {
    let root = tempfile::tempdir().unwrap();
    unsealed_package(root.path(), "oldmod");
    let good = root.path().join("goodmod");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::write(
        good.join("domain-os.yml"),
        "name: goodmod\nversion: \"0.1.0\"\nroute_namespace: /api/v1/goodmod\n\
         event_module: goodmod\ndata_dir: ~/.agent24/os/goodmod/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: bin/mod\n",
    )
    .unwrap();
    agent24_os_packages::test_support::seal_exit_stub(&good, "bin/mod").unwrap();
    let scan = agent24_os_packages::test_support::scan(root.path(), &CancellationToken::new());
    assert_eq!(
        scan.found
            .iter()
            .map(|d| d.manifest.name())
            .collect::<Vec<_>>(),
        vec!["goodmod"]
    );
    let old = scan.refused.iter().find(|r| r.dir.ends_with("oldmod"));
    assert!(old.is_some(), "old package was accepted");
    let old = old.unwrap();
    assert_eq!(
        old.integrity,
        Some(agent24_components::integrity::IntegrityReason::ManifestMissing)
    );
    let reports = crate::domain::integrity_refusal_reports(&scan.refused, &[]);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].name, "oldmod");
    let (state, reason, _) = os_mapped(&reports[0].outcome, None);
    assert_eq!(state, ComponentState::Failed);
    assert_eq!(reason, Some("integrity_manifest_missing"));
    let item = Item::new("oldmod", Form::Resident, state).with(|i| {
        i.version = Some("0.1.0".to_owned());
        i.reason = reason.map(str::to_owned);
        i.provides.clear();
    });
    let full = render(std::slice::from_ref(&item), true);
    assert_eq!(full["components"][0]["state"], "failed");
    assert_eq!(
        full["components"][0]["reason"],
        "integrity_manifest_missing"
    );
    let redacted = render(&[item], false);
    assert!(redacted["components"][0].get("reason").is_none());
    assert!(redacted["components"][0].get("detail").is_none());

    agent24_os_packages::test_support::seal_exit_stub(&root.path().join("oldmod"), "bin/mod")
        .unwrap();
    let again = agent24_os_packages::test_support::scan(root.path(), &CancellationToken::new());
    assert!(
        again.refused.iter().all(|r| !r.dir.ends_with("oldmod")),
        "reinstall left the refusal: {:?}",
        again.refused
    );
    assert!(again.found.iter().any(|d| d.manifest.name() == "oldmod"));
}

/// A manifest too damaged to trust, or one that forges a builtin name, does
/// not take that name and does not publish its provides.
#[test]
fn t6_a_forged_or_unreadable_name_uses_a_directory_identity() {
    let root = tempfile::tempdir().unwrap();
    let forged = root.path().join("sneaky");
    std::fs::create_dir_all(&forged).unwrap();
    std::fs::write(
        forged.join("domain-os.yml"),
        "name: idoris\nversion: \"9.9.9\"\nroute_namespace: /api/v1/idoris\n\
         event_module: idoris\ndata_dir: ~/.agent24/os/idoris/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: bin/mod\nprovides:\n  - id: model-gateway\n    tools: []\n",
    )
    .unwrap();
    let broken = root.path().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("domain-os.yml"), "name: [").unwrap();
    let scan = agent24_os_packages::discovery::scan(root.path(), &CancellationToken::new());
    let reports = crate::domain::integrity_refusal_reports(&scan.refused, &[]);
    let names: Vec<&str> = reports.iter().map(|r| r.name.as_str()).collect();
    assert!(names.iter().all(|n| *n != "idoris"), "{names:?}");
    assert!(reports.iter().all(|r| r.d9.is_none()));
    assert!(
        reports
            .iter()
            .all(|r| r.version.is_empty() || r.name != "idoris")
    );
    assert!(names.iter().any(|n| n.starts_with("dir-")));
}

fn package_yaml(name: &str, version: &str) -> String {
    format!(
        "name: {name}\nversion: \"{version}\"\nroute_namespace: /api/v1/{name}\n\
         event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: bin/{name}\n"
    )
}

/// The Documents package the app installs is refused on the `documents` row.
/// Pre-claiming `documents` in `taken` renames it to `dir-*` and this row
/// stays `absent`. A forged `name: documents` in another directory, and a
/// forged builtin name, still use `dir-*`.
#[tokio::test]
async fn a_refused_documents_package_is_failed_on_the_documents_row() {
    let root = tempfile::tempdir().unwrap();
    let documents = root.path().join("documents");
    std::fs::create_dir_all(&documents).unwrap();
    std::fs::write(
        documents.join("domain-os.yml"),
        package_yaml("documents", "1.2.3"),
    )
    .unwrap();
    let forged_documents = root.path().join("aaa");
    std::fs::create_dir_all(&forged_documents).unwrap();
    std::fs::write(
        forged_documents.join("domain-os.yml"),
        package_yaml("documents", "9.9.9"),
    )
    .unwrap();
    let forged_builtin = root.path().join("sneaky");
    std::fs::create_dir_all(&forged_builtin).unwrap();
    std::fs::write(
        forged_builtin.join("domain-os.yml"),
        package_yaml("idoris", "9.9.9"),
    )
    .unwrap();

    let scan = agent24_os_packages::discovery::scan(root.path(), &CancellationToken::new());
    let reports = crate::domain::integrity_refusal_reports(&scan.refused, &[]);
    let mut state = server_tests::state().await;
    state.os_reports = Arc::new(reports);
    let body = render(&collect(&state, std::time::Instant::now()).await, true);
    let rows = body["components"].as_array().unwrap();
    let documents_rows: Vec<_> = rows.iter().filter(|row| row["id"] == "documents").collect();
    assert_eq!(documents_rows.len(), 1);
    assert_eq!(documents_rows[0]["state"], "failed");
    assert_eq!(documents_rows[0]["reason"], "integrity_manifest_missing");
    assert_eq!(documents_rows[0]["version"], "1.2.3");
    assert!(
        rows.iter()
            .all(|row| row["id"] != "idoris" || row["reason"] != "integrity_manifest_missing"),
        "{body}"
    );
    assert!(
        rows.iter()
            .any(|row| row["id"].as_str().is_some_and(|id| id.starts_with("dir-"))),
        "{body}"
    );
}

/// T8. Each integrity reason is `failed` for the host and absent from the
/// redacted row. Dropping `reason`, or copying `detail` into the redacted
/// view, turns this red.
#[test]
fn t8_each_integrity_reason_is_host_only() {
    use crate::domain::RefusalReason;
    use agent24_components::integrity::IntegrityReason;
    for reason in [
        IntegrityReason::ManifestMissing,
        IntegrityReason::HashMismatch,
        IntegrityReason::ManifestInvalid,
        IntegrityReason::SignatureRejected,
        IntegrityReason::CheckFailed,
    ] {
        let outcome = MountOutcome::RefusedFor(
            RefusalReason::PackageIntegrity(reason),
            format!("detail /tmp/secret stderr {}", reason.code()),
        );
        let (state, code, message) = os_mapped(&outcome, None);
        assert_eq!(state, ComponentState::Failed);
        assert_eq!(code, Some(reason.code()));
        let item = Item::new("pkg", Form::Resident, state).with(|i| {
            i.version = Some("1".to_owned());
            i.reason = code.map(str::to_owned);
            i.detail = message.map(|text| json!({"message": text}));
        });
        let full = render(std::slice::from_ref(&item), true);
        assert_eq!(full["components"][0]["state"], "failed");
        assert_eq!(full["components"][0]["reason"], reason.code());
        assert_ne!(full["components"][0]["state"], "absent");
        assert_ne!(full["components"][0]["state"], "incompatible");
        let redacted = render(&[item], false).to_string();
        assert!(!redacted.contains("reason"), "{redacted}");
        assert!(!redacted.contains("detail"), "{redacted}");
        assert!(!redacted.contains("/tmp"), "{redacted}");
        assert!(!redacted.contains("stderr"), "{redacted}");
    }
}
