#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use agent24_idoris::install::{InstallError, SignatureVerifier};
use flate2::{Compression, write::GzEncoder};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicBool, Ordering},
};

struct MemoryKeychain {
    locked: AtomicBool,
}
impl Keychain for MemoryKeychain {
    fn get(&self, _: &str) -> Result<Zeroizing<String>, KeychainError> {
        if self.locked.load(Ordering::SeqCst) {
            Err(KeychainError::KeychainLocked)
        } else {
            Ok(Zeroizing::new(format!("idk_{}", "a".repeat(64))))
        }
    }
    fn put(&self, _: &str, _: &str) -> Result<(), KeychainError> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<(), KeychainError> {
        Ok(())
    }
}
struct FixtureRuntime {
    lock: IdorisLock,
    gate: AtomicBool,
}
struct AcceptSignature;
impl SignatureVerifier for AcceptSignature {
    fn verify(
        &self,
        _: &Path,
        _: &agent24_idoris::install::SigningRequirement,
    ) -> Result<(), InstallError> {
        Ok(())
    }
}
#[async_trait]
impl Runtime for FixtureRuntime {
    async fn config(
        &self,
        root: &Path,
        r: &Installation,
        cancel: CancellationToken,
    ) -> Result<StartConfig, Code> {
        while self.gate.load(Ordering::SeqCst) {
            tokio::select! { _ = cancel.cancelled() => return Err(Code::Cancelled), _ = tokio::time::sleep(Duration::from_millis(5)) => {} }
        }
        let installed = agent24_idoris::install::verify_existing(
            &r.release_dir,
            &self.lock,
            &VerifiedArchive {
                path: r.archive_path.clone(),
                sha256: String::new(),
            },
            &AcceptSignature,
        )
        .unwrap();
        let mut config = StartConfig::new(
            installed,
            ComponentsDir::new(root, r.components_dir.as_ref().unwrap()).unwrap(),
            DataPaths {
                db: root.join("data/idoris.sqlite3"),
                state: root.join("data/state"),
            },
        )
        .unwrap();
        config.ports = Ports {
            data: r.data_port,
            admin: r.admin_port,
        };
        config.log_dir = root.join("logs");
        config.shutdown = cancel;
        config.timings.ready = Duration::from_secs(3);
        config.timings.poll = Duration::from_millis(10);
        config.timings.terminate = Duration::from_millis(100);
        config.timings.drain = Duration::from_millis(100);
        Ok(config)
    }
    async fn inputs(&self, _: &Installation) -> Result<LaunchInputs, Code> {
        Ok(LaunchInputs {
            budget: Budget {
                allocated_bytes: 8_000_000_000,
            },
            omlx_key: Some(Zeroizing::new("sk-upstream-sensitive".into())),
        })
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    host: Arc<IdorisHost>,
    record: Installation,
    runtime: Arc<FixtureRuntime>,
    keychain: Arc<MemoryKeychain>,
}
fn mode(path: &Path, permissions: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(permissions)).unwrap();
}
async fn port() -> u16 {
    tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for part in ["release/config/components", "components", "data/state"] {
            fs::create_dir_all(root.join(part)).unwrap();
        }
        let output = std::process::Command::new("/usr/bin/python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let script = include_str!("../../../../../crates/agent24-idoris/tests/fixtures/idoris.py")
            .replace(
                "root = Path(os.environ['IDORIS_COMPONENTS_DIR'])",
                &format!(
                    "{}\nroot = Path(os.environ['IDORIS_COMPONENTS_DIR'])",
                    include_str!("fixtures/provision.py")
                ),
            )
            .replacen(
                "#!/usr/bin/python3",
                &format!("#!{}", String::from_utf8(output.stdout).unwrap().trim()),
                1,
            );
        let mut lock = IdorisLock::embedded().clone();
        let files = &lock.assets["aarch64-apple-darwin"].files;
        let archive_path = root.join("fixture.tar.gz");
        let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for name in files {
            let data = if name == "idoris" {
                script.as_bytes()
            } else {
                b"# fixture\n"
            };
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            archive.append_data(&mut header, name, data).unwrap();
            let path = root.join("release").join(name);
            fs::write(&path, data).unwrap();
            mode(&path, if name == "idoris" { 0o500 } else { 0o400 });
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        lock.assets.get_mut("aarch64-apple-darwin").unwrap().sha256 =
            format!("{:x}", Sha256::digest(&bytes));
        fs::write(&archive_path, bytes).unwrap();
        for part in ["release", "release/config", "release/config/components"] {
            mode(&root.join(part), 0o700);
        }
        fs::write(root.join("components/scenario.json"), "{}").unwrap();
        let record = Installation {
            version: "0.4.0".into(),
            release_dir: root.join("release"),
            archive_path,
            components_dir: Some(root.join("components")),
            key_id: Some("vk_fixture".into()),
            data_port: port().await,
            admin_port: port().await,
            roles: vec![Role::Daily],
            core_reserved_bytes: Some(0),
            omlx_url: None,
            omlx_key_id: None,
            desired_running: true,
            verification: None,
            install_root: None,
        };
        let runtime = Arc::new(FixtureRuntime {
            lock,
            gate: AtomicBool::new(false),
        });
        let keychain = Arc::new(MemoryKeychain {
            locked: AtomicBool::new(false),
        });
        let router = Arc::new(ModelRouter::idoris_unavailable("test"));
        let managed =
            Arc::new(ManagedIdoris::new(router.clone(), root.join("connection.json")).unwrap());
        let host = IdorisHost::open(
            root,
            managed,
            router,
            runtime.clone(),
            Arc::new(KeychainSource(keychain.clone())),
        )
        .unwrap();
        Self {
            _dir: dir,
            host,
            record,
            runtime,
            keychain,
        }
    }
    async fn install(&self) {
        self.host
            .record_installation(self.record.clone())
            .await
            .unwrap();
    }
    fn scenario(&self, value: serde_json::Value) {
        fs::write(
            self.host.root.join("components/scenario.json"),
            value.to_string(),
        )
        .unwrap();
    }
    async fn complete(&self, op: &Operation) -> Operation {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let value = self.host.operation(&op.operation_id).await.unwrap();
                if value.result.is_some() {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }
    fn assert_clean(&self) {
        assert!(
            !self
                .host
                .root
                .join("data/state/managed-process.json")
                .exists()
        );
    }
    fn assert_no_secrets(&self) {
        for name in ["installation.json", "operations.json", "connection.json"] {
            let body = fs::read_to_string(self.host.root.join(name)).unwrap_or_default();
            for secret in ["idk_", "sk-upstream", "Bearer "] {
                assert!(!body.contains(secret), "{name}");
            }
        }
    }
}

#[tokio::test]
async fn state_machine_idempotency_busy_and_stop() {
    let f = Fixture::new().await;
    assert_eq!(f.host.status().await.state, State::NotInstalled);
    assert!(matches!(
        f.host.submit(Action::Start, None).await,
        Err(Code::NotInstalled)
    ));
    let mut incomplete = f.record.clone();
    incomplete.components_dir = None;
    f.host.record_installation(incomplete).await.unwrap();
    assert_eq!(f.host.status().await.state, State::NotConfigured);
    assert!(matches!(
        f.host.submit(Action::Start, None).await,
        Err(Code::NotConfigured)
    ));
    f.install().await;
    assert_eq!(f.host.status().await.state, State::Stopped);
    f.runtime.gate.store(true, Ordering::SeqCst);
    let start = f
        .host
        .submit(Action::Start, Some("start-once"))
        .await
        .unwrap();
    assert_eq!(
        f.host
            .submit(Action::Start, Some("start-once"))
            .await
            .unwrap()
            .operation_id,
        start.operation_id
    );
    assert!(matches!(
        f.host.submit(Action::Start, None).await,
        Err(Code::Busy)
    ));
    assert!(matches!(
        f.host.submit(Action::Stop, None).await,
        Err(Code::Busy)
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.host.status().await.state != State::Starting {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.runtime.gate.store(false, Ordering::SeqCst);
    assert_eq!(f.complete(&start).await.result, Some(Outcome::Succeeded));
    assert_eq!(f.host.status().await.state, State::Running);
    let repeated = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(repeated.result, Some(Outcome::Succeeded));
    let status = serde_json::to_string(&f.host.status().await).unwrap();
    for secret in ["idk_", "sk-", "token", "key_id", "admin_token"] {
        assert!(!status.contains(secret));
    }
    let stop = f
        .host
        .submit(Action::Stop, Some("stop-once"))
        .await
        .unwrap();
    assert_eq!(f.complete(&stop).await.result, Some(Outcome::Succeeded));
    assert_eq!(f.host.status().await.state, State::Stopped);
    assert_eq!(
        f.host.submit(Action::Stop, None).await.unwrap().result,
        Some(Outcome::Succeeded)
    );
    assert!(f.host.router.idoris_gateway().is_err());
    let attempts = fs::read_to_string(f.host.root.join("data/state/attempts.jsonl")).unwrap();
    assert_eq!(attempts.lines().count(), 1);
    f.assert_clean();
    f.assert_no_secrets();
}

#[tokio::test]
async fn keychain_locked_is_stable_without_plaintext_or_spawn() {
    let f = Fixture::new().await;
    f.install().await;
    f.keychain.locked.store(true, Ordering::SeqCst);
    let op = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(f.complete(&op).await.error_code, Some(Code::KeychainLocked));
    assert_eq!(f.host.status().await.last_error, Some(Code::KeychainLocked));
    assert!(!f.host.root.join("data/state/attempts.jsonl").exists());
    f.assert_no_secrets();
}

#[tokio::test]
async fn l4_and_contract_failures_are_mapped_and_reaped() {
    for (scenario, code, state) in [
        (
            json!({"mode":"exit", "code":3}),
            Code::InstanceConflict,
            State::InstanceConflict,
        ),
        (
            json!({"contract":"2.0.0"}),
            Code::ContractMismatch,
            State::Failed,
        ),
        (
            json!({"mode":"exit", "code":1}),
            Code::StartFailed,
            State::Failed,
        ),
    ] {
        let f = Fixture::new().await;
        f.install().await;
        f.scenario(scenario);
        let op = f.host.submit(Action::Start, None).await.unwrap();
        assert_eq!(f.complete(&op).await.error_code, Some(code));
        assert_eq!(f.host.status().await.state, state);
        f.assert_clean();
    }
}

#[tokio::test]
async fn cancel_and_shutdown_wait_for_process_reaping() {
    let f = Fixture::new().await;
    f.install().await;
    f.runtime.gate.store(true, Ordering::SeqCst);
    let op = f.host.submit(Action::Start, None).await.unwrap();
    f.host.cancel(&op.operation_id).await.unwrap();
    assert_eq!(f.complete(&op).await.result, Some(Outcome::Cancelled));
    f.assert_clean();
    f.runtime.gate.store(false, Ordering::SeqCst);
    let op = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(f.complete(&op).await.result, Some(Outcome::Succeeded));
    let shutdown = CancellationToken::new();
    let stopping = f.host.stop_on_shutdown(shutdown.clone());
    shutdown.cancel();
    stopping.await.unwrap().unwrap();
    f.assert_clean();
    assert_eq!(f.host.status().await.state, State::Stopped);
    assert!(matches!(
        f.host.submit(Action::Start, None).await,
        Err(Code::ShuttingDown)
    ));
    f.assert_no_secrets();
}

#[tokio::test]
async fn shutdown_during_start_closes_admission_and_cancels() {
    let f = Fixture::new().await;
    f.install().await;
    f.runtime.gate.store(true, Ordering::SeqCst);
    let op = f.host.submit(Action::Start, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), f.host.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.complete(&op).await.result, Some(Outcome::Cancelled));
    f.assert_clean();
}

#[tokio::test]
async fn forced_stop_is_reported_and_does_not_leave_a_process() {
    let f = Fixture::new().await;
    f.install().await;
    f.scenario(json!({"mode":"ignore_term"}));
    let op = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(f.complete(&op).await.result, Some(Outcome::Succeeded));
    let stop = f.host.submit(Action::Stop, None).await.unwrap();
    assert_eq!(f.complete(&stop).await.error_code, Some(Code::StopTimeout));
    f.assert_clean();
}

#[tokio::test]
async fn restore_replays_no_completed_operation_and_honors_stop_intent() {
    let f = Fixture::new().await;
    f.install().await;
    let start = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(f.complete(&start).await.result, Some(Outcome::Succeeded));
    let attempts = fs::read(f.host.root.join("data/state/attempts.jsonl")).unwrap();
    let stop = f
        .host
        .submit(Action::Stop, Some("persistent-stop"))
        .await
        .unwrap();
    assert_eq!(f.complete(&stop).await.result, Some(Outcome::Succeeded));
    let restored = IdorisHost::open(
        f.host.root.clone(),
        f.host.managed.clone(),
        f.host.router.clone(),
        f.runtime.clone(),
        f.host.secrets.clone(),
    )
    .unwrap();
    restored.restore().await.unwrap();
    assert!(restored.inner.lock().await.active.is_none());
    assert_eq!(restored.status().await.state, State::Stopped);
    assert_eq!(
        fs::read(f.host.root.join("data/state/attempts.jsonl")).unwrap(),
        attempts
    );
    assert_eq!(
        restored.operation(&stop.operation_id).await.unwrap().result,
        Some(Outcome::Succeeded)
    );
    assert_eq!(
        restored
            .submit(Action::Stop, Some("persistent-stop"))
            .await
            .unwrap()
            .operation_id,
        stop.operation_id
    );
}

#[tokio::test]
async fn leftover_identity_conflict_is_preserved_without_spawning() {
    let owner = Fixture::new().await;
    owner.install().await;
    let started = owner.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(
        owner.complete(&started).await.result,
        Some(Outcome::Succeeded)
    );
    let mut record: serde_json::Value = serde_json::from_slice(
        &fs::read(owner.host.root.join("data/state/managed-process.json")).unwrap(),
    )
    .unwrap();
    let f = Fixture::new().await;
    f.install().await;
    // Keep the real live PID, start time and UID. A fabricated start time
    // represents a reused PID, which IL-07 now safely clears as stale.
    record["executable"] = json!(f.record.release_dir.join("foreign-idoris"));
    let path = f.host.root.join("data/state/managed-process.json");
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    mode(&path, 0o600);
    let op = f.host.submit(Action::Start, None).await.unwrap();
    let completed = f.complete(&op).await;
    owner.host.shutdown().await.unwrap();
    assert_eq!(completed.error_code, Some(Code::InstanceConflict));
    assert_eq!(f.host.status().await.state, State::InstanceConflict);
    assert!(path.exists());
    assert!(!f.host.root.join("data/state/attempts.jsonl").exists());
}

#[tokio::test]
async fn terminal_child_failure_closes_gateway_and_updates_status() {
    let f = Fixture::new().await;
    f.install().await;
    f.scenario(json!({"mode":"exit_after_ready", "after":0.3, "code":3}));
    let op = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(f.complete(&op).await.result, Some(Outcome::Succeeded));
    tokio::time::timeout(Duration::from_secs(3), async {
        while f.host.status().await.state != State::InstanceConflict {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(f.host.router.idoris_gateway().is_err());
    f.assert_clean();
}

#[tokio::test]
async fn stop_blocks_new_gateway_requests_and_drains_retained_snapshot() {
    let f = Fixture::new().await;
    f.install().await;
    let op = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(f.complete(&op).await.result, Some(Outcome::Succeeded));
    let old_request = f.host.router.idoris_gateway().unwrap();
    let stop = f.host.submit(Action::Stop, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.host.router.idoris_gateway().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        f.host
            .operation(&stop.operation_id)
            .await
            .unwrap()
            .result
            .is_none()
    );
    assert!(f.host.root.join("data/state/managed-process.json").exists());
    drop(old_request);
    assert_eq!(f.complete(&stop).await.result, Some(Outcome::Succeeded));
    f.assert_clean();
}

struct SmokeRuntime {
    system: SystemRuntime,
}
#[async_trait]
impl Runtime for SmokeRuntime {
    async fn config(
        &self,
        root: &Path,
        record: &Installation,
        cancel: CancellationToken,
    ) -> Result<StartConfig, Code> {
        let mut config = self.system.config(root, record, cancel).await?;
        config.log_dir = root.join("logs");
        Ok(config)
    }
    async fn inputs(&self, _: &Installation) -> Result<LaunchInputs, Code> {
        // The smoke test checks lifecycle only, with an explicit allocation.
        Ok(LaunchInputs {
            budget: Budget {
                allocated_bytes: 8_000_000_000,
            },
            omlx_key: None,
        })
    }
}

#[tokio::test]
#[ignore = "requires IDORIS_REAL_V040_DIR; verifies signed v0.4.0 and runs real serve"]
async fn real_v040_install_existing_start_running_stop() {
    use agent24_idoris::{
        download::{DownloadOptions, download_locked_asset},
        install::{InstallOptions, install_locked_asset},
    };
    let release =
        PathBuf::from(std::env::var_os("IDORIS_REAL_V040_DIR").expect("set release directory"));
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap().join("idoris");
    fs::create_dir(&root).unwrap();
    mode(&root, 0o700);
    let lock = IdorisLock::embedded();
    let cancel = CancellationToken::new();
    let archive = download_locked_asset(
        lock,
        "aarch64-apple-darwin",
        &root.join("cache"),
        &DownloadOptions::default(),
        &cancel,
    )
    .await
    .unwrap();
    let installed_dir = root.join("releases/0.4.0");
    for part in [
        "releases/0.4.0/config/components",
        "components",
        "data/state",
    ] {
        fs::create_dir_all(root.join(part)).unwrap();
    }
    for name in &lock.assets["aarch64-apple-darwin"].files {
        let target = installed_dir.join(name);
        fs::copy(release.join(name), &target).unwrap();
        mode(&target, if name == "idoris" { 0o500 } else { 0o400 });
    }
    for part in [
        "releases",
        "releases/0.4.0",
        "releases/0.4.0/config",
        "releases/0.4.0/config/components",
    ] {
        mode(&root.join(part), 0o700);
    }
    let installed = install_locked_asset(
        lock,
        "aarch64-apple-darwin",
        &archive,
        &InstallOptions::new(root.join("releases"), std::env::current_exe().unwrap()),
        Codesign,
        &cancel,
    )
    .await
    .unwrap();
    fs::copy(
        release.join("config/components/omlx.yaml"),
        root.join("components/omlx.yaml"),
    )
    .unwrap();
    let secrets: Arc<dyn SecretSource> = Arc::new(KeychainSource(Arc::new(MemoryKeychain {
        locked: AtomicBool::new(false),
    })));
    let router = Arc::new(ModelRouter::idoris_unavailable("smoke"));
    let managed =
        Arc::new(ManagedIdoris::new(router.clone(), root.join("connection.json")).unwrap());
    let host = IdorisHost::open(
        root.clone(),
        managed,
        router,
        Arc::new(SmokeRuntime {
            system: SystemRuntime {
                secrets: secrets.clone(),
            },
        }),
        secrets,
    )
    .unwrap();
    host.record_installation(Installation {
        version: "0.4.0".into(),
        release_dir: installed.directory().into(),
        archive_path: archive.path,
        components_dir: Some(root.join("components")),
        key_id: Some("vk_smoke".into()),
        data_port: port().await,
        admin_port: port().await,
        roles: vec![],
        core_reserved_bytes: Some(0),
        omlx_url: None,
        omlx_key_id: None,
        desired_running: true,
        verification: None,
        install_root: None,
    })
    .await
    .unwrap();
    async fn complete(host: &IdorisHost, id: &str) -> Operation {
        tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                let op = host.operation(id).await.unwrap();
                if op.result.is_some() {
                    return op;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    eprintln!(
        "SMOKE: upstream component only; oMLX setup/model bindings and inference are skipped; memory data key is used for public health only"
    );
    let start = host.submit(Action::Start, None).await.unwrap();
    let started = complete(&host, &start.operation_id).await;
    assert_eq!(
        started.result,
        Some(Outcome::Succeeded),
        "{:?}",
        started.error_code
    );
    assert_eq!(host.status().await.state, State::Running);
    let stop = host.submit(Action::Stop, None).await.unwrap();
    assert_eq!(
        complete(&host, &stop.operation_id).await.result,
        Some(Outcome::Succeeded)
    );
    assert_eq!(host.status().await.state, State::Stopped);
    assert!(!root.join("data/state/managed-process.json").exists());
}

#[tokio::test]
async fn restore_unconfigured_installation_keeps_daemon_available() {
    let f = Fixture::new().await;
    let mut record = f.record.clone();
    record.key_id = None;
    f.host.record_installation(record).await.unwrap();
    f.host.restore().await.unwrap();
    assert_eq!(f.host.status().await.state, State::NotConfigured);
    assert!(f.host.status().await.current_operation.is_none());
}

#[tokio::test]
async fn stop_before_install_creates_private_metadata_directory() {
    let f = Fixture::new().await;
    let root = f.host.root.join("new-instance");
    let host = IdorisHost::open(
        root.clone(),
        f.host.managed.clone(),
        f.host.router.clone(),
        f.runtime.clone(),
        f.host.secrets.clone(),
    )
    .unwrap();
    let op = host.submit(Action::Stop, None).await.unwrap();
    assert_eq!(op.result, Some(Outcome::Succeeded));
    assert_eq!(
        fs::metadata(root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(host.status().await.state, State::NotInstalled);
}

#[tokio::test]
async fn receipt_rejects_plaintext_in_a_credential_reference() {
    let f = Fixture::new().await;
    let mut record = f.record.clone();
    record.key_id = Some(format!("idk_{}", "a".repeat(64)));
    assert_eq!(
        f.host.record_installation(record).await,
        Err(Code::InvalidRequest)
    );
    assert!(!f.host.root.join("installation.json").exists());
}

#[tokio::test]
async fn start_during_supervisor_restart_is_busy_and_stop_wins_readiness_updates() {
    let f = Fixture::new().await;
    f.install().await;
    f.scenario(json!({"mode":"crash", "after":0.3}));
    let op = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(f.complete(&op).await.result, Some(Outcome::Succeeded));
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.host.status().await.state != State::Starting {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        f.host.submit(Action::Start, None).await,
        Err(Code::Busy)
    ));
    let stop = f.host.submit(Action::Stop, None).await.unwrap();
    assert_eq!(f.complete(&stop).await.result, Some(Outcome::Succeeded));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.host.status().await.state, State::Stopped);
    f.assert_clean();
}

#[test]
fn lifecycle_wire_fixtures_match_serde_and_openapi() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let openapi: serde_json::Value =
        serde_yaml::from_str(&fs::read_to_string(root.join("protocol/openapi.yaml")).unwrap())
            .unwrap();
    for (file, schema) in [
        ("status", "IdorisStatus"),
        ("operation", "IdorisOperation"),
        ("operation-failed", "IdorisOperation"),
    ] {
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join(format!("protocol/fixtures/idoris/{file}.json"))).unwrap(),
        )
        .unwrap();
        let roundtrip = if file == "status" {
            serde_json::to_value(serde_json::from_value::<Status>(value.clone()).unwrap()).unwrap()
        } else {
            // On disk the record keeps its private retry fields; the API
            // projection drops them (N1).
            let mut stored = value.clone();
            stored["idempotency_key"] = json!("idk-private");
            stored["request_fingerprint"] = json!("f".repeat(64));
            let op = serde_json::from_value::<Operation>(stored.clone()).unwrap();
            assert_eq!(serde_json::to_value(&op).unwrap(), stored);
            serde_json::to_value(op.view()).unwrap()
        };
        assert_eq!(roundtrip, value);
        let validator = jsonschema::options().offline().build(&json!({
            "$ref": format!("#/components/schemas/{schema}"), "components": openapi["components"]
        })).unwrap();
        assert!(validator.is_valid(&value), "{file}");
        let mut invalid = value.clone();
        invalid[if file == "status" {
            "state"
        } else {
            "error_code"
        }] = json!("unrecognized");
        assert!(!validator.is_valid(&invalid));
        let body = serde_json::to_string(&value).unwrap();
        assert!(!body.contains("idk_"));
        assert!(!body.contains("sk-"));
        assert!(!body.contains("token"));
    }
}

#[tokio::test]
async fn stop_after_failed_start_reaches_stopped_without_a_process() {
    let f = Fixture::new().await;
    f.install().await;
    f.keychain.locked.store(true, Ordering::SeqCst);
    let start = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(
        f.complete(&start).await.error_code,
        Some(Code::KeychainLocked)
    );
    let stop = f.host.submit(Action::Stop, None).await.unwrap();
    assert_eq!(stop.result, Some(Outcome::Succeeded));
    assert_eq!(f.host.status().await.state, State::Stopped);
    f.assert_clean();
}

#[tokio::test]
async fn cancelled_start_does_not_autostart_on_daemon_restart() {
    let f = Fixture::new().await;
    f.install().await;
    f.runtime.gate.store(true, Ordering::SeqCst);
    let start = f.host.submit(Action::Start, None).await.unwrap();
    f.host.cancel(&start.operation_id).await.unwrap();
    assert_eq!(f.complete(&start).await.result, Some(Outcome::Cancelled));
    let record: Installation =
        serde_json::from_slice(&fs::read(f.host.root.join("installation.json")).unwrap()).unwrap();
    assert!(!record.desired_running);
    let restored = IdorisHost::open(
        f.host.root.clone(),
        f.host.managed.clone(),
        f.host.router.clone(),
        f.runtime.clone(),
        f.host.secrets.clone(),
    )
    .unwrap();
    restored.restore().await.unwrap();
    assert_eq!(restored.status().await.state, State::Stopped);
    assert!(restored.status().await.current_operation.is_none());
}

#[tokio::test]
async fn keychain_lock_after_readiness_retains_the_stable_error_code() {
    use std::sync::atomic::AtomicUsize;
    struct LocksAfterFirstRead(AtomicUsize);
    #[async_trait]
    impl SecretSource for LocksAfterFirstRead {
        async fn secret(&self, _: &str) -> Result<Zeroizing<String>, SecretError> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(Zeroizing::new(format!("idk_{}", "a".repeat(64))))
            } else {
                Err(SecretError::Locked)
            }
        }
    }
    let mut f = Fixture::new().await;
    f.host = IdorisHost::open(
        f.host.root.clone(),
        f.host.managed.clone(),
        f.host.router.clone(),
        f.runtime.clone(),
        Arc::new(LocksAfterFirstRead(AtomicUsize::new(0))),
    )
    .unwrap();
    f.install().await;
    let start = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(
        f.complete(&start).await.error_code,
        Some(Code::KeychainLocked)
    );
    assert!(f.host.router.idoris_gateway().is_err());
    f.assert_clean();
    f.assert_no_secrets();
}

#[tokio::test]
async fn cancel_interrupts_a_pending_keychain_read_before_spawn() {
    struct PendingKeychain;
    #[async_trait]
    impl SecretSource for PendingKeychain {
        async fn secret(&self, _: &str) -> Result<Zeroizing<String>, SecretError> {
            std::future::pending().await
        }
    }
    let mut f = Fixture::new().await;
    f.host = IdorisHost::open(
        f.host.root.clone(),
        f.host.managed.clone(),
        f.host.router.clone(),
        f.runtime.clone(),
        Arc::new(PendingKeychain),
    )
    .unwrap();
    f.install().await;
    let start = f.host.submit(Action::Start, None).await.unwrap();
    f.host.cancel(&start.operation_id).await.unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let op = f.host.operation(&start.operation_id).await.unwrap();
            if op.result.is_some() {
                return op;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancellation must interrupt a pending keychain read");
    assert_eq!(completed.result, Some(Outcome::Cancelled));
    assert!(!f.host.root.join("data/state/attempts.jsonl").exists());
    f.assert_clean();
}

mod provision;

#[tokio::test]
async fn concurrent_submits_admit_exactly_one_start() {
    let f = Fixture::new().await;
    f.install().await;
    f.runtime.gate.store(true, Ordering::SeqCst);
    let barrier = Arc::new(tokio::sync::Barrier::new(33));
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let host = f.host.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            host.submit(Action::Start, None).await
        }));
    }
    barrier.wait().await;
    let mut admitted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => admitted += 1,
            Err(Code::Busy) => {}
            Err(code) => panic!("unexpected admission error: {code:?}"),
        }
    }
    f.host.shutdown().await.unwrap();
    assert_eq!(admitted, 1);
}

#[tokio::test]
async fn rejected_start_does_not_persist_running_intent() {
    let f = Fixture::new().await;
    let mut record = f.record.clone();
    record.desired_running = false;
    f.host.record_installation(record).await.unwrap();
    // The operations destination fails after installation.json is writable.
    let path = f.host.root.join("operations.json");
    fs::create_dir(&path).unwrap();
    assert!(matches!(
        f.host.submit(Action::Start, None).await,
        Err(Code::PersistenceFailed)
    ));
    fs::remove_dir(path).unwrap();
    let restored = IdorisHost::open(
        f.host.root.clone(),
        f.host.managed.clone(),
        f.host.router.clone(),
        f.runtime.clone(),
        f.host.secrets.clone(),
    )
    .unwrap();
    restored.restore().await.unwrap();
    let launched = restored.inner.lock().await.active.is_some();
    restored.shutdown().await.unwrap();
    let record: Installation =
        serde_json::from_slice(&fs::read(f.host.root.join("installation.json")).unwrap()).unwrap();
    assert!(
        !record.desired_running,
        "rejected start left a running intent on disk"
    );
    assert!(!launched, "restore admitted a rejected start");
    assert!(
        !f.host
            .inner
            .lock()
            .await
            .installation
            .as_ref()
            .unwrap()
            .desired_running
    );
}

#[tokio::test]
async fn stale_closed_monitor_cannot_take_the_next_supervisor_or_block_status() {
    let f = Fixture::new().await;
    f.install().await;
    let start = f.host.submit(Action::Start, None).await.unwrap();
    let old_cancel = f.host.inner.lock().await.active.as_ref().unwrap().1.clone();
    assert_eq!(f.complete(&start).await.result, Some(Outcome::Succeeded));
    let old_generation = start.operation_id.clone();
    let stop = f.host.submit(Action::Stop, None).await.unwrap();
    assert_eq!(f.complete(&stop).await.result, Some(Outcome::Succeeded));
    assert!(old_cancel.is_cancelled());
    let start = f.host.submit(Action::Start, None).await.unwrap();
    let new_stop = f.host.inner.lock().await.active.as_ref().unwrap().1.clone();
    assert_eq!(f.complete(&start).await.result, Some(Outcome::Succeeded));
    let inner = f.host.inner.lock().await;
    let (sender, signal) = watch::channel(true);
    drop(sender);
    // Wake A's closed-channel monitor only after B has been installed.
    f.host.monitor(
        signal,
        f.record.clone(),
        old_generation,
        old_cancel,
        Arc::new(SyncMutex::new(None)),
    );
    tokio::task::yield_now().await;
    drop(inner);
    let status = tokio::time::timeout(Duration::from_millis(300), f.host.status()).await;
    // B must be stopped even when the regression holds inner across B.wait().
    new_stop.cancel();
    f.host.shutdown().await.unwrap();
    assert!(
        status.is_ok(),
        "stale monitor blocked status on the new supervisor"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn stop_after_reopen_reaps_a_live_leftover_without_starting() {
    let f = Fixture::new().await;
    f.install().await;
    let executable = fs::canonicalize("/bin/sleep").unwrap();
    fs::remove_file(f.record.release_dir.join("idoris")).unwrap();
    std::os::unix::fs::symlink(&executable, f.record.release_dir.join("idoris")).unwrap();
    let mut command = tokio::process::Command::new(&executable);
    command.arg("60").process_group(0).kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    let pid = child.id().unwrap();
    let info =
        libproc::libproc::proc_pid::pidinfo::<libproc::libproc::bsd_info::BSDInfo>(pid as i32, 0)
            .unwrap();
    let record = json!({"pid":pid, "uid":info.pbi_uid,
        "start_time":info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        "executable":executable});
    write_json(
        &f.host.root.join("data/state/managed-process.json"),
        &record,
    )
    .unwrap();
    // Model SIGKILL after the stop intent is durable, before any worker runs.
    let mut stopped = f.record.clone();
    stopped.desired_running = false;
    write_json(&f.host.root.join("installation.json"), &stopped).unwrap();
    let restored = IdorisHost::open(
        f.host.root.clone(),
        f.host.managed.clone(),
        f.host.router.clone(),
        f.runtime.clone(),
        f.host.secrets.clone(),
    )
    .unwrap();
    let mut reaper = tokio::spawn(async move { child.wait().await.unwrap() });
    restored.restore().await.unwrap();
    let stop = restored.submit(Action::Stop, None).await.unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let op = restored.operation(&stop.operation_id).await.unwrap();
            if op.result.is_some() {
                break op;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let exited = tokio::time::timeout(Duration::from_millis(500), &mut reaper)
        .await
        .is_ok();
    if !exited {
        reaper.abort();
        let _ = reaper.await;
    }
    assert!(exited, "Stop left the recorded child alive");
    assert_eq!(completed.result, Some(Outcome::Succeeded));
    f.assert_clean();
    assert!(!f.host.root.join("data/state/attempts.jsonl").exists());
}

#[tokio::test]
async fn stopped_generation_cannot_publish_a_gateway_after_prepare() {
    let f = Fixture::new().await;
    f.install().await;
    let start = f.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(f.complete(&start).await.result, Some(Outcome::Succeeded));
    let mut inner = f.host.inner.lock().await;
    let supervisor = inner.supervisor.take().unwrap();
    inner.generation = None;
    f.host.block_gateway();
    let (sender, signal) = watch::channel(false);
    f.host.monitor(
        signal,
        f.record.clone(),
        start.operation_id,
        CancellationToken::new(),
        Arc::new(SyncMutex::new(None)),
    );
    sender.send(true).unwrap();
    // Keep stop's lock until preparation has queued the final generation check.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            tokio::time::sleep(Duration::from_millis(5)).await;
            drop(inner);
            match f.host.inner.try_lock() {
                Ok(reacquired) => inner = reacquired,
                Err(_) => break,
            }
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), sender.closed())
        .await
        .unwrap();
    let blocked = f.host.router.idoris_gateway().is_err();
    supervisor.stop().await.unwrap();
    f.host.shutdown().await.unwrap();
    assert!(blocked, "monitor published its prepared gateway after stop");
}

#[tokio::test]
async fn stale_monitor_generation_is_rejected_even_without_cancellation() {
    let f = Fixture::new().await;
    f.install().await;
    let start = f.host.submit(Action::Start, None).await.unwrap();
    let stop_token = f.host.inner.lock().await.active.as_ref().unwrap().1.clone();
    assert_eq!(f.complete(&start).await.result, Some(Outcome::Succeeded));
    let inner = f.host.inner.lock().await;
    let (sender, signal) = watch::channel(true);
    drop(sender);
    f.host.monitor(
        signal,
        f.record.clone(),
        "retired-generation".into(),
        CancellationToken::new(),
        Arc::new(SyncMutex::new(None)),
    );
    tokio::task::yield_now().await;
    drop(inner);
    let status = tokio::time::timeout(Duration::from_millis(300), f.host.status()).await;
    stop_token.cancel();
    f.host.shutdown().await.unwrap();
    assert!(status.is_ok(), "monitor ignored its generation identity");
}

#[tokio::test]
async fn rejected_start_when_intent_write_fails_is_not_admitted_or_restored() {
    let f = Fixture::new().await;
    let mut record = f.record.clone();
    record.desired_running = false;
    f.host.record_installation(record.clone()).await.unwrap();
    let path = f.host.root.join("installation.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(matches!(
        f.host.submit(Action::Start, Some("rejected")).await,
        Err(Code::PersistenceFailed)
    ));
    assert!(f.host.inner.lock().await.active.is_none());
    assert!(f.host.inner.lock().await.operations.is_empty());
    fs::remove_dir(&path).unwrap();
    write_json(&path, &record).unwrap();
    let restored = IdorisHost::open(
        f.host.root.clone(),
        f.host.managed.clone(),
        f.host.router.clone(),
        f.runtime.clone(),
        f.host.secrets.clone(),
    )
    .unwrap();
    restored.restore().await.unwrap();
    assert!(restored.inner.lock().await.active.is_none());
    assert!(!f.host.root.join("data/state/attempts.jsonl").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn stop_preserves_an_unverified_live_leftover_without_signalling() {
    let owner = Fixture::new().await;
    owner.install().await;
    let start = owner.host.submit(Action::Start, None).await.unwrap();
    assert_eq!(
        owner.complete(&start).await.result,
        Some(Outcome::Succeeded)
    );
    let f = Fixture::new().await;
    f.install().await;
    let mut record: serde_json::Value = serde_json::from_slice(
        &fs::read(owner.host.root.join("data/state/managed-process.json")).unwrap(),
    )
    .unwrap();
    record["uid"] = json!(record["uid"].as_u64().unwrap() + 1);
    let path = f.host.root.join("data/state/managed-process.json");
    write_json(&path, &record).unwrap();
    let stop = f.host.submit(Action::Stop, None).await.unwrap();
    let completed = f.complete(&stop).await;
    assert_eq!(completed.error_code, Some(Code::InstanceConflict));
    assert!(path.exists());
    assert_eq!(owner.host.status().await.state, State::Running);
    assert!(owner.host.router.idoris_gateway().is_ok());
    owner.host.shutdown().await.unwrap();
}
