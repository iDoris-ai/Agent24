#![allow(clippy::unwrap_used)]
use super::*;
use crate::capabilities::first_party::test_peer;
use crate::enroll_api::EnrollAuthority;
use crate::server::{build_router, tests as server_tests};
use axum::http::{Method, header};
use http_body_util::BodyExt;
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
    .into_iter()
    .map(|n| crate::attach_registry::AttachSnap {
        name: n.to_owned(),
        version: String::new(),
        disabled: false,
        drain: Some(DrainState::Running),
        mismatch: None,
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
    fn apply(&mut self, outcome: crate::attached::RegisterOutcome) {
        let response = match outcome {
            crate::attached::RegisterOutcome::Created(response)
            | crate::attached::RegisterOutcome::Rotated { response, .. } => response,
        };
        self.token = response.token;
        self.digest = response.manifest_digest;
    }

    fn rotate(&mut self) {
        let registry = std::sync::Arc::clone(&self.state.attach_registry);
        let outcome = crate::attached::register(
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
        let found = crate::attached::set_disabled(&self.path, "agentear", disabled, |change| {
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
    let outcome = crate::attached::register(
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
        crate::attach_listener::handle_connection(kernel, &registry, CancellationToken::new())
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
fn dispatched_tool_names() -> Vec<String> {
    let src = include_str!("../../../agent24-documents/src/tools.rs");
    let start = src.find("match op {").unwrap();
    let rest = &src[start..];
    let end = rest.find("_ =>").unwrap();
    let mut names = Vec::new();
    for line in rest[..end].lines() {
        let line = line.trim();
        let Some(line) = line.strip_prefix('"') else {
            continue;
        };
        let Some((name, tail)) = line.split_once('"') else {
            continue;
        };
        if tail.trim_start().starts_with("=>") {
            names.push(format!("documents.{name}"));
        }
    }
    sorted_tools(names)
}

fn mount_report(outcome: MountOutcome) -> crate::domain::MountReport {
    crate::domain::MountReport {
        name: "documents".into(),
        namespace: "/api/v1/documents".into(),
        version: "9.9.9".into(),
        enabled_at_start: Some(true),
        outcome,
        granted: Vec::new(),
        resources: crate::domain::ResourceStatus::NotChecked,
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
    assert_eq!(catalog.version, "0.1.0");
    assert_eq!(catalog.name.en, "Documents");
    assert_eq!(catalog.name.zh, "文档");
    assert!(!catalog.name.th.is_empty());
    assert!(catalog.assurance.is_empty());
    assert_eq!(catalog.requires.kernel, ["events", "models"]);
    assert!(catalog.requires.operations.is_empty());
    let manifest = include_str!("../../../agent24-documents/domain-os.yml");
    assert!(
        manifest.contains("version: \"0.1.0\""),
        "catalog version drifts from domain-os.yml"
    );
    assert!(manifest.contains("kernel_capabilities: [events, models]"));
    assert_eq!(sorted_tools(catalog.tool_names()), DOC_TOOLS);
    assert_eq!(dispatched_tool_names(), DOC_TOOLS);
    let item = documents_from(None, None, Ok(catalog));
    assert_eq!(item.form, Form::Resident);
    assert_eq!(item.state, ComponentState::Absent);
    assert_eq!(item.version.as_deref(), Some("0.1.0"));
    assert!(item.reason.is_none());
    assert_eq!(item.provides.len(), 1);
    assert_eq!(item.provides[0].id, "documents");
    assert_eq!(sorted_tools(item.provides[0].tools.iter()), DOC_TOOLS);
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
            v["requires"]["operations"] = json!(["Bad"])
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
    let report = mount_report(MountOutcome::Mounted);
    let item = documents_from(
        Some(&report),
        Some(&Supervised::Running),
        documents_catalog::DocumentsCatalog::embedded(),
    );
    assert_eq!(item.state, ComponentState::Ready);
    assert_eq!(item.version.as_deref(), Some("0.1.0"));
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
    assert_eq!(doc["version"], "0.1.0");
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
