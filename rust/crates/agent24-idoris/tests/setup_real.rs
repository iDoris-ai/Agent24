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
#[ignore = "requires signed v0.5.0, local oMLX:8088, and explicit model/core selection"]
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
        Codesign,
        &cancel,
    )
    .await
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
        "PASS real v0.5.0 setup: validated generation, daily binding, budget={} bytes",
        budget.bytes()
    );
}

async fn real_upstream_setup(embed: bool) {
    use agent24_idoris::{install::InstalledVersion, setup::run_setup_upstream};
    use axum::{Json, Router, routing::get};
    let binary = std::env::var_os("AGENT24_IDORIS_V6_BIN").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/models",
                get(|| async {
                    Json(serde_json::json!({"object":"list", "data":[
                        {"id":"tiny-chat", "object":"model"},
                        {"id":"text-embedding-bge-m3", "object":"model"}
                    ]}))
                }),
            ),
        )
        .await
        .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let version = dir.path().join("binary");
    std::fs::create_dir(&version).unwrap();
    std::os::unix::fs::symlink(binary, version.join("idoris")).unwrap();
    let installed = InstalledVersion::from_test_directory(version, "idoris");
    let mut bindings = vec![
        (Role::Daily, "tiny-chat".into()),
        (Role::Deep, "tiny-chat".into()),
    ];
    if embed {
        bindings.push((Role::Embed, "text-embedding-bge-m3".into()));
    }
    let result = run_setup_upstream(
        &installed,
        &dir.path().join("instances/smoke"),
        &endpoint,
        &bindings,
        "lmstudio",
        &CancellationToken::new(),
    )
    .await;
    server.abort();
    let outcome = result.unwrap();
    assert_eq!(outcome.bindings, bindings.into_iter().collect());
    assert!(outcome.components_dir.join("lmstudio.yaml").is_file());
}

/// AGENT24_IDORIS_V6_BIN=/Users/jason/Dev/auraai/a24-idoris-builds/m-2e7184d/x/idoris
#[tokio::test]
#[ignore = "requires AGENT24_IDORIS_V6_BIN; in-process OpenAI server, chat-only"]
async fn real_v6_setup_upstream_chat() {
    real_upstream_setup(false).await;
}

#[tokio::test]
#[ignore = "requires newer AGENT24_IDORIS_V6_BIN supporting --bind embed; m-2e7184d rejects it"]
async fn real_v6_setup_upstream_embed() {
    real_upstream_setup(true).await;
}
