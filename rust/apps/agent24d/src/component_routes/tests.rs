#![allow(clippy::unwrap_used)]
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

// ---- Knowledge capabilities probe (ADR-038 C5) -----------------------------------------

use crate::capabilities::unix_now;
use crate::knowledge_host::{Change, Credential, Endpoint, KnowledgeHostRegistry};
use crate::knowledge_probe::{KNOWLEDGE_PROBE_TIMEOUT, KNOWLEDGE_PROBE_TTL};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

fn ready_body() -> String {
    r#"{"code":0,"msg":"LEAK-msg-9f3a","data":{"edition":"lite","x_unknown":1,"integration":{"other":{},"agent24_knowledge":{"protocol_versions":[1,2],"mode":"integration","supported_operations":["search","read","ingest"],"server_version":"0.8.2","future_field":true}}}}"#.to_owned()
}

#[derive(Clone)]
enum Script {
    Reply(u16, String),
    /// 500 if the request carries a credential header, else `body`.
    NoAuth(String),
    Stall,
    Hold(Arc<AtomicBool>, String),
    Bearer {
        secret: String,
        ok: String,
        anon: String,
    },
}

struct Fake {
    base: String,
    hits: Arc<AtomicUsize>,
}

async fn serve(script: Script) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let script = script.clone();
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                let _ = answer(stream, script, &counter).await;
            });
        }
    });
    Fake {
        base: format!("http://{addr}/api/v1"),
        hits,
    }
}

async fn answer(mut stream: TcpStream, script: Script, hits: &AtomicUsize) -> std::io::Result<()> {
    let head = match read_head(&mut stream).await {
        Some(head) => head,
        None => return Ok(()),
    };
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
    }
    Ok(())
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut data = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        if data.windows(4).any(|w| w == b"\r\n\r\n") {
            return String::from_utf8(data).ok();
        }
        if data.len() > 16 * 1024 {
            return None;
        }
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
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
    let body = r#"{"code":0,"msg":"LEAK-degraded-msg","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search","ingest"],"server_version":"1.2.3"}}}}"#;
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
    let body = r#"{"data":{"integration":{"agent24_knowledge":{"protocol_versions":[2,3],"mode":"integration","supported_operations":["search","read"],"server_version":"9.9.9"}}}}"#;
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
    let body = r#"{"code":0,"msg":"LEAK-mode-msg","data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"standalone","supported_operations":["search","read"],"server_version":"1.0 LEAK-ver-77"}}}}"#;
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
    let anon = r#"{"data":{"integration":{"agent24_knowledge":{"protocol_versions":[1],"mode":"integration","supported_operations":["search"],"server_version":"0.1.0"}}}}"#;
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
