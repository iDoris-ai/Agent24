//! COMM-G1 acceptance: every `/comm/groupchat/*` route against the REAL
//! Hyphae CLI (the release asset matching `hyphae.lock.json`), in a
//! throwaway HOME. Requires `HYPHAE_TEST_BIN`; without it each test prints
//! why and returns early (same convention as `router_lifecycle.rs`).
//!
//! Hyphae's partial-fan-out failure hook (`HYPHAE_FAIL_GROUP_RECIPIENT`) is
//! an env var the runner's `env_clear()` would strip, so that one test runs
//! the router over a verified wrapper script that sets it and `exec`s the
//! real binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    CommState, HyphaeLock, HyphaeRunner, MemoryPasswordStore, VerifiedBinary, current_platform,
    router,
};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fx {
    app: Router,
    tmp: tempfile::TempDir,
    source: PathBuf,
    store: Arc<MemoryPasswordStore>,
    home: PathBuf,
    bob: String,
    carol: String,
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
    let bytes = to_bytes(resp.into_body(), 4 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn install(source: &std::path::Path, dir: &std::path::Path) -> VerifiedBinary {
    let expected = HyphaeLock::embedded()
        .unwrap()
        .expected_for(&current_platform())
        .unwrap();
    VerifiedBinary::install(source, expected, dir)
        .await
        .expect("HYPHAE_TEST_BIN must be the release asset pinned by hyphae.lock.json")
}

impl Fx {
    /// Fresh HOME with identities alice (default), bob and carol.
    async fn new() -> Option<Self> {
        let source = PathBuf::from(std::env::var("HYPHAE_TEST_BIN").ok()?)
            .canonicalize()
            .expect("HYPHAE_TEST_BIN is readable");
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        std::fs::create_dir_all(&home).unwrap();
        let bin = install(&source, &tmp.path().join("bin")).await;
        let runner = Arc::new(HyphaeRunner::new(
            bin,
            home.clone(),
            Duration::from_secs(30),
        ));
        let store = Arc::new(MemoryPasswordStore::new());
        let app = router(CommState::ready(runner, store.clone(), home.clone()));
        let mut npubs = vec![];
        for (nick, default) in [("alice", true), ("bob", false), ("carol", false)] {
            let (s, b) = call(
                &app,
                "POST",
                "/identity",
                Some(json!({"nickname": nick, "default": default})),
            )
            .await;
            assert_eq!(s, StatusCode::OK, "{b}");
            npubs.push(b["data"]["npub"].as_str().unwrap().to_owned());
        }
        Some(Self {
            app,
            tmp,
            source,
            store,
            home,
            bob: npubs[1].clone(),
            carol: npubs[2].clone(),
        })
    }

    async fn create(&self) -> String {
        let (s, b) = call(
            &self.app,
            "POST",
            "/groupchat",
            Some(json!({"name": "g1", "invitees": [self.bob, self.carol], "identity": "alice"})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        b["data"]["group_id"].as_str().unwrap().to_owned()
    }

    async fn invite_ids(&self, g: &str) -> Vec<(String, String)> {
        let (s, b) = call(
            &self.app,
            "GET",
            &format!("/groupchat/{g}/invites?identity=alice"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        b["data"]["invites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["invitee_npub"].as_str().unwrap().to_owned(),
                    i["invite_id"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    async fn accept(&self, g: &str, who: &str, npub: &str) -> Value {
        let id = self
            .invite_ids(g)
            .await
            .into_iter()
            .find(|(n, _)| n == npub)
            .unwrap()
            .1;
        let (s, b) = call(
            &self.app,
            "POST",
            &format!("/groupchat/invites/{id}/accept"),
            Some(json!({"identity": who})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        b["data"].clone()
    }

    async fn active_group(&self) -> String {
        let g = self.create().await;
        assert_eq!(self.accept(&g, "bob", &self.bob).await["state"], "pending");
        assert_eq!(
            self.accept(&g, "carol", &self.carol).await["state"],
            "active"
        );
        g
    }

    async fn send(&self, g: &str, body: &str, logical: &str) -> (StatusCode, Value) {
        call(
            &self.app,
            "POST",
            &format!("/groupchat/{g}/messages"),
            Some(json!({"body": body, "logical_id": logical, "identity": "alice"})),
        )
        .await
    }
}

fn lid(n: u8) -> String {
    format!("{n:02x}").repeat(16)
}

macro_rules! fx {
    ($name:literal) => {
        match Fx::new().await {
            Some(fx) => fx,
            None => {
                eprintln!("skipping {}: HYPHAE_TEST_BIN is not set", $name);
                return;
            }
        }
    };
}

#[tokio::test]
async fn create_then_invites_list_pending_invites() {
    let fx = fx!("create");
    let g = fx.create().await;
    assert_eq!(g.len(), 32);
    let invites = fx.invite_ids(&g).await;
    assert_eq!(invites.len(), 2);
    let (_, b) = call(
        &fx.app,
        "GET",
        &format!("/groupchat/{g}/invites?identity=alice"),
        None,
    )
    .await;
    assert!(
        b["data"]["invites"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["state"] == "pending")
    );
}

#[tokio::test]
async fn accept_is_pending_until_the_last_invitee_then_active() {
    let fx = fx!("accept");
    let g = fx.create().await;
    let first = fx.accept(&g, "bob", &fx.bob).await;
    assert_eq!(
        (first["state"].as_str(), first["group_id"].as_str()),
        (Some("pending"), Some(g.as_str()))
    );
    let last = fx.accept(&g, "carol", &fx.carol).await;
    assert_eq!(last["state"], "active");
}

#[tokio::test]
async fn list_and_members_report_the_group_creator_first() {
    let fx = fx!("list/members");
    let (s, b) = call(&fx.app, "GET", "/groupchat?identity=alice", None).await;
    assert_eq!(
        (s, &b["data"]["groups"]),
        (StatusCode::OK, &json!([])),
        "no groups yet is []"
    );
    let g = fx.active_group().await;
    let (_, b) = call(&fx.app, "GET", "/groupchat?identity=alice", None).await;
    let groups = b["data"]["groups"].as_array().unwrap();
    assert_eq!(
        (
            groups.len(),
            groups[0]["group_id"].as_str(),
            groups[0]["state"].as_str()
        ),
        (1, Some(g.as_str()), Some("active"))
    );
    let (s, b) = call(&fx.app, "GET", &format!("/groupchat/{g}/members"), None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let members = b["data"]["members"].as_array().unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(members[0]["is_creator"], true);
}

#[tokio::test]
async fn send_then_status_reports_each_recipient_as_queued() {
    let fx = fx!("send/status");
    let g = fx.active_group().await;
    let logical = lid(1);
    let (s, b) = fx.send(&g, "hello", &logical).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (
            b["data"]["logical_id"].as_str(),
            b["data"]["queued"].as_u64()
        ),
        (Some(logical.as_str()), Some(2))
    );
    let (s, b) = call(
        &fx.app,
        "GET",
        &format!("/groupchat/{g}/messages/{logical}?identity=alice"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["data"]["partial"], false);
    assert_eq!(b["data"]["recipients"].as_array().unwrap().len(), 2);
    // a logical id Hyphae never saw is a caller error (user_error, exit 1 since
    // Hyphae v0.27.2), not a 500 and no longer an upstream 502
    let (s, _) = call(
        &fx.app,
        "GET",
        &format!("/groupchat/{g}/messages/{}", lid(9)),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn history_is_an_array_pages_by_cursor_and_rejects_bad_cursors() {
    let fx = fx!("history");
    let g = fx.active_group().await;
    let uri = |q: &str| format!("/groupchat/{g}/messages?identity=alice{q}");
    let (_, b) = call(&fx.app, "GET", &uri(""), None).await;
    assert_eq!(
        b["data"]["messages"],
        json!([]),
        "Go nil slice must surface as []"
    );
    for n in 1..=3u8 {
        assert_eq!(
            fx.send(&g, &format!("m{n}"), &lid(n)).await.0,
            StatusCode::OK
        );
    }
    let plain = |b: &Value| -> Vec<String> {
        b["data"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["plaintext"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, all) = call(&fx.app, "GET", &uri(""), None).await;
    assert_eq!(all["data"]["messages"].as_array().unwrap().len(), 3);
    let ids: Vec<String> = all["data"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect();
    let (_, before) = call(
        &fx.app,
        "GET",
        &uri(&format!("&before_id={}&limit=1", ids[2])),
        None,
    )
    .await;
    assert_eq!(plain(&before), [plain(&all)[1].clone()]);
    let (_, after) = call(&fx.app, "GET", &uri(&format!("&after_id={}", ids[0])), None).await;
    assert_eq!(plain(&after), plain(&all)[1..]);
    // unknown cursor: Hyphae user_error -> 400 invalid (not 502)
    let (s, b) = call(&fx.app, "GET", &uri("&before_id=nope"), None).await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid")),
        "{b}"
    );
    let (s, b) = call(
        &fx.app,
        "GET",
        &uri(&format!("&after_id={}", lid(200))),
        None,
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid")),
        "{b}"
    );
}

#[tokio::test]
async fn partial_fanout_is_502_partial_with_per_recipient_detail_and_retry_recovers() {
    let fx = fx!("partial/retry");
    let g = fx.active_group().await;
    // Same HOME + password store, but the binary is a verified wrapper that
    // arms Hyphae's fail-one-recipient hook before exec'ing the real one.
    let wrapper = format!(
        "#!/bin/sh\nHYPHAE_FAIL_GROUP_RECIPIENT={} exec {} \"$@\"\n",
        fx.bob,
        fx.source.display()
    );
    let path = fx.tmp.path().join("wrapper");
    std::fs::write(&path, &wrapper).unwrap();
    let digest = agent24_comm::binary::sha256_of(wrapper.as_bytes());
    let bin = VerifiedBinary::install(&path, digest, &fx.tmp.path().join("wbin"))
        .await
        .unwrap();
    let runner = Arc::new(HyphaeRunner::new(
        bin,
        fx.home.clone(),
        Duration::from_secs(30),
    ));
    let flaky = router(CommState::ready(runner, fx.store.clone(), fx.home.clone()));

    let logical = lid(7);
    let (s, b) = call(
        &flaky,
        "POST",
        &format!("/groupchat/{g}/messages"),
        Some(json!({"body": "half", "logical_id": logical, "max_retries": 1, "identity": "alice"})),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("partial")),
        "{b}"
    );
    let data = &b["data"];
    assert_eq!(
        (data["queued"].as_u64(), data["failed"].as_u64()),
        (Some(1), Some(1))
    );
    let states: Vec<_> = data["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["recipient_npub"].as_str().unwrap(),
                r["state"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(states.contains(&(fx.bob.as_str(), "failed")), "{states:?}");

    // retry through the normal router (no failure hook) requeues bob
    let (s, b) = call(
        &fx.app,
        "POST",
        &format!("/groupchat/{g}/messages/{logical}/retry"),
        Some(json!({"recipient": fx.bob, "identity": "alice"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (b["data"]["failed"].as_u64(), b["data"]["queued"].as_u64()),
        (Some(0), Some(2))
    );
}

/// 挑战 #1：以 `-` 开头的消息正文/群名/游标必须原样当作 flag 的值，
/// 不能被 Hyphae 的 flag 解析当成另一个 flag。
#[tokio::test]
async fn dash_leading_text_is_a_value_not_a_flag() {
    let fx = fx!("dash-leading");
    let g = fx.active_group().await;
    for (n, text) in [
        (2u8, "--help"),
        (3, "-x"),
        (4, "--password-stdin"),
        (5, "-"),
    ] {
        let logical = lid(n);
        let (s, b) = fx.send(&g, text, &logical).await;
        assert_eq!(s, StatusCode::OK, "{text}: {b}");
        let (s, b) = call(
            &fx.app,
            "GET",
            &format!("/groupchat/{g}/messages?identity=alice&limit=50"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert!(
            b["data"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["plaintext"] == text),
            "{text} not stored verbatim: {b}"
        );
    }
    let (s, b) = call(
        &fx.app,
        "POST",
        "/groupchat",
        Some(json!({"name": "--name", "invitees": [fx.bob, fx.carol], "identity": "alice"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let made = b["data"]["group_id"].as_str().unwrap().to_owned();
    let (s, b) = call(&fx.app, "GET", "/groupchat?identity=alice", None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(
        b["data"]["groups"].as_array().unwrap().iter().any(|g| {
            let id = g["group_id"].as_str().or_else(|| g["id"].as_str());
            id == Some(made.as_str()) && g["name"] == "--name"
        }),
        "{b}"
    );
}
