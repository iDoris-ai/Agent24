#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(clippy::unwrap_used)]

use agent24_idoris::{
    IdorisLock,
    download::{DownloadOptions, download_locked_asset},
    install::{Codesign, InstallOptions, install_locked_asset},
    omlx_probe::{Endpoint, OmlxProbe, ProbeStatus, ServiceIdentity, read_settings_key},
    setup::{Role, SetupBudget, run_setup},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Run manually with A24_IL05_MODEL_ID and A24_IL05_CORE_RESERVED_BYTES set.
/// Credentials come only from the existing IL-04 in-memory settings reader.
#[tokio::test]
#[ignore = "requires signed v0.4.0, local oMLX:8088, and explicit model/core selection"]
async fn real_v0_4_0_setup_with_local_omlx() {
    let endpoint = Endpoint::parse("http://127.0.0.1:8088").unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    if client
        .get("http://127.0.0.1:8088/v1/models")
        .send()
        .await
        .is_err()
    {
        eprintln!("SKIP real setup: local oMLX:8088 is unreachable");
        return;
    }
    let key = read_settings_key().ok();
    let probe = OmlxProbe::new()
        .unwrap()
        .probe(Some(&endpoint), key.as_ref())
        .await
        .into_iter()
        .find(|p| p.endpoint == endpoint)
        .unwrap();
    assert_eq!(probe.identity, ServiceIdentity::Omlx);
    assert!(matches!(probe.status, ProbeStatus::Ready { .. }));
    let model = std::env::var("A24_IL05_MODEL_ID").unwrap();
    let core = std::env::var("A24_IL05_CORE_RESERVED_BYTES")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let report = agent24_hw::HardwareReport::from_env(&agent24_hw::SystemProbe::new());
    let budget = SetupBudget::from_report(Some(&report), Some(core)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let lock = IdorisLock::embedded();
    let archive = download_locked_asset(
        lock,
        "aarch64-apple-darwin",
        &dir.path().join("cache"),
        &DownloadOptions::default(),
        &cancel,
    )
    .await
    .unwrap();
    let installed = install_locked_asset(
        lock,
        "aarch64-apple-darwin",
        &archive,
        &InstallOptions::new(
            dir.path().join("install"),
            env!("CARGO_BIN_EXE_idoris-install-child"),
        ),
        &Codesign,
    )
    .unwrap();
    let result = run_setup(
        &installed,
        &dir.path().join("instances/smoke"),
        &probe,
        key.map(|k| k.into_secret()),
        &[(Role::Daily, model.clone())],
        Some(budget),
        &cancel,
    )
    .await
    .unwrap();
    assert_eq!(result.bindings.get(&Role::Daily), Some(&model));
    assert!(result.components_dir.join("omlx.yaml").is_file());
    assert!(result.components_dir.join("subscription.yaml").is_file());
    eprintln!(
        "PASS real v0.4.0 setup: validated generation, daily binding, budget={} bytes",
        budget.bytes()
    );
}
