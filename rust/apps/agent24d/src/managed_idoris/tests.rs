#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    routing::{get, post},
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Default)]
struct Fixture {
    health: AtomicUsize,
    invalid: AtomicBool,
    hold_old: AtomicBool,
    started: Notify,
    release: Notify,
}

struct Server(String, Arc<Fixture>, tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.2.abort();
    }
}
async fn server() -> Server {
    let fixture = Arc::new(Fixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route(
            "/health",
            get(|State(f): State<Arc<Fixture>>| async move {
                f.health.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({"service":"idoris", "version":"0.4.0",
                "contract_version": if f.invalid.load(Ordering::SeqCst) {"2.0.0"} else {"1.3.0"},
                "instance_id":"fixture", "dev_no_key_enabled":false}))
            }),
        )
        .route(
            "/v1/chat/completions",
            post(
                |State(f): State<Arc<Fixture>>,
                 headers: HeaderMap,
                 Json(body): Json<serde_json::Value>| async move {
                    assert_eq!(body["model"], "idoris/daily");
                    let key = headers.get("authorization").unwrap().to_str().unwrap();
                    if key == "Bearer idk_old" && f.hold_old.load(Ordering::SeqCst) {
                        f.started.notify_one();
                        f.release.notified().await;
                    }
                    (
                        [
                            ("x-idoris-record-id", "record_fixture"),
                            ("x-idoris-served-locality", "loopback"),
                        ],
                        Json(
                            serde_json::json!({"model":key.trim_start_matches("Bearer idk_"),
                "choices":[{"message":{"role":"assistant","content":"ok"}}]}),
                        ),
                    )
                },
            ),
        )
        .with_state(fixture.clone());
    Server(
        url,
        fixture,
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
}

struct Secrets {
    locked: bool,
    reads: AtomicUsize,
}
#[async_trait::async_trait]
impl SecretSource for Secrets {
    async fn secret(&self, key_id: &str) -> Result<Zeroizing<String>, SecretError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.locked {
            Err(SecretError::Locked)
        } else {
            Ok(Zeroizing::new(format!("idk_{key_id}")))
        }
    }
}
struct Ready(Notify);
#[async_trait::async_trait]
impl SupervisorReady for Ready {
    async fn wait_ready(&self, cancel: &CancellationToken) -> Result<(), ModelError> {
        tokio::select! {
            () = cancel.cancelled() => Err(ModelError::Cancelled),
            () = self.0.notified() => Ok(())
        }
    }
}
fn config(url: &str, key: &str) -> ManagedConfig {
    ManagedConfig {
        url: url.into(),
        key_id: key.into(),
    }
}
fn secrets(locked: bool) -> Secrets {
    Secrets {
        locked,
        reads: AtomicUsize::new(0),
    }
}
fn manager(path: PathBuf) -> (Arc<ModelRouter>, Arc<ManagedIdoris>) {
    let router = Arc::new(ModelRouter::with_defaults(vec![]));
    let managed = Arc::new(ManagedIdoris::new(router.clone(), path).unwrap());
    (router, managed)
}
async fn verify(router: &ModelRouter) -> String {
    let request = agent24_models::CompletionRequest {
        messages: vec![agent24_models::Msg::user("probe")],
        model: None,
        tools: vec![],
        response_format: None,
        max_tokens: std::num::NonZeroU32::new(1),
        disable_thinking: true,
        idoris_headroom_confirmed: false,
        temperature: None,
        progress: None,
    };
    router
        .complete_served(Default::default(), &request, &CancellationToken::new())
        .await
        .unwrap()
        .response
        .model_id
        .unwrap()
}

#[tokio::test]
async fn atomic_switch_keeps_in_flight_gateway_and_updates_shared_routers() {
    let server = server().await;
    let dir = tempfile::tempdir().unwrap();
    let (router, managed) = manager(dir.path().join("connection.json"));
    let modules = router.with_separate_health();
    managed
        .connect(
            config(&server.0, "old"),
            &secrets(false),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    server.1.hold_old.store(true, Ordering::SeqCst);
    let request = tokio::spawn({
        let router = router.clone();
        async move { verify(&router).await }
    });
    server.1.started.notified().await;
    managed
        .connect(
            config(&server.0, "new"),
            &secrets(false),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(verify(&router).await, "new");
    assert_eq!(verify(&modules).await, "new");
    server.1.release.notify_one();
    assert_eq!(request.await.unwrap(), "old");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&managed.path).unwrap()).unwrap();
    assert_eq!(saved, serde_json::json!({"url":server.0,"key_id":"new"}));
}

#[tokio::test]
async fn failed_health_preserves_gateway_and_persisted_config() {
    let server = server().await;
    let dir = tempfile::tempdir().unwrap();
    let (router, managed) = manager(dir.path().join("connection.json"));
    managed
        .connect(
            config(&server.0, "old"),
            &secrets(false),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let saved = std::fs::read(&managed.path).unwrap();
    server.1.invalid.store(true, Ordering::SeqCst);
    assert!(
        managed
            .connect(
                config(&server.0, "new"),
                &secrets(false),
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&managed.path).unwrap(), saved);
    server.1.invalid.store(false, Ordering::SeqCst);
    assert_eq!(verify(&router).await, "old");
}

#[tokio::test]
async fn locked_keychain_does_not_replace_gateway_or_contact_service() {
    let server = server().await;
    let dir = tempfile::tempdir().unwrap();
    let (router, managed) = manager(dir.path().join("connection.json"));
    managed
        .connect(
            config(&server.0, "old"),
            &secrets(false),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let health = server.1.health.load(Ordering::SeqCst);
    let error = managed
        .connect(
            config(&server.0, "new"),
            &secrets(true),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unlock Keychain"), "{error}");
    assert_eq!(server.1.health.load(Ordering::SeqCst), health);
    assert_eq!(verify(&router).await, "old");
}

#[tokio::test]
async fn restart_waits_for_ready_before_loading_secret_and_building_gateway() {
    let server = server().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connection.json");
    let (_, first) = manager(path.clone());
    first
        .connect(
            config(&server.0, "old"),
            &secrets(false),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    drop(first);
    let (router, restarted) = manager(path);
    let ready = Arc::new(Ready(Notify::new()));
    let secrets = Arc::new(secrets(false));
    let deps = ManagedDependencies {
        secrets: secrets.clone(),
        supervisor: ready.clone(),
    };
    let cancel = CancellationToken::new();
    let restore = restarted.restore(&deps, &cancel);
    tokio::pin!(restore);
    assert!(futures::poll!(&mut restore).is_pending());
    assert_eq!(secrets.reads.load(Ordering::SeqCst), 0);
    assert_eq!(server.1.health.load(Ordering::SeqCst), 1);
    assert!(router.idoris_gateway().is_err());
    ready.0.notify_one();
    assert!(restore.await.unwrap());
    assert_eq!(secrets.reads.load(Ordering::SeqCst), 1);
    assert_eq!(verify(&router).await, "old");
}

#[tokio::test]
async fn cancelled_restart_stays_disconnected_without_secret_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connection.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&config("http://127.0.0.1:1", "old")).unwrap(),
    )
    .unwrap();
    let (router, managed) = manager(path);
    let secrets = Arc::new(secrets(false));
    let deps = ManagedDependencies {
        secrets: secrets.clone(),
        supervisor: Arc::new(Ready(Notify::new())),
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        managed.restore(&deps, &cancel).await,
        Err(ModelError::Cancelled)
    ));
    assert_eq!(secrets.reads.load(Ordering::SeqCst), 0);
    assert!(router.idoris_gateway().is_err());
}

#[tokio::test]
async fn failed_persistence_keeps_old_live_gateway() {
    let server = server().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connection.json");
    let (router, managed) = manager(path.clone());
    managed
        .connect(
            config(&server.0, "old"),
            &secrets(false),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        managed
            .connect(
                config(&server.0, "new"),
                &secrets(false),
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(verify(&router).await, "old");
}

#[tokio::test]
async fn locked_keychain_on_restart_keeps_gateway_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connection.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&config("http://127.0.0.1:1", "old")).unwrap(),
    )
    .unwrap();
    let (router, managed) = manager(path);
    let ready = Arc::new(Ready(Notify::new()));
    ready.0.notify_one();
    let deps = ManagedDependencies {
        secrets: Arc::new(secrets(true)),
        supervisor: ready,
    };
    let error = managed
        .restore(&deps, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unlock Keychain"));
    assert!(router.idoris_gateway().is_err());
}
