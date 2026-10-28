#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::IdorisLock;

fn healthy() -> serde_json::Value {
    serde_json::json!({
        "status": "ok", "service": "idoris", "version": "0.4.0",
        "contract_version": IdorisLock::embedded().tested_contract_version,
        "instance_id": "fixture-instance", "dev_no_key_enabled": false
    })
}

#[test]
fn health_missing_or_invalid_fields_fail_closed() {
    let lock = crate::IdorisLock::embedded();
    let body = healthy();
    assert!(validate_health(&serde_json::to_vec(&body).unwrap(), lock).is_ok());
    for field in body.as_object().unwrap().keys() {
        let mut missing = body.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            validate_health(&serde_json::to_vec(&missing).unwrap(), lock).is_err(),
            "missing {field}"
        );
    }
    for (field, value) in [
        ("status", serde_json::json!("starting")),
        ("service", serde_json::json!("other")),
        ("version", serde_json::json!("0.3.0")),
        ("contract_version", serde_json::json!("2.0.0")),
        ("contract_version", serde_json::json!("invalid")),
        ("instance_id", serde_json::json!("")),
        ("dev_no_key_enabled", serde_json::json!(true)),
        ("dev_no_key_enabled", serde_json::json!("false")),
    ] {
        let mut bad = body.clone();
        bad[field] = value;
        assert!(
            validate_health(&serde_json::to_vec(&bad).unwrap(), lock).is_err(),
            "invalid {field}"
        );
    }
}

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::net::TcpListener;

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
}

/// The fixture child must own its listeners (F3), so a test proxy cannot hold
/// them. Ports come from 20000-29999, below every kernel ephemeral range, so
/// no concurrent `bind(0)` can be handed one; each port is issued once per
/// test process, and a per-process offset separates parallel worktrees.
fn reserve_port() -> u16 {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let offset = std::process::id() as usize * 397;
    loop {
        let port = 20_000 + (offset + NEXT.fetch_add(1, Ordering::SeqCst)) % 10_000;
        let port = port as u16;
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}
impl Fixture {
    fn new(scenario: serde_json::Value) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for part in ["release/config", "components", "state", "logs"] {
            fs::create_dir_all(root.join(part)).unwrap();
        }
        // /usr/bin/python3 is an xcrun shim on macOS and adds SDK env vars.
        // Resolve the interpreter once so the fixture observes our environment.
        static PYTHON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let python = PYTHON.get_or_init(|| {
            let result = std::process::Command::new("/usr/bin/python3")
                .env_clear()
                .args(["-c", "import sys; print(sys.executable)"])
                .output()
                .unwrap();
            assert!(result.status.success());
            String::from_utf8(result.stdout).unwrap().trim().to_owned()
        });
        let script = include_str!("../../tests/fixtures/idoris.py").replacen(
            "#!/usr/bin/python3",
            &format!("#!{python}"),
            1,
        );
        fs::write(root.join("release/idoris"), script).unwrap();
        for name in ["catalog.yaml", "routing-policy.yaml"] {
            fs::write(root.join("release/config").join(name), "fixture: true\n").unwrap();
        }
        fs::set_permissions(
            root.join("release/idoris"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::write(root.join("components/scenario.json"), scenario.to_string()).unwrap();
        Self { _root: temp, root }
    }
    fn scenario(&self, scenario: serde_json::Value) {
        fs::write(
            self.root.join("components/scenario.json"),
            scenario.to_string(),
        )
        .unwrap();
    }
    async fn config(&self) -> StartConfig {
        let components = ComponentsDir::new(&self.root, &self.root.join("components")).unwrap();
        let installed = InstalledVersion::fixture(self.root.join("release")).unwrap();
        let data = DataPaths {
            db: self.root.join("db.sqlite3"),
            state: self.root.join("state"),
        };
        let mut config = StartConfig::new(installed, components, data).unwrap();
        config.log_dir = self.root.join("logs");
        config.diagnostic_output = true;
        config.ports = Ports {
            data: reserve_port(),
            admin: reserve_port(),
        };
        config.timings.ready = Duration::from_secs(3);
        config.timings.poll = Duration::from_millis(10);
        config.timings.terminate = Duration::from_millis(70);
        config.timings.restart_delays = [
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(40),
        ];
        config
    }
    fn attempts(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.root.join("state/attempts.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn record(&self) -> PathBuf {
        self.root.join("state/managed-process.json")
    }
}
impl Fixture {
    fn release_exit(&self) {
        fs::write(self.root.join("state/release-exit"), "exit").unwrap();
    }
}
async fn inputs() -> Result<LaunchInputs, SupervisorError> {
    Ok(LaunchInputs {
        budget: Budget { allocated_bytes: 0 },
        omlx_key: Some(Zeroizing::new("fixture-upstream-secret".into())),
    })
}

#[tokio::test]
async fn stdin_frame_environment_readiness_stop_and_log_redaction() {
    let fixture = Fixture::new(serde_json::json!({"mode": "flood"}));
    let config = fixture.config().await;
    let supervisor = Supervisor::start(config, inputs, || async {})
        .await
        .unwrap();
    let accepting = supervisor.accepting_requests();
    assert!(*accepting.borrow());
    let attempts = fixture.attempts();
    assert_eq!(attempts.len(), 1);
    let report = &attempts[0];
    assert_eq!(
        report["argv"],
        serde_json::json!(["serve", "--admin-token-stdin"])
    );
    assert_eq!(report["length"], 65);
    assert_eq!(report["valid_frame"], true);
    assert_eq!(report["token_in_env"], false);
    assert_eq!(report["pid"], report["group"]);
    assert_eq!(report["budget"], "0");
    let allowed = [
        "PATH",
        "HOME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "__CF_USER_TEXT_ENCODING",
        "IDORIS_COMPONENTS_DIR",
        "IDORIS_DB_PATH",
        "IDORIS_STATE_DIR",
        "IDORIS_CATALOG",
        "IDORIS_ROUTING_POLICY",
        "IDORIS_ALLOCATED_BUDGET_BYTES",
        "IDORIS_PORT",
        "IDORIS_ADMIN_PORT",
        "IDORIS_BIND_HOST",
        "IDORIS_DEPLOY_MODE",
        "IDORIS_OMLX_API_KEY",
    ];
    for name in report["env_names"].as_array().unwrap() {
        assert!(
            allowed.contains(&name.as_str().unwrap()),
            "unexpected env {name}"
        );
    }
    let record = fs::read_to_string(fixture.record()).unwrap();
    assert!(!record.contains("token"));
    assert_eq!(
        fs::metadata(fixture.record()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let drained_flag = accepting.clone();
    // A separate lifecycle below verifies the drain callback ordering.
    let stopped = supervisor.stop().await.unwrap();
    assert!(!stopped.forced);
    assert!(!*drained_flag.borrow());
    assert!(!fixture.record().exists());
    let logs = fs::read_to_string(fixture.root.join("logs/idoris.log")).unwrap();
    assert!(logs.contains("[REDACTED]"));
    assert!(!logs.contains("fixture-upstream-secret"));
    assert!(!logs.contains("idk_"));
    assert!(!logs.contains(&"x".repeat(100)));
}

#[tokio::test]
async fn sigterm_timeout_kills_and_reaps_own_group_after_drain() {
    let fixture = Fixture::new(serde_json::json!({"mode": "ignore_term"}));
    let config = fixture.config().await;
    let called = Arc::new(AtomicUsize::new(0));
    let callback = called.clone();
    let supervisor = Supervisor::start(config, inputs, move || async move {
        callback.fetch_add(1, Ordering::SeqCst);
    })
    .await
    .unwrap();
    let pid = fixture.attempts()[0]["pid"].as_u64().unwrap() as u32;
    let accepting = supervisor.accepting_requests();
    let began = Instant::now();
    let report = supervisor.stop().await.unwrap();
    assert!(report.forced);
    assert!(began.elapsed() >= Duration::from_millis(70));
    assert!(!*accepting.borrow());
    assert_eq!(called.load(Ordering::SeqCst), 1);
    assert!(ProcessRecord::capture(pid).is_err());
}

#[tokio::test]
async fn exit_three_is_l4_other_exit_codes_are_failed() {
    for code in [3, 1, 2] {
        let fixture = Fixture::new(serde_json::json!({"mode": "exit", "code": code}));
        let result = Supervisor::start(fixture.config().await, inputs, || async {}).await;
        let error = match result {
            Ok(_) => panic!("unexpected readiness"),
            Err(error) => error,
        };
        if code == 3 {
            assert!(
                matches!(error, SupervisorError::InstanceConflict),
                "{error:?}"
            );
        } else {
            assert!(
                matches!(error, SupervisorError::Failed { code: Some(value) } if value == code),
                "{error:?}"
            );
        }
        assert!(!fixture.record().exists());
    }
}

#[tokio::test]
async fn unread_stdin_and_no_health_fail_start_and_reap() {
    let fixture = Fixture::new(serde_json::json!({"mode": "no_read"}));
    let mut config = fixture.config().await;
    // A 65-byte pipe write can succeed before its reader consumes anything.
    // The externally observable failure is absence of /health readiness.
    config.timings.ready = Duration::from_millis(100);
    let result = Supervisor::start(config, inputs, || async {}).await;
    assert!(matches!(result, Err(SupervisorError::StartupTimeout)));
    assert!(!fixture.record().exists());
}

#[tokio::test]
async fn health_invalid_or_missing_fields_stop_child_and_never_retry() {
    let fixture = Fixture::new(serde_json::json!({}));
    let baseline = healthy();
    for field in baseline.as_object().unwrap().keys() {
        fixture.scenario(serde_json::json!({"missing": [field]}));
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let result = Supervisor::start(
            fixture.config().await,
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                inputs()
            },
            || async {},
        )
        .await;
        assert!(
            matches!(result, Err(SupervisorError::Contract)),
            "missing {field}: {:?}",
            result.as_ref().err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(!fixture.record().exists());
    }
    for (field, value) in [
        ("status", serde_json::json!("bad")),
        ("service", serde_json::json!("alien")),
        ("version", serde_json::json!("0.3.0")),
        ("contract_version", serde_json::json!("99.0.0")),
        ("instance_id", serde_json::json!("")),
        ("dev_no_key_enabled", serde_json::json!(true)),
    ] {
        let mut changes = serde_json::Map::new();
        changes.insert(field.into(), value);
        fixture.scenario(serde_json::json!({"health": changes}));
        let result = Supervisor::start(fixture.config().await, inputs, || async {}).await;
        assert!(
            matches!(result, Err(SupervisorError::Contract)),
            "invalid {field}: {:?}",
            result.as_ref().err()
        );
        assert!(!fixture.record().exists());
    }
}

#[test]
fn restart_delays_limit_window_and_stability_reset() {
    let timings = Timings::default();
    assert_eq!(timings.terminate, Duration::from_secs(17));
    assert_eq!(timings.token_write, Duration::from_secs(10));
    assert_eq!(timings.health_request, Duration::from_secs(2));
    assert_eq!(timings.ready, Duration::from_secs(30));
    assert_eq!(timings.poll, Duration::from_millis(250));
    let initial = Instant::now();
    let mut policy = RestartPolicy {
        attempts: VecDeque::new(),
    };
    for delay in [1, 2, 4] {
        assert_eq!(
            policy.delay(initial, initial, &timings).unwrap(),
            Duration::from_secs(delay)
        );
    }
    assert!(matches!(
        policy.delay(initial, initial, &timings),
        Err(SupervisorError::RestartLimit)
    ));
    assert_eq!(
        policy
            .delay(initial + Duration::from_secs(300), initial, &timings)
            .unwrap(),
        Duration::from_secs(1)
    );
}

#[tokio::test]
async fn crash_restarts_three_times_with_fresh_inputs_and_tokens() {
    let fixture = Fixture::new(serde_json::json!({"mode": "crash"}));
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let supervisor = Supervisor::start(
        fixture.config().await,
        move || {
            let number = count.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(LaunchInputs {
                    budget: Budget {
                        allocated_bytes: number as u64,
                    },
                    omlx_key: Some(Zeroizing::new(format!("fresh-key-{number}"))),
                })
            }
        },
        || async {},
    )
    .await
    .unwrap();
    let accepting = supervisor.accepting_requests();
    assert!(matches!(
        timeout(Duration::from_secs(8), supervisor.wait())
            .await
            .unwrap(),
        Err(SupervisorError::RestartLimit)
    ));
    assert!(!*accepting.borrow());
    let attempts = fixture.attempts();
    assert_eq!(attempts.len(), 4);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let hashes: std::collections::BTreeSet<_> = attempts
        .iter()
        .map(|item| item["token_hash"].as_str().unwrap())
        .collect();
    let keys: std::collections::BTreeSet<_> = attempts
        .iter()
        .map(|item| item["key_hash"].as_str().unwrap())
        .collect();
    assert_eq!(hashes.len(), 4);
    assert_eq!(keys.len(), 4);
    for (number, item) in attempts.iter().enumerate() {
        assert_eq!(item["budget"], number.to_string());
    }
}

#[tokio::test]
async fn no_restart_for_l4_config_auth_budget_contract_or_user_stop() {
    for error_code in [1, 3] {
        let fixture = Fixture::new(
            serde_json::json!({"mode": "exit_after_ready", "code": error_code, "wait_for_exit": true, "after": 0.0}),
        );
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let supervisor = Supervisor::start(
            fixture.config().await,
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                inputs()
            },
            || async {},
        )
        .await
        .unwrap();
        fixture.release_exit();
        assert!(supervisor.wait().await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
    for error in [
        SupervisorError::Authentication,
        SupervisorError::Configuration,
        SupervisorError::Budget,
        SupervisorError::Contract,
        SupervisorError::InstanceConflict,
    ] {
        let fixture =
            Fixture::new(serde_json::json!({"mode": "crash", "wait_for_exit": true, "after": 0.0}));
        let mut next_error = Some(error);
        let mut called = false;
        let supervisor = Supervisor::start(
            fixture.config().await,
            move || {
                let value = if called {
                    Err(next_error.take().unwrap())
                } else {
                    called = true;
                    Ok(LaunchInputs {
                        budget: Budget { allocated_bytes: 0 },
                        omlx_key: None,
                    })
                };
                async move { value }
            },
            || async {},
        )
        .await
        .unwrap();
        fixture.release_exit();
        assert!(supervisor.wait().await.is_err());
        assert_eq!(fixture.attempts().len(), 1);
    }
    let fixture = Fixture::new(serde_json::json!({}));
    let supervisor = Supervisor::start(fixture.config().await, inputs, || async {})
        .await
        .unwrap();
    supervisor.stop().await.unwrap();
    sleep(Duration::from_millis(70)).await;
    assert_eq!(fixture.attempts().len(), 1);
}

#[test]
fn components_reject_relative_paths_and_symlink_escape() {
    let root = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    assert!(ComponentsDir::new(root.path(), Path::new("relative")).is_err());
    std::os::unix::fs::symlink(external.path(), root.path().join("escape")).unwrap();
    assert!(matches!(
        ComponentsDir::new(root.path(), &root.path().join("escape")),
        Err(SupervisorError::Configuration)
    ));
}

#[tokio::test]
async fn leftover_four_fields_must_match_before_any_signal() {
    let fixture = Fixture::new(serde_json::json!({}));
    let mut child = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let record = ProcessRecord::capture(child.id().unwrap()).unwrap();
    let lease = Lease::acquire(&fixture.root.join("state")).unwrap();
    let timings = Timings {
        terminate: Duration::from_millis(100),
        ..Timings::default()
    };
    for field in ["pid", "start_time", "executable", "uid"] {
        let mut bad = record.clone();
        match field {
            "pid" => {
                bad.pid = std::process::id();
                bad.start_time = ProcessRecord::capture(bad.pid).unwrap().start_time;
            }
            "start_time" => bad.start_time += 1,
            "executable" => bad.executable = PathBuf::from("/different-program"),
            "uid" => bad.uid += 1,
            _ => unreachable!(),
        }
        lease.write(&bad).unwrap();
        let result = lease
            .recover(&timings, &CancellationToken::new(), &record.executable)
            .await;
        if field == "start_time" {
            // A reused PID belongs to a new process. The recorded process is
            // gone, so clear the stale record without signalling its new owner.
            assert!(result.is_ok(), "{result:?}");
            assert!(!fixture.record().exists());
        } else {
            assert!(
                matches!(result, Err(SupervisorError::InstanceConflict)),
                "{field}"
            );
            assert!(fixture.record().exists());
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "sent a signal on {field} mismatch"
        );
    }
    lease.write(&record).unwrap();
    let reaper = tokio::spawn(async move { child.wait().await.unwrap() });
    lease
        .recover(&timings, &CancellationToken::new(), &record.executable)
        .await
        .unwrap();
    reaper.await.unwrap();
    assert!(!fixture.record().exists());
}

#[tokio::test]
#[ignore = "requires IDORIS_REAL_V040_DIR with the verified v0.4.0 release; runs real serve"]
async fn real_v040_ready_then_stop() {
    let release =
        PathBuf::from(std::env::var_os("IDORIS_REAL_V040_DIR").expect("set release directory"));
    let fixture = Fixture::new(serde_json::json!({}));
    // Use the upstream oMLX card without probing or modifying a user's setup.
    // Readiness needs no model inference or data key with a personal loopback bind.
    fs::copy(
        release.join("config/components/omlx.yaml"),
        fixture.root.join("components/omlx.yaml"),
    )
    .unwrap();
    fs::remove_file(fixture.root.join("components/scenario.json")).unwrap();
    let mut config = fixture.config().await;
    config.installed = InstalledVersion::fixture(fs::canonicalize(release).unwrap()).unwrap();
    config.ports = Ports::default();
    config.timings = Timings::default();
    config.diagnostic_output = false;
    let supervisor = Supervisor::start(
        config,
        || async {
            Ok(LaunchInputs {
                budget: Budget {
                    allocated_bytes: 8_000_000_000,
                },
                omlx_key: None,
            })
        },
        || async {},
    )
    .await
    .unwrap();
    assert!(*supervisor.accepting_requests().borrow());
    let report = supervisor.stop().await.unwrap();
    assert!(!report.forced);
    assert!(!fixture.record().exists());
}

#[tokio::test]
async fn stop_closes_gateway_before_drain_and_bounds_pending_drain() {
    let fixture = Fixture::new(serde_json::json!({}));
    let mut config = fixture.config().await;
    config.timings.drain = Duration::from_millis(30);
    let shared = Arc::new(std::sync::Mutex::new(None::<watch::Receiver<bool>>));
    let callback = shared.clone();
    let supervisor = Supervisor::start(config, inputs, move || async move {
        assert!(!*callback.lock().unwrap().as_ref().unwrap().borrow());
        std::future::pending::<()>().await;
    })
    .await
    .unwrap();
    *shared.lock().unwrap() = Some(supervisor.accepting_requests());
    let result = supervisor.stop().await.unwrap();
    assert!(result.drain_timed_out);
    assert!(!result.forced);
}

#[tokio::test]
async fn shutdown_during_backoff_does_not_refresh_or_restart() {
    let fixture = Fixture::new(serde_json::json!({"mode": "crash"}));
    let mut config = fixture.config().await;
    config.timings.restart_delays = [Duration::from_secs(3); 3];
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let supervisor = Supervisor::start(
        config,
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            inputs()
        },
        || async {},
    )
    .await
    .unwrap();
    let mut accepting = supervisor.accepting_requests();
    timeout(Duration::from_secs(3), accepting.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(!*accepting.borrow());
    supervisor.stop().await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.attempts().len(), 1);
}

#[tokio::test]
async fn pre_cancel_and_token_deadline_fail_without_downgrade() {
    let fixture = Fixture::new(serde_json::json!({}));
    let config = fixture.config().await;
    config.shutdown.cancel();
    let result = Supervisor::start(
        config,
        || async { panic!("must not refresh after stop") },
        || async {},
    )
    .await;
    assert!(matches!(result, Err(SupervisorError::Stopped)));
    let mut config = fixture.config().await;
    config.timings.token_write = Duration::ZERO;
    let result = Supervisor::start(config, inputs, || async {}).await;
    assert!(matches!(result, Err(SupervisorError::TokenHandoff)));
    assert!(fixture.attempts().is_empty());
    assert!(!fixture.record().exists());
}

#[test]
fn health_uses_lock_minimum_minor_and_allows_newer_minor() {
    let mut lock: IdorisLock =
        serde_json::from_str(include_str!("../../idoris.lock.json")).unwrap();
    lock.min_contract_minor = 3;
    let mut body = healthy();
    body["contract_version"] = serde_json::json!("1.2.99");
    assert!(validate_health(&serde_json::to_vec(&body).unwrap(), &lock).is_err());
    for contract in ["1.3.0", "1.9.1"] {
        body["contract_version"] = serde_json::json!(contract);
        assert!(validate_health(&serde_json::to_vec(&body).unwrap(), &lock).is_ok());
    }
}

#[tokio::test]
async fn crash_before_first_health_uses_the_same_restart_limit() {
    let fixture = Fixture::new(serde_json::json!({"mode": "crash_before_ready"}));
    let result = Supervisor::start(fixture.config().await, inputs, || async {}).await;
    assert!(matches!(result, Err(SupervisorError::RestartLimit)));
    assert_eq!(fixture.attempts().len(), 4);
    assert!(!fixture.record().exists());
}

#[tokio::test]
async fn early_leader_exit_still_reclaims_stubborn_descendants() {
    let fixture = Fixture::new(serde_json::json!({"mode": "stubborn_descendant"}));
    let supervisor = Supervisor::start(fixture.config().await, inputs, || async {})
        .await
        .unwrap();
    let descendant: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(fixture.root.join("state/descendant.json")).unwrap(),
    )
    .unwrap();
    let record = ProcessRecord::capture(descendant["pid"].as_u64().unwrap() as u32).unwrap();
    let result = supervisor.stop().await.unwrap();
    assert!(result.forced);
    timeout(Duration::from_secs(2), async {
        while record.alive().unwrap() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn blocked_or_closed_stdin_fails_handoff_within_deadline() {
    let token = mint_token().unwrap();
    let (mut writer, _unread) = tokio::io::duplex(1);
    let began = Instant::now();
    let error = write_token(
        &mut writer,
        &token,
        began + Duration::from_millis(20),
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(error, Err(SupervisorError::TokenHandoff)));
    assert!(began.elapsed() >= Duration::from_millis(20));
    let (mut writer, reader) = tokio::io::duplex(65);
    drop(reader);
    let error = write_token(
        &mut writer,
        &token,
        Instant::now() + Duration::from_millis(20),
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(error, Err(SupervisorError::TokenHandoff)));
}

#[tokio::test]
async fn dead_leftover_is_cleared_on_repeated_recovery() {
    let fixture = Fixture::new(serde_json::json!({}));
    let mut child = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let record = ProcessRecord::capture(child.id().unwrap()).unwrap();
    child.kill().await.unwrap();
    let lease = Lease::acquire(&fixture.root.join("state")).unwrap();
    lease.write(&record).unwrap();
    for _ in 0..2 {
        let result = lease
            .recover(
                &Timings::default(),
                &CancellationToken::new(),
                &record.executable,
            )
            .await;
        assert!(result.is_ok(), "dead record blocks recovery: {result:?}");
        assert!(!fixture.record().exists());
    }
}

#[tokio::test]
async fn reused_pid_is_cleared_without_signalling_new_owner() {
    let fixture = Fixture::new(serde_json::json!({}));
    let mut child = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut record = ProcessRecord::capture(child.id().unwrap()).unwrap();
    record.start_time += 1;
    let lease = Lease::acquire(&fixture.root.join("state")).unwrap();
    lease.write(&record).unwrap();
    let result = lease
        .recover(
            &Timings::default(),
            &CancellationToken::new(),
            &record.executable,
        )
        .await;
    let untouched = child.try_wait().unwrap().is_none();
    child.kill().await.unwrap();
    assert!(result.is_ok(), "reused PID blocks recovery: {result:?}");
    assert!(!fixture.record().exists());
    assert!(untouched, "signalled the new PID owner");
}

#[tokio::test]
async fn foreign_health_without_child_admin_never_becomes_ready() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture::new(serde_json::json!({"mode": "no_read"}));
    let mut config = fixture.config().await;
    config.ports.data = listener.local_addr().unwrap().port();
    config.timings.ready = Duration::from_millis(200);
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            let body = healthy().to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let result = Supervisor::start(config, inputs, || async {}).await;
    server.abort();
    // The foreign data listener is now identified as a port conflict (L4).
    let rejected = matches!(result, Err(SupervisorError::InstanceConflict));
    if let Ok(supervisor) = result {
        supervisor.stop().await.unwrap();
    }
    assert!(
        rejected,
        "foreign /health admitted a child that never bound"
    );
    assert!(!fixture.record().exists());
}

#[tokio::test]
async fn modified_installed_files_are_rejected_before_spawn() {
    for name in [
        "idoris",
        "config/catalog.yaml",
        "config/routing-policy.yaml",
    ] {
        let fixture = Fixture::new(serde_json::json!({}));
        let config = fixture.config().await;
        let path = fixture.root.join("release").join(name);
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(
            serve_command(&config, &Budget { allocated_bytes: 0 }, None).is_err(),
            "accepted changed {name}"
        );
        assert!(fixture.attempts().is_empty());
    }
}

#[tokio::test]
async fn reaped_leader_does_not_signal_recycled_group() {
    let fixture = Fixture::new(serde_json::json!({}));
    let config = Arc::new(fixture.config().await);
    let lease = Lease::acquire(&config.data.state).unwrap();
    let mut process = ManagedProcess::start(config, inputs().await.unwrap(), &lease)
        .await
        .unwrap();
    // Reap through the production path: observe the exit, then snapshot.
    process.child.start_kill().unwrap();
    process.child.wait().await.unwrap();
    process.adopt_survivors().unwrap();
    let mut foreign = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Model a cached PGID that now names an unrelated process group.
    process.group = Some(Pid::from_raw(foreign.id().unwrap() as i32));
    // An unproven group is neither signalled nor restarted beside.
    assert!(matches!(
        process.finish().await,
        Err(SupervisorError::InstanceConflict)
    ));
    sleep(Duration::from_millis(20)).await;
    let untouched = foreign.try_wait().unwrap().is_none();
    if untouched {
        foreign.kill().await.unwrap();
    }
    assert!(
        untouched,
        "signalled a cached PGID after reaping its leader"
    );
}

#[tokio::test]
async fn symlinked_installed_files_are_rejected_before_spawn() {
    for name in [
        "idoris",
        "config/catalog.yaml",
        "config/routing-policy.yaml",
    ] {
        let fixture = Fixture::new(serde_json::json!({}));
        let config = fixture.config().await;
        let path = fixture.root.join("release").join(name);
        let target = path.with_extension("original");
        fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(target, path).unwrap();
        assert!(
            serve_command(&config, &Budget { allocated_bytes: 0 }, None).is_err(),
            "followed {name} symlink"
        );
        assert!(fixture.attempts().is_empty());
    }
}

#[tokio::test]
async fn modified_binary_is_rejected_on_crash_restart() {
    let fixture =
        Fixture::new(serde_json::json!({"mode": "crash", "wait_for_exit": true, "after": 0.0}));
    let supervisor = Supervisor::start(fixture.config().await, inputs, || async {})
        .await
        .unwrap();
    let path = fixture.root.join("release/idoris");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"\n# changed after first readiness\n");
    fs::write(path, bytes).unwrap();
    fixture.release_exit();
    let result = timeout(Duration::from_secs(3), supervisor.wait())
        .await
        .unwrap();
    assert!(
        matches!(result, Err(SupervisorError::Installation(crate::install::InstallError::HashMismatch(ref name))) if name == "idoris"),
        "{result:?}"
    );
    assert_eq!(fixture.attempts().len(), 1);
    assert!(!fixture.record().exists());
}

#[tokio::test]
async fn admin_requires_exactly_200_and_never_restarts() {
    for status in [204, 302, 401, 403, 500] {
        let fixture = Fixture::new(serde_json::json!({"admin_status": status}));
        let result = Supervisor::start(fixture.config().await, inputs, || async {}).await;
        if matches!(status, 401 | 403) {
            assert!(matches!(result, Err(SupervisorError::Authentication)));
        } else {
            assert!(matches!(result, Err(SupervisorError::Contract)));
        }
        assert_eq!(fixture.attempts().len(), 1);
        assert!(!fixture.record().exists());
    }
}

#[tokio::test]
async fn concurrent_starts_admit_only_one_owner() {
    let fixture = Fixture::new(serde_json::json!({}));
    let first = fixture.config().await;
    let second = fixture.config().await;
    let (a, b) = tokio::join!(
        Supervisor::start(first, inputs, || async {}),
        Supervisor::start(second, inputs, || async {})
    );
    let owner = match (a, b) {
        (Ok(owner), Err(SupervisorError::InstanceConflict))
        | (Err(SupervisorError::InstanceConflict), Ok(owner)) => owner,
        _ => panic!("concurrent starts did not elect exactly one owner"),
    };
    assert_eq!(fixture.attempts().len(), 1);
    owner.stop().await.unwrap();
    assert!(!fixture.record().exists());
}

#[tokio::test]
async fn recovery_waits_for_reaping_before_checking_group() {
    let fixture = Fixture::new(serde_json::json!({}));
    let mut child = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let record = ProcessRecord::capture(child.id().unwrap()).unwrap();
    let lease = Lease::acquire(&fixture.root.join("state")).unwrap();
    lease.write(&record).unwrap();
    let reaper = tokio::spawn(async move {
        sleep(Duration::from_millis(60)).await;
        child.wait().await.unwrap()
    });
    let result = lease
        .recover(
            &Timings::default(),
            &CancellationToken::new(),
            &record.executable,
        )
        .await;
    reaper.await.unwrap();
    assert!(result.is_ok(), "zombie group blocked recovery: {result:?}");
    assert!(!fixture.record().exists());
}

/// Serves every request with `status` and `body`, recording any bearer token.
fn foreign_server(
    listener: TcpListener,
    status: u16,
    body: String,
    saw_bearer: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0; 4096];
            let read = socket.read(&mut request).await.unwrap_or(0);
            let text = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            if text.contains("authorization: bearer") {
                saw_bearer.store(true, Ordering::SeqCst);
            }
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
        }
    })
}

async fn foreign_ports(
    config: &mut StartConfig,
) -> (Arc<std::sync::atomic::AtomicBool>, [JoinHandle<()>; 2]) {
    let data = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    config.ports = Ports {
        data: data.local_addr().unwrap().port(),
        admin: admin.local_addr().unwrap().port(),
    };
    let saw_bearer = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let servers = [
        foreign_server(data, 200, healthy().to_string(), saw_bearer.clone()),
        // Accepts any token: a status code cannot authenticate the server.
        foreign_server(admin, 200, "{}".into(), saw_bearer.clone()),
    ];
    (saw_bearer, servers)
}

#[tokio::test]
async fn probe_foreign_data_and_admin_listeners_admit_unbound_child() {
    let fixture = Fixture::new(serde_json::json!({"mode": "no_read"}));
    let mut config = fixture.config().await;
    config.timings.ready = Duration::from_millis(500);
    let (_, servers) = foreign_ports(&mut config).await;
    let result = Supervisor::start(config, inputs, || async {}).await;
    servers.iter().for_each(JoinHandle::abort);
    let admitted = result.is_ok();
    if let Ok(supervisor) = result {
        supervisor.stop().await.unwrap();
    }
    assert!(
        !admitted,
        "foreign data+admin listeners admitted a child that never bound"
    );
    assert!(!fixture.record().exists());
}

#[tokio::test]
async fn admin_token_is_never_sent_to_an_unowned_listener() {
    let fixture = Fixture::new(serde_json::json!({"mode": "no_read"}));
    let mut config = fixture.config().await;
    config.timings.ready = Duration::from_millis(500);
    let (saw_bearer, servers) = foreign_ports(&mut config).await;
    let result = Supervisor::start(config, inputs, || async {}).await;
    servers.iter().for_each(JoinHandle::abort);
    if let Ok(supervisor) = result {
        supervisor.stop().await.unwrap();
    }
    assert!(
        !saw_bearer.load(Ordering::SeqCst),
        "Admin token sent before listener ownership was proven"
    );
}

fn workers(fixture: &Fixture) -> Vec<u32> {
    fs::read_to_string(fixture.root.join("state/workers.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            value["pid"].as_u64().unwrap() as u32
        })
        .collect()
}

/// A reclaimed worker is gone (or an unreaped zombie) within the bound.
async fn all_dead(pids: &[u32]) -> bool {
    let live =
        |pid: u32| ProcessRecord::capture(pid).is_ok_and(|record| record.alive().unwrap_or(true));
    timeout(Duration::from_secs(2), async {
        while pids.iter().any(|pid| live(*pid)) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

fn kill_leftovers(pids: &[u32]) {
    for pid in pids {
        if ProcessRecord::capture(*pid).is_ok_and(|record| record.alive().unwrap_or(false)) {
            let _ = nix::sys::signal::kill(Pid::from_raw(*pid as i32), Signal::SIGKILL);
        }
    }
}

#[tokio::test]
async fn worker_forked_after_ready_is_reclaimed_when_leader_crashes() {
    let fixture = Fixture::new(
        serde_json::json!({"mode": "late_worker", "wait_for_exit": true, "after": 0.0}),
    );
    let supervisor = Supervisor::start(fixture.config().await, inputs, || async {})
        .await
        .unwrap();
    fixture.release_exit();
    // The crash restarts; the next generation waits for a fresh release.
    timeout(Duration::from_secs(5), async {
        while fixture.attempts().len() < 2 && !supervisor_done(&supervisor) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let late = workers(&fixture);
    assert_eq!(late.len(), 1);
    let reclaimed = all_dead(&late).await;
    kill_leftovers(&late);
    let _ = supervisor.stop().await;
    assert!(
        reclaimed,
        "worker forked after ready survived the leader crash"
    );
}

fn supervisor_done(supervisor: &Supervisor) -> bool {
    supervisor.task.as_ref().is_none_or(JoinHandle::is_finished)
}

#[tokio::test]
async fn readiness_crash_reclaims_worker_before_any_retry() {
    let fixture = Fixture::new(serde_json::json!({"mode": "worker_then_crash_before_ready"}));
    let config = Arc::new(fixture.config().await);
    let lease = Lease::acquire(&config.data.state).unwrap();
    let result = ManagedProcess::start(config, inputs().await.unwrap(), &lease).await;
    assert!(
        matches!(result, Err(SupervisorError::Failed { code: None })),
        "{:?}",
        result.as_ref().err()
    );
    let survivors = workers(&fixture);
    assert_eq!(survivors.len(), 1);
    let reclaimed = all_dead(&survivors).await;
    kill_leftovers(&survivors);
    assert!(reclaimed, "readiness-phase stop left the worker running");
}

#[tokio::test]
async fn stop_reports_forced_only_after_killing_survivors() {
    let fixture = Fixture::new(
        serde_json::json!({"mode": "late_worker", "wait_for_exit": true, "after": 0.0}),
    );
    let config = Arc::new(fixture.config().await);
    let lease = Lease::acquire(&config.data.state).unwrap();
    let mut process = ManagedProcess::start(config, inputs().await.unwrap(), &lease)
        .await
        .unwrap();
    fixture.release_exit();
    timeout(Duration::from_secs(3), async {
        while workers(&fixture).is_empty() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let survivors = workers(&fixture);
    let forced = process.stop().await;
    let reclaimed = all_dead(&survivors).await;
    kill_leftovers(&survivors);
    assert!(matches!(forced, Ok(true)), "{forced:?}");
    assert!(reclaimed, "stop reported forced=true but killed nothing");
}

#[tokio::test]
async fn dead_leader_with_live_group_keeps_record() {
    let fixture = Fixture::new(serde_json::json!({}));
    // sh leads the group; its background sleep stays in it after sh exits.
    let mut leader = Command::new("/bin/sh")
        .args(["-c", "sleep 60 & read line"])
        .process_group(0)
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = leader.id().unwrap();
    let group = Pid::from_raw(pid as i32);
    timeout(Duration::from_secs(2), async {
        while ProcessRecord::group_members(group).unwrap().len() < 2 {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let record = ProcessRecord::capture(pid).unwrap();
    drop(leader.stdin.take());
    leader.wait().await.unwrap();
    let lease = Lease::acquire(&fixture.root.join("state")).unwrap();
    lease.write(&record).unwrap();
    let timings = Timings {
        terminate: Duration::from_millis(100),
        ..Timings::default()
    };
    let result = lease
        .recover(&timings, &CancellationToken::new(), &record.executable)
        .await;
    let survived = group_exists(group).unwrap();
    let _ = killpg(group, Signal::SIGKILL);
    assert!(survived, "fixture group vanished early");
    assert!(
        matches!(result, Err(SupervisorError::InstanceConflict)),
        "{result:?}"
    );
    assert!(
        fixture.record().exists(),
        "cleared a record whose group lives"
    );
}
