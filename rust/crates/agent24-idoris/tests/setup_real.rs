#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(clippy::unwrap_used)]

use agent24_idoris::{
    IdorisLock,
    download::{DownloadOptions, download_locked_asset},
    install::{Codesign, InstallOptions, install_locked_asset},
    setup::{Role, SetupBudget, run_setup_auto},
    upstream::read_settings_key,
};
use tokio_util::sync::CancellationToken;

/// Run manually with A24_IL05_MODEL_ID and A24_IL05_CORE_RESERVED_BYTES set.
/// Credentials come only from the in-memory oMLX settings reader.
#[tokio::test]
#[ignore = "requires signed v0.6.0, local oMLX:8088, and explicit model/core selection"]
async fn real_v0_6_0_setup_with_local_omlx() {
    let key = read_settings_key().ok();
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
    let result = run_setup_auto(
        &installed,
        &dir.path().join("instances/smoke"),
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
        "PASS real v0.6.0 setup: validated generation, daily binding, budget={} bytes",
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

/// Run manually with AGENT24_IDORIS_V6_BIN set.
#[tokio::test]
#[ignore = "requires AGENT24_IDORIS_V6_BIN; in-process OpenAI server, chat-only"]
async fn real_v6_setup_upstream_chat() {
    real_upstream_setup(false).await;
}

#[tokio::test]
#[ignore = "requires AGENT24_IDORIS_V6_BIN supporting --bind embed (S7 build 14ff0b4 or later)"]
async fn real_v6_setup_upstream_embed() {
    real_upstream_setup(true).await;
}
