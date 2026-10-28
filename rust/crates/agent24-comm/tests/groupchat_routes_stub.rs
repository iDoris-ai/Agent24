//! COMM-G1: HTTP status mapping and argv of the `/comm/groupchat/*` routes,
//! driven through the real router over a STUB `hyphae` (a shell script that
//! records its argv and replays a canned envelope + exit code). Needs no
//! Hyphae release binary; `groupchat_real_binary.rs` covers the real one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    Account, CommState, HyphaeRunner, MemoryPasswordStore, PasswordStore, VerifiedBinary, router,
};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const G: &str = "0123456789abcdef0123456789abcdef";
const L: &str = "fedcba9876543210fedcba9876543210";
const SALT: &str = "c3R1Yi1zYWx0LXN0dWItc2FsdA==";

/// Builds the router over a stub that prints `stdout` / `stderr` and exits
/// `code`. The stub's argv lands in `$HOME/argv` (one arg per line).
async fn stub_app(code: i32, stdout: &str, stderr: &str) -> (Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    tokio::fs::create_dir_all(home.join(".hyphae"))
        .await
        .unwrap();
    tokio::fs::write(
        home.join(".hyphae/keystore.json"),
        json!({"salt": SALT}).to_string(),
    )
    .await
    .unwrap();
    let script = tmp.path().join("fake-hyphae");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/argv\"\ncat >/dev/null\n\
         printf '%s' '{stdout}'\nprintf '%s' '{stderr}' >&2\nexit {code}\n"
    );
    tokio::fs::write(&script, &body).await.unwrap();
    let digest = agent24_comm::binary::sha256_of(body.as_bytes());
    let bin = VerifiedBinary::install(&script, digest, &tmp.path().join("bin"))
        .await
        .unwrap();
    let store = Arc::new(MemoryPasswordStore::new());
    let password = agent24_comm::Password::new(b"stub-password-0123456789".to_vec()).unwrap();
    store
        .put(&Account::from_salt(SALT), &password)
        .await
        .unwrap();
    let runner = Arc::new(HyphaeRunner::new(
        bin,
        home.clone(),
        Duration::from_secs(10),
    ));
    (router(CommState::ready(runner, store, home)), tmp)
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn argv(tmp: &tempfile::TempDir) -> Vec<String> {
    std::fs::read_to_string(tmp.path().join("home/argv"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn failure(error: &str, message: &str, data: Option<Value>) -> String {
    let mut v = json!({"ok": false, "error": error, "message": message});
    if let Some(d) = data {
        v["data"] = d;
    }
    v.to_string()
}

#[tokio::test]
async fn exit_one_user_error_is_400_invalid() {
    let (app, _t) = stub_app(
        1,
        "",
        &failure(
            "user_error",
            "history cursor is not a message of this group",
            None,
        ),
    )
    .await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/groupchat/{G}/messages?before_id=nope"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid");
    assert_eq!(body["ok"], false);
}

#[tokio::test]
async fn partial_fanout_exit_four_keeps_the_report_as_502_partial() {
    let report = json!({"group_id": G, "logical_id": L, "state": "failed", "accepted": 0,
        "queued": 1, "failed": 1, "recipients": [{"recipient_npub": "n1", "state": "queued"},
        {"recipient_npub": "n2", "state": "failed"}]});
    let (app, t) = stub_app(
        4,
        "",
        &failure(
            "other_error",
            "partial fanout queued 1 failed 1",
            Some(report.clone()),
        ),
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/groupchat/{G}/messages"),
        Some(json!({"body": "half", "logical_id": L, "max_retries": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"], "partial");
    assert_eq!(body["data"], report);
    assert_eq!(
        argv(&t),
        [
            "groupchat",
            "send",
            "--group-id",
            G,
            "--body",
            "half",
            "--logical-id",
            L,
            "--max-retries",
            "1",
            "--password-stdin"
        ]
    );
}

#[tokio::test]
async fn exit_four_without_data_is_502_upstream() {
    // v0.27.2: storage/protocol errors stay `other_error` / exit 4.
    let (app, _t) = stub_app(
        4,
        "",
        &failure("other_error", "sqlite: disk I/O error", None),
    )
    .await;
    let (status, body) = call(&app, "GET", &format!("/groupchat/{G}/members"), None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"], "upstream");
}

/// Hyphae v0.27.2 (Hyphae#295) moved caller-input errors (group not active,
/// unknown logical id, invite/identity not found, ...) from `other_error`
/// (exit 4) to `user_error` (exit 1): they now surface as 400 `invalid`.
#[tokio::test]
async fn exit_one_user_error_group_not_active_is_400_invalid() {
    let (app, _t) = stub_app(1, "", &failure("user_error", "group is not active", None)).await;
    let (status, body) = call(&app, "GET", &format!("/groupchat/{G}/members"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid");
}

#[tokio::test]
async fn auth_and_network_exits_map_to_423_and_502() {
    let (app, _t) = stub_app(3, "", &failure("auth_error", "wrong password", None)).await;
    let (status, body) = call(&app, "GET", "/groupchat", None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::LOCKED, Some("locked"))
    );
    let (app, _t) = app_net().await;
    let (status, body) = call(&app, "GET", "/groupchat", None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("network"))
    );
}

async fn app_net() -> (Router, tempfile::TempDir) {
    stub_app(2, "", &failure("network_error", "relay down", None)).await
}

#[tokio::test]
async fn accept_pending_is_a_success_and_nil_history_becomes_an_array() {
    let (app, t) = stub_app(
        0,
        r#"{"ok":true,"data":{"group_id":"g","state":"pending"}}"#,
        "",
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/groupchat/invites/{G}/accept"),
        Some(json!({"identity": "bob"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["state"], "pending");
    assert_eq!(
        argv(&t),
        [
            "groupchat",
            "accept",
            "--invite",
            G,
            "--identity",
            "bob",
            "--password-stdin"
        ]
    );

    let (app, _t) = stub_app(0, r#"{"ok":true,"data":{"messages":null}}"#, "").await;
    let (status, body) = call(&app, "GET", &format!("/groupchat/{G}/messages"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["messages"], json!([]));
}

#[tokio::test]
async fn bad_input_is_rejected_before_hyphae_runs() {
    let (app, t) = stub_app(0, r#"{"ok":true,"data":{}}"#, "").await;
    let cases: &[(&str, String, Option<Value>)] = &[
        ("GET", "/groupchat/not-hex/members".into(), None),
        ("GET", "/groupchat/not-hex/invites".into(), None),
        ("GET", "/groupchat/not-hex/messages".into(), None),
        (
            "POST",
            "/groupchat/not-hex/messages".into(),
            Some(json!({"body": "x"})),
        ),
        ("GET", format!("/groupchat/{G}/messages/not-hex"), None),
        (
            "POST",
            format!("/groupchat/not-hex/messages/{L}/retry"),
            Some(json!({})),
        ),
        (
            "POST",
            "/groupchat/invites/not-hex/accept".into(),
            Some(json!({})),
        ),
        ("GET", format!("/groupchat/{G}/messages?limit=0"), None),
        (
            "GET",
            format!("/groupchat/{G}/messages?before_id=a&after_id=b"),
            None,
        ),
        ("GET", format!("/groupchat/{G}/messages?bogus=1"), None),
        (
            "POST",
            format!("/groupchat/{G}/messages"),
            Some(json!({"body": "  "})),
        ),
        (
            "POST",
            format!("/groupchat/{G}/messages"),
            Some(json!({"body": "x", "logical_id": "short"})),
        ),
        (
            "POST",
            "/groupchat".into(),
            Some(json!({"name": "g", "invitees": ["npub1bogus"]})),
        ),
        (
            "POST",
            format!("/groupchat/{G}/messages/{L}/retry"),
            Some(json!({"recipient": "xx"})),
        ),
        (
            "POST",
            format!("/groupchat/{G}/messages"),
            Some(json!({"body": "x", "nope": 1})),
        ),
    ];
    for (method, uri, body) in cases {
        let (status, resp) = call(&app, method, uri, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {resp}");
        assert_eq!(resp["error"], "invalid", "{method} {uri}");
    }
    assert!(
        !Path::new(&t.path().join("home/argv")).exists(),
        "hyphae must not have run"
    );
}

#[tokio::test]
async fn missing_keystore_is_409_not_configured() {
    let (app, t) = stub_app(0, r#"{"ok":true,"data":{}}"#, "").await;
    std::fs::remove_file(t.path().join("home/.hyphae/keystore.json")).unwrap();
    let (status, body) = call(&app, "GET", "/groupchat", None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_configured"))
    );
}

#[tokio::test]
async fn retry_and_status_and_invites_pass_the_expected_hyphae_flags() {
    let (app, t) = stub_app(0, r#"{"ok":true,"data":{"invites":null}}"#, "").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/groupchat/{G}/invites?identity=alice"),
        None,
    )
    .await;
    assert_eq!(
        (status, &body["data"]["invites"]),
        (StatusCode::OK, &json!([]))
    );
    assert_eq!(
        argv(&t),
        [
            "groupchat",
            "invite",
            "list",
            "--group",
            G,
            "--identity",
            "alice",
            "--password-stdin"
        ]
    );
    call(&app, "GET", &format!("/groupchat/{G}/messages/{L}"), None).await;
    assert_eq!(
        argv(&t),
        [
            "groupchat",
            "status",
            "--group-id",
            G,
            "--logical-id",
            L,
            "--password-stdin"
        ]
    );
    // a recipient must be a valid npub: this one is a real Hyphae-generated key
    let valid = "npub10pcu0s25x3z0fhvguk6ug7hq7ljf44e5tlcuh72xwvmr7dck409sfyjxaz";
    let (status, body) = call(
        &app,
        "POST",
        &format!("/groupchat/{G}/messages/{L}/retry"),
        Some(json!({"recipient": valid, "identity": "alice"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        argv(&t),
        [
            "groupchat",
            "retry",
            "--group-id",
            G,
            "--logical-id",
            L,
            "--recipient",
            valid,
            "--identity",
            "alice",
            "--password-stdin"
        ]
    );
}
