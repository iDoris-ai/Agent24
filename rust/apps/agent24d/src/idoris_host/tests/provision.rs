use super::*;
use crate::idoris_host::provision::*;
use agent24_idoris::{keys::Keys, setup::SetupBudget};
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Store {
    entries: SyncMutex<BTreeMap<String, Zeroizing<String>>>,
    fail_put: AtomicBool,
    missing_delete_is_error: AtomicBool,
    missing_deletes: AtomicUsize,
    lock_after_first_read: AtomicBool,
    reads: AtomicUsize,
}
impl Keychain for Store {
    fn get(&self, id: &str) -> Result<Zeroizing<String>, KeychainError> {
        if self.lock_after_first_read.load(Ordering::SeqCst)
            && self.reads.fetch_add(1, Ordering::SeqCst) > 0
        {
            return Err(KeychainError::KeychainLocked);
        }
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or(KeychainError::NotFound)
    }
    fn put(&self, id: &str, secret: &str) -> Result<(), KeychainError> {
        if self.fail_put.load(Ordering::SeqCst) {
            return Err(KeychainError::KeychainLocked);
        }
        self.entries
            .lock()
            .unwrap()
            .insert(id.into(), Zeroizing::new(secret.into()));
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<(), KeychainError> {
        if self.entries.lock().unwrap().remove(id).is_none()
            && self.missing_delete_is_error.load(Ordering::SeqCst)
        {
            self.missing_deletes.fetch_add(1, Ordering::SeqCst);
            return Err(KeychainError::NotFound);
        }
        Ok(())
    }
}
struct Provision {
    record: Installation,
    lock: IdorisLock,
    gate: AtomicBool,
    downloads: AtomicUsize,
    initial_gate: AtomicBool,
    budget_probes: AtomicUsize,
    verifications: AtomicUsize,
    no_key: AtomicBool,
    key_waiting: AtomicBool,
    budget_unavailable: AtomicBool,
    fail_install: AtomicBool,
    signature: Gatekeeper,
    install_roots: SyncMutex<Vec<PathBuf>>,
}
fn copy_release(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_release(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
    mode(to, 0o700);
}
#[async_trait]
impl Provisioner for Provision {
    async fn budget_suggestion(&self) -> Result<agent24_hw::IdorisBudgetSuggestion, &'static str> {
        self.budget_probes.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
        let total = if self.budget_unavailable.load(Ordering::SeqCst) {
            0
        } else {
            16 * 1024 * 1024 * 1024_u64
        };
        let profile = serde_json::from_value(json!({"total_mem_bytes": total, "avail_mem_bytes": total, "arch":"aarch64", "cpu_cores":8, "accelerator":"metal", "os":"macos"})).unwrap();
        agent24_hw::HardwareReport::new(profile, None).idoris_budget_suggestion()
    }
    async fn verify(&self, r: &Installation, mode: VerifyMode) -> Result<InstalledVersion, Code> {
        self.verifications.fetch_add(1, Ordering::SeqCst);
        agent24_idoris::install::verify_existing(
            &r.release_dir,
            &self.lock,
            &VerifiedArchive {
                path: r.archive_path.clone(),
                sha256: String::new(),
            },
            &self.signature,
            mode,
        )
        .map_err(|_| Code::NotInstalled)
    }
    async fn download(
        &self,
        _: &Path,
        cancel: &CancellationToken,
        progress: watch::Sender<agent24_idoris::download::DownloadProgress>,
    ) -> Result<VerifiedArchive, Code> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        while self.initial_gate.load(Ordering::SeqCst) {
            tokio::select! { _ = cancel.cancelled() => return Err(Code::Cancelled), _ = tokio::time::sleep(Duration::from_millis(5)) => {} }
        }
        progress.send_replace(agent24_idoris::download::DownloadProgress {
            bytes_done: 123,
            bytes_total: None,
        });
        while self.gate.load(Ordering::SeqCst) {
            tokio::select! { _ = cancel.cancelled() => return Err(Code::Cancelled), _ = tokio::time::sleep(Duration::from_millis(5)) => {} }
        }
        progress.send_replace(agent24_idoris::download::DownloadProgress {
            bytes_done: 456,
            bytes_total: Some(456),
        });
        Ok(VerifiedArchive {
            path: self.record.archive_path.clone(),
            sha256: String::new(),
        })
    }
    async fn install(
        &self,
        root: &Path,
        _: &VerifiedArchive,
        _: &CancellationToken,
    ) -> Result<InstalledVersion, Code> {
        self.install_roots.lock().unwrap().push(root.to_owned());
        if self.fail_install.load(Ordering::SeqCst) {
            return Err(Code::NotInstalled);
        }
        // A managed root other than the fixture's own receives a fresh copy, like a real install.
        if Some(root) != self.record.release_dir.parent() {
            let release = root.join("v0.4.0");
            // Like the real installer: an existing version directory is verified, not rewritten.
            if !release.exists() {
                copy_release(&self.record.release_dir, &release);
            }
            let record = Installation {
                release_dir: release,
                ..self.record.clone()
            };
            return self.verify(&record, VerifyMode::Install).await;
        }
        self.verify(&self.record, VerifyMode::Install).await
    }
    async fn upstream_key(&self) -> Result<Option<ApiKey>, Code> {
        if self.gate.load(Ordering::SeqCst) {
            self.key_waiting.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
        if self.no_key.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(Some(
            ApiKey::new(Zeroizing::new("sk-upstream-sensitive".into())).unwrap(),
        ))
    }
    async fn budget(&self, core: Option<u64>) -> Result<SetupBudget, Code> {
        SystemProvisioner.budget(core).await
    }
}
struct ConnectedFixture {
    f: Fixture,
    provision: Arc<Provision>,
    data: Arc<Store>,
    upstream: Arc<Store>,
    endpoint: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for ConnectedFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl ConnectedFixture {
    async fn new() -> Self {
        use axum::{Json, Router, routing::get};
        let mut f = Fixture::new().await;
        let provision = Arc::new(Provision {
            record: f.record.clone(),
            lock: f.runtime.lock.clone(),
            gate: AtomicBool::new(false),
            downloads: AtomicUsize::new(0),
            initial_gate: AtomicBool::new(false),
            budget_probes: AtomicUsize::new(0),
            verifications: AtomicUsize::new(0),
            no_key: AtomicBool::new(false),
            key_waiting: AtomicBool::new(false),
            budget_unavailable: AtomicBool::new(false),
            fail_install: AtomicBool::new(false),
            signature: Gatekeeper::default(),
            install_roots: SyncMutex::new(Vec::new()),
        });
        let data = Arc::new(Store::default());
        let upstream = Arc::new(Store::default());
        f.host = f
            .host
            .with_provisioner(provision.clone(), data.clone(), upstream.clone())
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/v1/models", get(|headers: axum::http::HeaderMap| async move {
            if headers.get("authorization").is_some_and(|v| v == "Bearer sk-upstream-sensitive") { (axum::http::StatusCode::OK, Json(json!({"data": [{"id": "real-model", "object":"model", "owned_by":"omlx"}]}))) }
            else { (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"auth required"}))) }
        })).route("/openapi.json", get(|| async { Json(json!({"openapi":"3.1.0", "info":{"title":"oMLX API"}, "paths":{"/v1/models":{"get":{}}, "/health":{"get":{}}, "/api/status":{"get":{}}}})) }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            f,
            provision,
            data,
            upstream,
            endpoint,
            server,
        }
    }
    async fn installed(&self) {
        let mut record = self.f.record.clone();
        record.key_id = None;
        record.components_dir = None;
        record.desired_running = false;
        self.f.host.record_installation(record).await.unwrap();
    }
    fn request(&self) -> ConnectRequest {
        ConnectRequest {
            endpoint: self.endpoint.clone(),
            daily_model: "real-model".into(),
            deep_model: None,
            save_omlx_key: false,
            replace: false,
            core_reserved_bytes: Some(0),
            consent: Some(ConnectConsent {
                share_omlx_key: true,
                daily_model: "real-model".into(),
                deep_model: None,
            }),
        }
    }
    fn scenario(&self, value: serde_json::Value) {
        fs::write(
            self.f.host.root.join("provision-scenario.json"),
            value.to_string(),
        )
        .unwrap();
    }
    fn keys(&self) -> Vec<serde_json::Value> {
        serde_json::from_str(
            &fs::read_to_string(self.f.host.root.join("fake-keys.json")).unwrap_or("[]".into()),
        )
        .unwrap()
    }
    fn assert_safe(&self) {
        self.f.assert_no_secrets();
        fn walk(path: &Path) {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path);
                } else if path
                    .extension()
                    .is_some_and(|e| e == "json" || e == "jsonl" || e == "log")
                {
                    let body = fs::read_to_string(path).unwrap();
                    assert!(!body.contains("sk-upstream-sensitive"));
                    assert!(!body.contains(&format!("idk_{}", "a".repeat(64))));
                }
            }
        }
        walk(&self.f.host.root);
    }
}
#[tokio::test]
async fn install_reverifies_and_is_idempotent_and_cancellable() {
    let x = ConnectedFixture::new().await;
    let op =
        x.f.host
            .install(InstallRequest { consent: true }, Some("install-1"))
            .await
            .unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Succeeded));
    assert_eq!(x.provision.downloads.load(Ordering::SeqCst), 1);
    let second =
        x.f.host
            .install(InstallRequest { consent: true }, Some("install-2"))
            .await
            .unwrap();
    assert_eq!(x.f.complete(&second).await.result, Some(Outcome::Succeeded));
    assert_eq!(x.provision.downloads.load(Ordering::SeqCst), 1);
    assert!(x.provision.verifications.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        x.f.host
            .install(InstallRequest { consent: true }, Some("install-1"))
            .await
            .unwrap()
            .operation_id,
        op.operation_id
    );
    assert!(matches!(
        x.f.host
            .install(InstallRequest { consent: false }, Some("no-consent"))
            .await,
        Err(Code::ConsentRequired)
    ));
    let x = ConnectedFixture::new().await;
    x.provision.gate.store(true, Ordering::SeqCst);
    let op =
        x.f.host
            .install(InstallRequest { consent: true }, Some("cancel-install"))
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while x.provision.downloads.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot =
                serde_json::to_value(x.f.host.operation(&op.operation_id).await.unwrap()).unwrap();
            if snapshot["download_progress"]["bytes_done"] == 123 {
                assert_eq!(snapshot["phase"], "downloading");
                assert!(snapshot["download_progress"].get("bytes_total").is_none());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    x.f.host.cancel(&op.operation_id).await.unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Cancelled));
    assert!(!x.f.host.root.join("installation.json").exists());
}

#[tokio::test]
async fn budget_suggestion_matches_hw_allocation_and_probe_failure_is_explicit() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    let expected = x.provision.budget_suggestion().await.unwrap();
    let value = serde_json::to_value(x.f.host.status().await).unwrap();
    assert_eq!(
        value["budget_suggestion"],
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(value["budget_unavailable_reason"], serde_json::Value::Null);
    let x = ConnectedFixture::new().await;
    x.installed().await;
    x.provision.budget_unavailable.store(true, Ordering::SeqCst);
    let value = serde_json::to_value(x.f.host.status().await).unwrap();
    assert_eq!(value["budget_suggestion"], serde_json::Value::Null);
    assert_eq!(
        value["budget_unavailable_reason"],
        "hardware_memory_unavailable"
    );
}
#[tokio::test]
async fn status_caches_budget_probe_across_concurrent_and_repeated_calls() {
    for unavailable in [false, true] {
        let x = ConnectedFixture::new().await;
        x.provision
            .budget_unavailable
            .store(unavailable, Ordering::SeqCst);
        x.installed().await;
        let (first, second) = tokio::join!(x.f.host.status(), x.f.host.status());
        let third = x.f.host.status().await;
        assert_eq!(x.provision.budget_probes.load(Ordering::SeqCst), 1);
        for status in [first, second, third] {
            assert_eq!(status.budget_suggestion.is_none(), unavailable);
            assert_eq!(status.budget_unavailable_reason.is_some(), unavailable);
        }
        x.f.install().await;
        assert!(x.f.host.status().await.budget_suggestion.is_none());
        assert_eq!(x.provision.budget_probes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn status_skips_budget_probe_outside_setup() {
    let x = ConnectedFixture::new().await;
    assert!(x.f.host.status().await.budget_suggestion.is_none());
    x.f.install().await;
    assert!(x.f.host.status().await.budget_suggestion.is_none());
    assert_eq!(x.provision.budget_probes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn install_projects_zero_progress_before_the_first_download_update() {
    let x = ConnectedFixture::new().await;
    x.provision.initial_gate.store(true, Ordering::SeqCst);
    let op =
        x.f.host
            .install(InstallRequest { consent: true }, Some("progress-test"))
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while x.provision.downloads.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let snapshot = x.f.host.operation(&op.operation_id).await.unwrap();
    x.f.host.cancel(&op.operation_id).await.unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Cancelled));
    assert_eq!(snapshot.phase, Phase::Downloading);
    assert_eq!(
        serde_json::to_value(snapshot).unwrap()["download_progress"],
        json!({"bytes_done": 0})
    );
}

#[tokio::test]
async fn install_flushes_the_last_progress_when_download_completes() {
    let x = ConnectedFixture::new().await;
    let op =
        x.f.host
            .install(InstallRequest { consent: true }, Some("progress-test"))
            .await
            .unwrap();
    let done = x.f.complete(&op).await;
    assert_eq!(done.result, Some(Outcome::Succeeded));
    assert_eq!(
        serde_json::to_value(done).unwrap()["download_progress"],
        json!({"bytes_done":456, "bytes_total":456})
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(x.f.host.root.join("operations.json")).unwrap()).unwrap();
    assert_eq!(
        persisted[&op.operation_id]["download_progress"],
        json!({"bytes_done":456, "bytes_total":456})
    );
}

#[tokio::test]
async fn connect_rejects_oversized_or_unsafe_verification_model_id_without_commit() {
    for model_id in [
        "x".repeat(257),
        "bad\nmodel".into(),
        "bad\u{7f}model".into(),
        "bad\u{061c}model".into(),
        "bad\u{200e}model".into(),
        "bad\u{202e}model".into(),
        "bad\u{2066}model".into(),
    ] {
        let x = ConnectedFixture::new().await;
        x.installed().await;
        let before = fs::read(x.f.host.root.join("installation.json")).unwrap();
        x.scenario(json!({"serve":{"verify_model": model_id}}));
        let op =
            x.f.host
                .connect(x.request(), Some("invalid-receipt"))
                .await
                .unwrap();
        let done = x.f.complete(&op).await;
        x.f.host.shutdown().await.unwrap();
        assert_eq!(done.error_code, Some(Code::VerifyFailed));
        assert_eq!(done.result, Some(Outcome::Failed));
        assert!(done.verification.is_none());
        assert_eq!(
            fs::read(x.f.host.root.join("installation.json")).unwrap(),
            before
        );
        assert!(x.f.host.status().await.verification.is_none());
        assert!(!x.f.host.root.join("connection.json").exists());
        assert!(!x.f.host.root.join("pending-connect.json").exists());
        assert!(x.f.host.router.idoris_gateway().is_err());
        assert_eq!(x.keys()[0]["status"], "revoked");
        assert!(x.data.entries.lock().unwrap().is_empty());
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(x.f.host.root.join("operations.json")).unwrap())
                .unwrap();
        assert!(persisted[&op.operation_id].get("verification").is_none());
        x.f.assert_clean();
    }
}

#[tokio::test]
async fn connect_accepts_verification_model_id_at_the_limit() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    let model_id = "x".repeat(256);
    x.scenario(json!({"serve":{"verify_model": model_id}}));
    let op =
        x.f.host
            .connect(x.request(), Some("bounded-receipt"))
            .await
            .unwrap();
    let done = x.f.complete(&op).await;
    x.f.host.shutdown().await.unwrap();
    assert_eq!(done.result, Some(Outcome::Succeeded));
    assert_eq!(done.verification.unwrap().model_id, model_id);
    assert_eq!(
        x.f.host.status().await.verification.unwrap().model_id,
        model_id
    );
    x.f.assert_clean();
}

#[tokio::test]
async fn candidates_expose_identity_models_and_source_but_never_credentials() {
    let x = ConnectedFixture::new().await;
    let value =
        serde_json::to_value(x.f.host.candidates(Some(&x.endpoint)).await.unwrap()).unwrap();
    assert_eq!(value["key_from_local_settings"], true);
    let candidate = value["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["endpoint"].as_str().unwrap().trim_end_matches('/') == x.endpoint)
        .unwrap();
    assert_eq!(candidate["identity"], "omlx");
    assert_eq!(candidate["models"], json!(["real-model"]));
    assert!(!value.to_string().contains("sk-upstream"));
}
#[tokio::test]
async fn connect_verifies_chat_commits_once_and_requires_explicit_replace() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    let request = x.request();
    let op =
        x.f.host
            .connect(request.clone(), Some("connect-once"))
            .await
            .unwrap();
    let done = x.f.complete(&op).await;
    assert_eq!(
        done.result,
        Some(Outcome::Succeeded),
        "{:?}",
        done.error_code
    );
    assert_eq!(x.f.host.status().await.state, State::Running);
    let status = serde_json::to_value(x.f.host.status().await).unwrap();
    let receipt = &status["verification"];
    assert_eq!(receipt["locality"], "loopback");
    assert_eq!(receipt["model_id"], "real-model");
    assert_eq!(receipt["record_id"], "fixture-chat");
    assert_eq!(receipt.as_object().unwrap().len(), 4);
    assert!(chrono::DateTime::parse_from_rfc3339(receipt["verified_at"].as_str().unwrap()).is_ok());
    assert_eq!(
        serde_json::to_value(&done).unwrap()["verification"],
        *receipt
    );
    assert!(x.f.host.router.idoris_gateway().is_ok());
    assert_eq!(x.keys().len(), 1);
    assert_eq!(x.keys()[0]["allowed_roles"], json!(["daily"]));
    assert!(x.upstream.entries.lock().unwrap().is_empty());
    let replay =
        x.f.host
            .connect(request.clone(), Some("connect-once"))
            .await
            .unwrap();
    assert_eq!(replay.operation_id, op.operation_id);
    assert!(matches!(
        x.f.host.connect(request.clone(), Some("different")).await,
        Err(Code::ReplaceRequired)
    ));
    let mut changed = request.clone();
    changed.save_omlx_key = true;
    assert!(matches!(
        x.f.host.connect(changed, Some("connect-once")).await,
        Err(Code::InvalidRequest)
    ));
    assert_eq!(x.keys().len(), 1);
    let consent: serde_json::Value = serde_json::from_slice(
        &fs::read(
            x.f.host
                .root
                .join(format!("consents/{}.json", op.operation_id)),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(consent["confirmed_at"].as_str().is_some());
    x.f.host.shutdown().await.unwrap();
    let reopened = IdorisHost::open(
        x.f.host.root.clone(),
        x.f.host.managed.clone(),
        x.f.host.router.clone(),
        x.f.host.runtime.clone(),
        x.f.host.secrets.clone(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(reopened.status().await).unwrap()["verification"],
        *receipt
    );
    assert_eq!(
        serde_json::to_value(reopened.operation(&op.operation_id).await.unwrap()).unwrap()["verification"],
        *receipt
    );
    reopened.shutdown().await.unwrap();
    x.f.assert_clean();
    x.assert_safe();
}
#[tokio::test]
async fn connect_requires_consent_and_matching_selected_models() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    for consent in [
        None,
        Some(ConnectConsent {
            share_omlx_key: false,
            daily_model: "real-model".into(),
            deep_model: None,
        }),
        Some(ConnectConsent {
            share_omlx_key: true,
            daily_model: "other-model".into(),
            deep_model: None,
        }),
    ] {
        let mut request = x.request();
        request.consent = consent;
        assert!(matches!(
            x.f.host.connect(request, Some("no-consent")).await,
            Err(Code::ConsentRequired)
        ));
    }
    assert!(x.keys().is_empty());
}
#[tokio::test]
async fn each_connect_failure_preserves_receipt_and_revokes_new_keys() {
    for (scenario, locked, upstream_locked, expected, issued) in [
        (
            json!({"setup_code":"write_failed"}),
            false,
            false,
            Code::SetupFailed,
            false,
        ),
        (
            json!({"issue_fail":true}),
            false,
            false,
            Code::KeyUnavailable,
            false,
        ),
        (json!({}), true, false, Code::KeychainLocked, true),
        (json!({}), false, true, Code::KeychainLocked, true),
        (
            json!({"serve":{"mode":"exit", "code":1}}),
            false,
            false,
            Code::StartFailed,
            true,
        ),
        (
            json!({"serve":{"verify_status":500}}),
            false,
            false,
            Code::VerifyFailed,
            true,
        ),
    ] {
        let x = ConnectedFixture::new().await;
        x.installed().await;
        x.scenario(scenario);
        x.data.fail_put.store(locked, Ordering::SeqCst);
        x.upstream.fail_put.store(upstream_locked, Ordering::SeqCst);
        let before = fs::read(x.f.host.root.join("installation.json")).unwrap();
        let mut request = x.request();
        request.save_omlx_key = upstream_locked;
        let op = x.f.host.connect(request, Some("failure")).await.unwrap();
        let done = x.f.complete(&op).await;
        assert_eq!(done.error_code, Some(expected));
        if expected == Code::SetupFailed {
            assert_eq!(
                done.setup_code,
                Some(agent24_idoris::setup::SetupCode::WriteFailed)
            );
        }
        assert_eq!(
            fs::read(x.f.host.root.join("installation.json")).unwrap(),
            before
        );
        assert_eq!(!x.keys().is_empty(), issued);
        assert!(x.keys().iter().all(|k| k["status"] == "revoked"));
        assert!(x.data.entries.lock().unwrap().is_empty());
        assert!(x.upstream.entries.lock().unwrap().is_empty());
        assert!(!x.f.host.root.join("pending-connect.json").exists());
        x.f.assert_clean();
        x.assert_safe();
    }
}
#[tokio::test]
async fn cancel_after_issuance_revokes_and_reaps() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    x.scenario(json!({"serve":{"verify_wait":true}}));
    let op =
        x.f.host
            .connect(x.request(), Some("cancel-connect"))
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while x.f.host.operation(&op.operation_id).await.unwrap().phase
            != Phase::VerifyingConnection
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    x.f.host.cancel(&op.operation_id).await.unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Cancelled));
    assert!(x.keys().iter().all(|k| k["status"] == "revoked"));
    x.f.assert_clean();
    x.assert_safe();
}

#[tokio::test]
async fn explicit_replace_retires_old_key_only_after_success_and_saves_upstream_separately() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    let op = x.f.host.connect(x.request(), Some("first")).await.unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Succeeded));
    let old = x.keys()[0]["key_id"].as_str().unwrap().to_owned();
    let mut request = x.request();
    request.replace = true;
    request.save_omlx_key = true;
    let op = x.f.host.connect(request, Some("replace")).await.unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Succeeded));
    let keys = x.keys();
    // Stop before assertions so a failing regression cannot leave a child behind.
    x.f.host.shutdown().await.unwrap();
    x.f.assert_clean();
    assert_eq!(keys[0]["status"], "revoked");
    assert_eq!(keys[1]["status"], "active");
    assert!(!x.data.entries.lock().unwrap().contains_key(&old));
    assert_eq!(x.upstream.entries.lock().unwrap().len(), 1);
    assert!(
        x.data
            .entries
            .lock()
            .unwrap()
            .keys()
            .all(|id| id.starts_with("vk_"))
    );
    assert_ne!(
        agent24_idoris::keychain::SERVICE,
        agent24_idoris::keychain::OMLX_SERVICE
    );
    x.assert_safe();
}

#[tokio::test]
async fn failed_replace_preserves_old_key_and_configuration_for_restart() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    let op = x.f.host.connect(x.request(), Some("first")).await.unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Succeeded));
    let before = fs::read(x.f.host.root.join("installation.json")).unwrap();
    let connection = fs::read(x.f.host.root.join("connection.json")).unwrap();
    x.scenario(json!({"serve":{"verify_status":500}}));
    let mut request = x.request();
    request.replace = true;
    let op =
        x.f.host
            .connect(request, Some("failed-replace"))
            .await
            .unwrap();
    assert_eq!(x.f.complete(&op).await.error_code, Some(Code::VerifyFailed));
    assert_eq!(x.keys()[0]["status"], "active");
    assert_eq!(x.keys()[1]["status"], "revoked");
    assert_eq!(
        fs::read(x.f.host.root.join("installation.json")).unwrap(),
        before
    );
    assert_eq!(
        fs::read(x.f.host.root.join("connection.json")).unwrap(),
        connection
    );
    x.f.assert_clean();
    let op = x.f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Succeeded));
    x.f.host.shutdown().await.unwrap();
    x.f.assert_clean();
    x.assert_safe();
}

#[tokio::test]
async fn failed_revocation_is_durable_and_recovery_mints_no_second_key() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    x.scenario(json!({"serve":{"verify_status":500}, "revoke_fail":true}));
    let op =
        x.f.host
            .connect(x.request(), Some("pending-revoke"))
            .await
            .unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Failed));
    assert!(x.f.host.root.join("pending-connect.json").exists());
    x.f.assert_clean();
    x.scenario(json!({}));
    x.f.host.restore().await.unwrap();
    assert_eq!(x.keys().len(), 1);
    assert_eq!(x.keys()[0]["status"], "revoked");
    assert!(!x.f.host.root.join("pending-connect.json").exists());
    assert!(x.data.entries.lock().unwrap().is_empty());
    x.assert_safe();
}

#[tokio::test]
async fn restore_finishes_committed_replace_after_old_key_revoke_failure() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    let first = x.f.host.connect(x.request(), Some("first")).await.unwrap();
    assert_eq!(x.f.complete(&first).await.result, Some(Outcome::Succeeded));
    x.scenario(json!({"revoke_fail":true}));
    let mut request = x.request();
    request.replace = true;
    let op = x.f.host.connect(request, Some("replace")).await.unwrap();
    assert_eq!(
        x.f.complete(&op).await.error_code,
        Some(Code::KeyUnavailable)
    );
    let pending = x.f.host.root.join("pending-connect.json");
    assert!(pending.exists());
    assert_eq!(x.keys()[0]["status"], "active");
    assert!(
        !x.f.host
            .root
            .join("data/idoris.key-operation.json")
            .exists()
    );
    x.scenario(json!({}));
    let receipt =
        x.f.host
            .operation(&op.operation_id)
            .await
            .unwrap()
            .verification
            .unwrap();
    x.f.host
        .inner
        .lock()
        .await
        .operations
        .get_mut(&op.operation_id)
        .unwrap()
        .verification = None;
    let restored = x.f.host.restore().await;
    // Always reap the running replacement before a regression assertion can panic.
    let cleared = !pending.exists();
    let keys = x.keys();
    let next = x.f.host.connect(x.request(), Some("next")).await;
    x.f.host.shutdown().await.unwrap();
    assert_eq!(restored, Ok(()));
    assert!(cleared);
    let recovered = x.f.host.operation(&op.operation_id).await.unwrap();
    assert_eq!(
        serde_json::to_value(recovered.verification).unwrap(),
        serde_json::to_value(&receipt).unwrap()
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(x.f.host.root.join("operations.json")).unwrap()).unwrap();
    assert_eq!(
        persisted[&op.operation_id]["verification"],
        serde_json::to_value(receipt).unwrap()
    );
    assert_eq!(keys[0]["status"], "revoked");
    assert_eq!(keys[1]["status"], "active");
    assert!(matches!(next, Err(Code::ReplaceRequired)));
    assert_eq!(
        x.f.host.operation(&op.operation_id).await.unwrap().result,
        Some(Outcome::Succeeded)
    );
    x.f.assert_clean();
    x.assert_safe();
}

#[tokio::test]
async fn restore_after_crash_between_commit_issue_and_connect_journal_removal() {
    let mut x = ConnectedFixture::new().await;
    x.installed().await;
    let first = x.f.host.connect(x.request(), Some("first")).await.unwrap();
    assert_eq!(x.f.complete(&first).await.result, Some(Outcome::Succeeded));
    let previous = x.f.host.inner.lock().await.installation.clone().unwrap();
    x.f.host.shutdown().await.unwrap();
    let installed = x
        .provision
        .verify(&previous, VerifyMode::AtRest)
        .await
        .unwrap();
    let keys = Keys::new(
        &installed,
        &x.f.host.root.join("data/idoris.sqlite3"),
        x.data.clone(),
    )
    .unwrap();
    let issued = keys.issue_pending("managed", &["daily"]).await.unwrap();
    let mut candidate = previous.clone();
    candidate.key_id = Some(issued.key_id);
    let pending = json!({"previous": previous, "candidate": candidate, "committed": true, "operation_id": "crashed-replace"});
    write_json(&x.f.host.root.join("installation.json"), &candidate).unwrap();
    x.f.host
        .managed
        .persist_config(&ManagedConfig {
            url: format!("http://127.0.0.1:{}", candidate.data_port),
            key_id: candidate.key_id.clone().unwrap(),
        })
        .unwrap();
    write_json(&x.f.host.root.join("pending-connect.json"), &pending).unwrap();
    keys.commit_issue().unwrap();
    // Reopen only the durable state left by a crash at this exact cleanup boundary.
    x.f.host = IdorisHost::open(
        x.f.host.root.clone(),
        x.f.host.managed.clone(),
        x.f.host.router.clone(),
        x.f.runtime.clone(),
        x.f.host.secrets.clone(),
    )
    .unwrap()
    .with_provisioner(x.provision.clone(), x.data.clone(), x.upstream.clone())
    .unwrap();
    let restored = x.f.host.restore().await;
    if restored.is_ok()
        && let Some(id) = x.f.host.status().await.current_operation
    {
        let start = x.f.host.operation(&id).await.unwrap();
        assert_eq!(x.f.complete(&start).await.result, Some(Outcome::Succeeded));
    }
    let next = x.f.host.connect(x.request(), Some("next")).await;
    x.f.host.shutdown().await.unwrap();
    assert_eq!(restored, Ok(()));
    assert!(matches!(next, Err(Code::ReplaceRequired)));
    assert!(!x.f.host.root.join("pending-connect.json").exists());
    assert_eq!(x.keys()[0]["status"], "revoked");
    assert_eq!(x.keys()[1]["status"], "active");
    x.f.assert_clean();
    x.assert_safe();
}

#[tokio::test]
async fn rollback_accepts_data_key_already_deleted_by_compensation() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    x.data.missing_delete_is_error.store(true, Ordering::SeqCst);
    x.scenario(json!({"serve":{"verify_status":500}}));
    let op =
        x.f.host
            .connect(x.request(), Some("rollback-missing"))
            .await
            .unwrap();
    assert_eq!(x.f.complete(&op).await.error_code, Some(Code::VerifyFailed));
    assert_eq!(x.data.missing_deletes.load(Ordering::SeqCst), 1);
    assert!(!x.f.host.root.join("pending-connect.json").exists());
    assert!(x.keys().iter().all(|key| key["status"] == "revoked"));
    x.f.assert_clean();
}

#[tokio::test]
async fn cancellation_while_waiting_for_commit_lock_rolls_back() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    let before = fs::read(x.f.host.root.join("installation.json")).unwrap();
    x.scenario(json!({"serve":{"verify_wait":true}}));
    let op =
        x.f.host
            .connect(x.request(), Some("cancel-at-commit"))
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while x.f.host.operation(&op.operation_id).await.unwrap().phase
            != Phase::VerifyingConnection
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut guard = x.f.host.inner.lock().await;
    let cancel = guard.active.as_ref().unwrap().1.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::time::sleep(Duration::from_millis(5)).await;
            drop(guard);
            // Tokio reserves the released permit for the queued commit waiter.
            // Cancel synchronously before yielding to that waiter.
            match x.f.host.inner.try_lock() {
                Ok(reacquired) => guard = reacquired,
                Err(_) => {
                    cancel.cancel();
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    let done = x.f.complete(&op).await;
    x.f.host.shutdown().await.unwrap();
    assert_eq!(done.result, Some(Outcome::Cancelled));
    assert_eq!(
        fs::read(x.f.host.root.join("installation.json")).unwrap(),
        before
    );
    assert!(!x.f.host.root.join("connection.json").exists());
    assert!(!x.f.host.root.join("pending-connect.json").exists());
    assert!(x.keys().iter().all(|key| key["status"] == "revoked"));
    x.f.assert_clean();
}

#[tokio::test]
async fn connect_distinguishes_explicit_zero_core_reservation_from_omission() {
    for core in [None, Some(0)] {
        let x = ConnectedFixture::new().await;
        x.installed().await;
        let mut record = x.f.host.inner.lock().await.installation.clone().unwrap();
        record.core_reserved_bytes = None;
        x.f.host.record_installation(record).await.unwrap();
        let mut body = serde_json::to_value(x.request()).unwrap();
        body.as_object_mut().unwrap().remove("core_reserved_bytes");
        if let Some(core) = core {
            body["core_reserved_bytes"] = json!(core);
        }
        let op =
            x.f.host
                .connect(
                    serde_json::from_value(body).unwrap(),
                    Some("core-reservation"),
                )
                .await
                .unwrap();
        let done = x.f.complete(&op).await;
        x.f.host.shutdown().await.unwrap();
        if core.is_some() {
            assert_eq!(done.result, Some(Outcome::Succeeded));
            let receipt: Installation =
                serde_json::from_slice(&fs::read(x.f.host.root.join("installation.json")).unwrap())
                    .unwrap();
            assert_eq!(receipt.core_reserved_bytes, Some(0));
        } else {
            assert_eq!(done.error_code, Some(Code::NotConfigured));
            assert!(x.keys().is_empty());
        }
        x.f.assert_clean();
    }
}

#[tokio::test]
async fn stale_generation_monitor_cannot_withdraw_replacement_gateway() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    let first =
        x.f.host
            .connect(x.request(), Some("generation-a"))
            .await
            .unwrap();
    assert_eq!(x.f.complete(&first).await.result, Some(Outcome::Succeeded));
    let record = x.f.host.inner.lock().await.installation.clone().unwrap();
    let mut request = x.request();
    request.replace = true;
    let second =
        x.f.host
            .connect(request, Some("generation-b"))
            .await
            .unwrap();
    assert_eq!(x.f.complete(&second).await.result, Some(Outcome::Succeeded));
    assert!(x.f.host.router.idoris_gateway().is_ok());
    let mut available = Vec::new();
    for ready in [false, true] {
        let (sender, signal) = watch::channel(!ready);
        let failure = Arc::new(SyncMutex::new(None));
        x.f.host.monitor(
            signal,
            record.clone(),
            first.operation_id.clone(),
            CancellationToken::new(),
            failure.clone(),
        );
        // Cancellation remains unset: generation identity alone must protect B
        // against A's not-ready, ready and closed notifications.
        sender.send(ready).unwrap();
        drop(sender);
        tokio::time::timeout(Duration::from_secs(2), async {
            while Arc::strong_count(&failure) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        available.push(x.f.host.router.idoris_gateway().is_ok());
    }
    let state = x.f.host.status().await.state;
    x.f.host.shutdown().await.unwrap();
    assert_eq!(available, vec![true, true]);
    assert_eq!(state, State::Running);
    x.f.assert_clean();
}

#[tokio::test]
async fn submit_rejects_install_and_connect_actions() {
    let x = ConnectedFixture::new().await;
    for action in [Action::Install, Action::Connect] {
        assert!(matches!(
            x.f.host.submit(action, Some("wrong-entrypoint")).await,
            Err(Code::InvalidRequest)
        ));
    }
    assert!(x.f.host.inner.lock().await.operations.is_empty());
    assert!(!x.f.host.root.join("operations.json").exists());
    assert!(x.keys().is_empty());
}

#[tokio::test]
#[ignore = "real signed v0.4.0/download; optional oMLX on 8088 and IDORIS_DAILY_MODEL"]
async fn real_v040_install_reverify_and_connect_when_omlx_available() {
    let source = std::env::var_os("IDORIS_REAL_V040_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap()).join("a24-codex/idoris-v040")
        });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("idoris");
    for part in ["0.4.0/config/components", "data/state"] {
        fs::create_dir_all(root.join(part)).unwrap();
    }
    for path in [
        root.clone(),
        root.join("0.4.0"),
        root.join("0.4.0/config"),
        root.join("0.4.0/config/components"),
    ] {
        mode(&path, 0o700);
    }
    for name in &IdorisLock::embedded().assets["aarch64-apple-darwin"].files {
        let path = root.join("0.4.0").join(name);
        fs::copy(source.join(name), &path).unwrap();
        mode(&path, if name == "idoris" { 0o500 } else { 0o400 });
    }
    let data = Arc::new(Store::default());
    let upstream = Arc::new(Store::default());
    let secrets: Arc<dyn SecretSource> = Arc::new(KeychainSource(data.clone()));
    let router = Arc::new(ModelRouter::idoris_unavailable("real smoke"));
    let managed =
        Arc::new(ManagedIdoris::new(router.clone(), root.join("connection.json")).unwrap());
    let host = IdorisHost::open(
        root.clone(),
        managed,
        router,
        Arc::new(SystemRuntime::new(secrets.clone())),
        secrets,
    )
    .unwrap()
    .with_provisioner(Arc::new(SystemProvisioner), data, upstream)
    .unwrap();
    let op = host
        .install(InstallRequest { consent: true }, Some("real-install"))
        .await
        .unwrap();
    async fn completed(host: &IdorisHost, op: &Operation) -> Operation {
        tokio::time::timeout(Duration::from_secs(360), async {
            loop {
                let value = host.operation(&op.operation_id).await.unwrap();
                if value.result.is_some() {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap()
    }
    let done = completed(&host, &op).await;
    assert_eq!(
        done.result,
        Some(Outcome::Succeeded),
        "{:?}",
        done.error_code
    );
    let op = host
        .install(InstallRequest { consent: true }, Some("real-reverify"))
        .await
        .unwrap();
    assert_eq!(completed(&host, &op).await.result, Some(Outcome::Succeeded));
    eprintln!(
        "PASS: pinned archive downloaded, real signed v0.4.0 tree reverified and install replay succeeded"
    );
    if tokio::net::TcpStream::connect("127.0.0.1:8088")
        .await
        .is_err()
    {
        eprintln!("SKIP: real connect/daily chat; no oMLX listening on 127.0.0.1:8088");
        host.shutdown().await.unwrap();
        return;
    }
    let model = std::env::var("IDORIS_DAILY_MODEL")
        .expect("set explicit IDORIS_DAILY_MODEL; never select the first model implicitly");
    let mut record = host.inner.lock().await.installation.clone().unwrap();
    record.data_port = port().await;
    record.admin_port = port().await;
    host.record_installation(record).await.unwrap();
    let request = ConnectRequest {
        endpoint: "http://127.0.0.1:8088".into(),
        daily_model: model.clone(),
        deep_model: None,
        save_omlx_key: false,
        replace: false,
        core_reserved_bytes: Some(0),
        consent: Some(ConnectConsent {
            share_omlx_key: true,
            daily_model: model,
            deep_model: None,
        }),
    };
    let op = host.connect(request, Some("real-connect")).await.unwrap();
    let done = completed(&host, &op).await;
    host.shutdown().await.unwrap();
    assert_eq!(
        done.result,
        Some(Outcome::Succeeded),
        "{:?}",
        done.error_code
    );
    assert!(!root.join("data/state/managed-process.json").exists());
}

#[tokio::test]
async fn preflight_failures_are_stable_and_do_not_issue_keys() {
    for (no_key, model, consent_model, core, expected) in [
        (
            true,
            "real-model",
            "real-model",
            Some(0),
            Code::OmlxAuthRequired,
        ),
        (false, "missing", "missing", Some(0), Code::ModelNotFound),
        (false, "real-model", "real-model", None, Code::NotConfigured),
    ] {
        let x = ConnectedFixture::new().await;
        x.installed().await;
        if core.is_none() {
            let mut r = x.f.host.inner.lock().await.installation.clone().unwrap();
            r.core_reserved_bytes = None;
            x.f.host.record_installation(r).await.unwrap();
        }
        x.provision.no_key.store(no_key, Ordering::SeqCst);
        let mut request = x.request();
        request.daily_model = model.into();
        request.consent.as_mut().unwrap().daily_model = consent_model.into();
        request.core_reserved_bytes = core;
        let op = x.f.host.connect(request, Some("preflight")).await.unwrap();
        assert_eq!(x.f.complete(&op).await.error_code, Some(expected));
        assert!(x.keys().is_empty());
        x.f.assert_clean();
        x.assert_safe();
    }
}

#[tokio::test]
async fn launcher_lock_rejects_other_hosts_and_cancel_retains_the_lock_until_done() {
    let x = ConnectedFixture::new().await;
    let other = IdorisHost::open(
        x.f.host.root.clone(),
        x.f.host.managed.clone(),
        x.f.host.router.clone(),
        x.f.runtime.clone(),
        x.f.host.secrets.clone(),
    )
    .unwrap();
    x.provision.gate.store(true, Ordering::SeqCst);
    let op =
        x.f.host
            .install(InstallRequest { consent: true }, Some("locked-install"))
            .await
            .unwrap();
    assert!(matches!(
        other.submit(Action::Stop, Some("other-stop")).await,
        Err(Code::Busy)
    ));
    assert!(matches!(
        x.f.host.connect(x.request(), Some("busy-connect")).await,
        Err(Code::Busy)
    ));
    assert!(matches!(
        x.f.host
            .install(InstallRequest { consent: true }, None)
            .await,
        Err(Code::InvalidRequest)
    ));
    x.f.host.cancel(&op.operation_id).await.unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Cancelled));
    x.f.assert_clean();
}

#[tokio::test]
async fn untrusted_services_and_rejected_credentials_have_distinct_codes() {
    use axum::{Json, Router, http::StatusCode, routing::get};
    for (omlx, expected) in [
        (false, Code::UnknownService),
        (true, Code::OmlxAuthRejected),
    ] {
        let x = ConnectedFixture::new().await;
        x.installed().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/v1/models", get(|| async { (StatusCode::UNAUTHORIZED, Json(json!({"error":"auth"}))) }))
            .route("/openapi.json", get(move || async move { Json(json!({"openapi":"3.1.0", "info":{"title": if omlx { "oMLX API" } else { "Other API" }}, "paths":{"/v1/models":{"get":{}}, "/health":{"get":{}}, "/api/status":{"get":{}}}})) }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut request = x.request();
        request.endpoint = endpoint;
        let op = x.f.host.connect(request, Some("classified")).await.unwrap();
        assert_eq!(x.f.complete(&op).await.error_code, Some(expected));
        server.abort();
        assert!(x.keys().is_empty());
        x.f.assert_clean();
    }
    let x = ConnectedFixture::new().await;
    x.installed().await;
    x.server.abort();
    let op =
        x.f.host
            .connect(x.request(), Some("unreachable"))
            .await
            .unwrap();
    assert_eq!(
        x.f.complete(&op).await.error_code,
        Some(Code::OmlxUnreachable)
    );
    assert!(x.keys().is_empty());
}

#[tokio::test]
async fn connect_cancel_interrupts_pending_upstream_credential_read() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    x.provision.gate.store(true, Ordering::SeqCst);
    let op =
        x.f.host
            .connect(x.request(), Some("pending-key-read"))
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !x.provision.key_waiting.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    x.f.host.cancel(&op.operation_id).await.unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(1), x.f.complete(&op))
        .await
        .expect("cancellation must interrupt a pending read");
    assert_eq!(completed.result, Some(Outcome::Cancelled));
    assert!(x.keys().is_empty());
    x.f.assert_clean();
}

#[tokio::test]
async fn keychain_lock_before_verification_retains_code_and_compensates() {
    let x = ConnectedFixture::new().await;
    x.installed().await;
    x.data.lock_after_first_read.store(true, Ordering::SeqCst);
    let op =
        x.f.host
            .connect(x.request(), Some("verification-keychain-lock"))
            .await
            .unwrap();
    assert_eq!(
        x.f.complete(&op).await.error_code,
        Some(Code::KeychainLocked)
    );
    assert_eq!(x.keys()[0]["status"], "revoked");
    assert!(x.data.entries.lock().unwrap().is_empty());
    x.f.assert_clean();
    x.assert_safe();
}

/// ADR-038 D1 migration: a receipt pointing at the legacy tree keeps working until
/// a new copy under the managed root has verified.
///
/// Lays the fixture release out like a real legacy install: `<root>/0.4.0` and
/// `<root>/cache/<asset>.verified`, with the receipt pointing at them.
async fn legacy_fixture() -> (ConnectedFixture, PathBuf, Installation) {
    let x = ConnectedFixture::new().await;
    let managed = x.f.host.root.join("managed");
    let legacy = legacy_receipt(&x).await;
    let mut x = x;
    Arc::get_mut(&mut x.f.host).unwrap().install_root = managed.clone();
    (x, managed, legacy)
}
async fn legacy_receipt(x: &ConnectedFixture) -> Installation {
    let root = &x.f.host.root;
    let release = root.join("0.4.0");
    copy_release(&x.f.record.release_dir, &release);
    let cache = root.join("cache");
    fs::create_dir_all(&cache).unwrap();
    mode(&cache, 0o700);
    let archive = cache.join("idoris-v0.4.0-aarch64-apple-darwin.tar.gz.verified");
    fs::copy(&x.f.record.archive_path, &archive).unwrap();
    let mut record = x.f.record.clone();
    record.release_dir = release;
    record.archive_path = archive;
    record.key_id = None;
    record.components_dir = None;
    record.desired_running = false;
    x.f.host.record_installation(record.clone()).await.unwrap();
    record
}
async fn run_install(x: &ConnectedFixture, key: &str) {
    let op =
        x.f.host
            .install(InstallRequest { consent: true }, Some(key))
            .await
            .unwrap();
    assert_eq!(x.f.complete(&op).await.result, Some(Outcome::Succeeded));
}
fn deletable_paths(x: &ConnectedFixture) -> Vec<PathBuf> {
    let body = fs::read(x.f.host.root.join("legacy-install.deletable.json")).unwrap();
    let marker: serde_json::Value = serde_json::from_slice(&body).unwrap();
    serde_json::from_value(marker["paths"].clone()).unwrap()
}

#[tokio::test]
async fn legacy_install_is_redownloaded_into_the_managed_root_and_marked_deletable() {
    let (x, managed, legacy) = legacy_fixture().await;
    let status = x.f.host.status().await;
    assert!(status.migration_pending && status.last_migration_error.is_none());
    run_install(&x, "migrate-1").await;
    // Downloaded and installed under the managed root, never the legacy one.
    assert_eq!(x.provision.downloads.load(Ordering::SeqCst), 1);
    assert_eq!(
        *x.provision.install_roots.lock().unwrap(),
        std::slice::from_ref(&managed)
    );
    let record = x.f.host.inner.lock().await.installation.clone().unwrap();
    assert_eq!(record.release_dir, managed.join("v0.4.0"));
    assert_eq!(record.install_root.as_ref(), Some(&managed));
    let saved: Installation =
        serde_json::from_slice(&fs::read(x.f.host.root.join("installation.json")).unwrap())
            .unwrap();
    assert_eq!(saved.release_dir, record.release_dir);
    // The old tree is untouched and only *marked* deletable: exactly the release and archive.
    assert!(legacy.release_dir.join("idoris").exists());
    assert_eq!(
        deletable_paths(&x),
        [legacy.release_dir.clone(), legacy.archive_path.clone()]
    );
    assert!(!x.f.host.status().await.migration_pending);
    // Once migrated, a further install is a no-op verification.
    run_install(&x, "migrate-2").await;
    assert_eq!(x.provision.downloads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_migration_keeps_the_working_legacy_install_and_reports_it() {
    let (x, managed, legacy) = legacy_fixture().await;
    x.provision.fail_install.store(true, Ordering::SeqCst);
    run_install(&x, "migrate-fail").await;
    assert_eq!(x.provision.install_roots.lock().unwrap().len(), 1);
    let record = x.f.host.inner.lock().await.installation.clone().unwrap();
    assert_eq!(
        record.release_dir, legacy.release_dir,
        "receipt must still name the legacy copy"
    );
    assert!(!x.f.host.root.join("legacy-install.deletable.json").exists());
    assert!(!managed.join("v0.4.0").exists());
    let status = x.f.host.status().await;
    assert!(status.last_error.is_none());
    assert!(status.migration_pending);
    assert_eq!(status.last_migration_error, Some(Code::NotInstalled));
    // A later successful attempt clears the reported error.
    x.provision.fail_install.store(false, Ordering::SeqCst);
    run_install(&x, "migrate-retry").await;
    let status = x.f.host.status().await;
    assert!(!status.migration_pending && status.last_migration_error.is_none());
}

#[tokio::test]
async fn symlinked_data_dir_neither_remigrates_nor_lists_the_in_use_release() {
    let x = ConnectedFixture::new().await;
    let legacy = legacy_receipt(&x).await;
    let tmp = tempfile::tempdir().unwrap();
    let real = fs::canonicalize(tmp.path()).unwrap().join("real");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, tmp.path().join("link")).unwrap();
    let managed =
        crate::idoris_host::canonical_install_root(&tmp.path().join("link/components/idoris"))
            .unwrap();
    assert!(managed.starts_with(&real), "{managed:?} is not canonical");
    let mut x = x;
    Arc::get_mut(&mut x.f.host).unwrap().install_root = managed.clone();
    run_install(&x, "link-1").await;
    run_install(&x, "link-2").await;
    run_install(&x, "link-3").await;
    // One migration, then verification only: the receipt is canonical, so is the root.
    assert_eq!(x.provision.downloads.load(Ordering::SeqCst), 1);
    let record = x.f.host.inner.lock().await.installation.clone().unwrap();
    assert!(record.release_dir.starts_with(&managed));
    let listed = deletable_paths(&x);
    assert_eq!(listed, [legacy.release_dir, legacy.archive_path]);
    assert!(listed.iter().all(|p| !p.starts_with(&real)));
}

#[tokio::test]
async fn a_receipt_from_another_install_root_is_migrated_again_without_listing_it() {
    let (x, managed, _) = legacy_fixture().await;
    run_install(&x, "root-1").await;
    {
        let mut i = x.f.host.inner.lock().await;
        i.installation.as_mut().unwrap().install_root = Some("/elsewhere/idoris".into());
    }
    assert!(x.f.host.status().await.migration_pending);
    run_install(&x, "root-2").await;
    assert_eq!(x.provision.downloads.load(Ordering::SeqCst), 2);
    let record = x.f.host.inner.lock().await.installation.clone().unwrap();
    assert_eq!(record.install_root.as_ref(), Some(&managed));
    // The superseded release was under the managed root, not the legacy one: not listed.
    assert!(deletable_paths(&x).iter().all(|p| !p.starts_with(&managed)));
}

#[test]
fn deletable_allow_list_accepts_only_release_dirs_and_verified_archives() {
    let tmp = tempfile::tempdir().unwrap();
    let old = fs::canonicalize(tmp.path()).unwrap();
    for dir in [
        "0.4.0",
        "cache",
        "data",
        "instances",
        "transactions",
        "latest",
        "0.4.0/config",
        "01.2.3",
    ] {
        fs::create_dir_all(old.join(dir)).unwrap();
    }
    for file in [
        "installation.json",
        "operations.json",
        "cache/a.tar.gz.verified",
        "cache/a.tar.gz.part",
        "cache/.verified",
        "cache/.hidden.verified",
    ] {
        fs::write(old.join(file), b"x").unwrap();
    }
    std::os::unix::fs::symlink(old.join("data"), old.join("0.5.0")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(outside.path().join("0.4.0")).unwrap();
    let ok = [old.join("0.4.0"), old.join("cache/a.tar.gz.verified")];
    let refused = [
        old.clone(),
        old.join("data"),
        old.join("instances"),
        old.join("transactions"),
        old.join("installation.json"),
        old.join("operations.json"),
        old.join("latest"),
        old.join("01.2.3"),
        old.join("0.4.0/config"),
        old.join("0.5.0"), // symlink to data/
        old.join("cache"),
        old.join("cache/a.tar.gz.part"),
        old.join("cache/.verified"),
        old.join("cache/.hidden.verified"),
        old.join("cache/../data"),
        old.join("0.4.0/../data"),
        outside.path().join("0.4.0"),
        PathBuf::from("/"),
        PathBuf::new(),
    ];
    for path in &refused {
        assert!(
            crate::idoris_host::provision::legacy_deletable(&old, &[path.as_path()]).is_empty(),
            "{path:?} must never be deletable"
        );
    }
    let all: Vec<&Path> = ok
        .iter()
        .chain(refused.iter())
        .map(PathBuf::as_path)
        .collect();
    assert_eq!(
        crate::idoris_host::provision::legacy_deletable(&old, &all),
        ok
    );
    // An unreadable old root yields nothing.
    assert!(
        crate::idoris_host::provision::legacy_deletable(&old.join("absent"), &[ok[0].as_path()])
            .is_empty()
    );
}

#[tokio::test]
async fn install_without_a_notarization_record_fails_closed_when_gatekeeper_is_offline() {
    let x = ConnectedFixture::new().await;
    x.f.install().await; // receipt without notarized_verified_at
    x.provision.signature.0.store(true, Ordering::SeqCst);
    let op =
        x.f.host
            .install(InstallRequest { consent: true }, Some("offline-1"))
            .await
            .unwrap();
    let done = x.f.complete(&op).await;
    assert_eq!(done.result, Some(Outcome::Failed));
    assert!(
        x.f.host
            .inner
            .lock()
            .await
            .installation
            .as_ref()
            .unwrap()
            .notarized_verified_at
            .is_none()
    );
}

#[tokio::test]
async fn install_with_a_notarization_record_is_intact_offline() {
    let x = ConnectedFixture::new().await;
    let mut record = x.f.record.clone();
    record.notarized_verified_at = Some("2026-11-02T00:00:00+00:00".into());
    x.f.host.record_installation(record).await.unwrap();
    x.provision.signature.0.store(true, Ordering::SeqCst);
    run_install(&x, "offline-2").await;
    assert_eq!(x.provision.downloads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_online_install_stamps_the_receipt_once() {
    let x = ConnectedFixture::new().await;
    x.f.install().await;
    run_install(&x, "online-1").await;
    assert_eq!(x.provision.downloads.load(Ordering::SeqCst), 0);
    let stamped =
        x.f.host
            .inner
            .lock()
            .await
            .installation
            .as_ref()
            .unwrap()
            .notarized_verified_at
            .clone()
            .expect("full check passed, receipt must record it");
    let saved: Installation =
        serde_json::from_slice(&fs::read(x.f.host.root.join("installation.json")).unwrap())
            .unwrap();
    assert_eq!(
        saved.notarized_verified_at.as_deref(),
        Some(stamped.as_str())
    );
    // The next install trusts the record: at-rest checks only, even with Gatekeeper gone.
    x.provision.signature.0.store(true, Ordering::SeqCst);
    run_install(&x, "online-2").await;
    assert_eq!(x.provision.downloads.load(Ordering::SeqCst), 0);
}
