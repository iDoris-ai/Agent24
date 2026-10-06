//! COMM-unlock integration against the real, currently locked Hyphae CLI.
//! This named test is ignored in normal suites because it requires a local
//! lock-matched CLI. Explicit `--ignored` execution without `HYPHAE_TEST_BIN`
//! intentionally fails rather than silently skipping the real-binary check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    Account, CommState, Envelope, HyphaeLock, HyphaeRunner, Invocation, MemoryPasswordStore,
    Password, PasswordStore, VerifiedBinary, current_platform, router,
};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

fn embedded_lock_cli() -> PathBuf {
    let test_bin = std::env::var("HYPHAE_TEST_BIN")
        .expect("HYPHAE_TEST_BIN is required for unlock_real_binary");
    let source = PathBuf::from(test_bin)
        .canonicalize()
        .expect("HYPHAE_TEST_BIN must name a readable local Hyphae CLI");
    let bytes = std::fs::read(&source).expect("read HYPHAE_TEST_BIN");
    let actual = agent24_comm::binary::sha256_of(&bytes);
    let lock = HyphaeLock::embedded().expect("embedded Hyphae lock parses");
    let expected = lock
        .expected_for(&current_platform())
        .expect("embedded lock has a hash for the current platform");
    assert_eq!(
        actual, expected,
        "unlock_real_binary accepts only the embedded lock's current-platform CLI"
    );
    source
}

fn file_inventory(root: &Path) -> BTreeMap<String, (u64, String)> {
    fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<String, (u64, String)>) {
        for entry in std::fs::read_dir(dir).expect("read temporary HOME inventory") {
            let entry = entry.expect("read inventory entry");
            let path = entry.path();
            let metadata = entry.metadata().expect("stat inventory entry");
            if metadata.is_dir() {
                visit(root, &path, files);
            } else if metadata.is_file() {
                let bytes = std::fs::read(&path).expect("read inventory file");
                files.insert(
                    path.strip_prefix(root)
                        .expect("inventory path beneath HOME")
                        .to_string_lossy()
                        .into_owned(),
                    (
                        metadata.len(),
                        agent24_comm::binary::sha256_of(&bytes).to_hex(),
                    ),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

async fn post_unlock(app: axum::Router, password: &str, remember: bool) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/unlock")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({"password": password, "remember": remember})).unwrap(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
#[ignore = "requires the current embedded-lock Hyphae CLI; set HYPHAE_TEST_BIN and pass --ignored"]
async fn unlock_checks_current_lock_binary_before_registering_password() {
    let source = embedded_lock_cli();
    let tmp = tempfile::tempdir().expect("temporary HOME");
    let home = tmp.path().join("hyphae-home");
    tokio::fs::create_dir_all(&home)
        .await
        .expect("create temporary HOME");
    let install_dir = tmp.path().join("verified-bin");
    let expected = HyphaeLock::embedded()
        .unwrap()
        .expected_for(&current_platform())
        .unwrap();
    let verified = VerifiedBinary::install(&source, expected, &install_dir)
        .await
        .expect("install current lock binary");
    let runner = Arc::new(HyphaeRunner::new(
        verified,
        home.clone(),
        Duration::from_secs(15),
    ));
    let secret = "unlock integration password";
    let created = runner
        .run(Invocation {
            args: vec![
                OsString::from("identity"),
                OsString::from("create"),
                OsString::from("--nickname"),
                OsString::from("unlock-test"),
            ],
            password: Some(Password::new(secret.as_bytes().to_vec()).unwrap()),
            timeout: None,
        })
        .await
        .expect("run identity create");
    assert!(matches!(created, Envelope::Ok { .. }), "{created:?}");

    let keystore: Value = serde_json::from_slice(
        &tokio::fs::read(home.join(".hyphae").join("keystore.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    let account = Account::from_salt(keystore["salt"].as_str().unwrap());
    let store = Arc::new(MemoryPasswordStore::new());
    assert!(matches!(
        store.get(&account).await,
        Err(agent24_comm::StoreError::NotFound)
    ));
    let app = router(CommState::ready(runner, store.clone(), home.clone()));
    let files_before_unlock = file_inventory(tmp.path());

    let (bad_status, bad_body) = post_unlock(app.clone(), "wrong password", false).await;
    assert_eq!(bad_status, StatusCode::LOCKED, "{bad_body:?}");
    assert_eq!(bad_body["error"], "locked");
    assert!(matches!(
        store.get(&account).await,
        Err(agent24_comm::StoreError::NotFound)
    ));
    assert_eq!(file_inventory(tmp.path()), files_before_unlock);

    let (status, body) = post_unlock(app, secret, true).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["data"]["unlocked"], true);
    // MemoryPasswordStore always reports session-only storage even when the
    // request asks to remember the password.
    assert_eq!(body["data"]["remembered"], false);
    assert_eq!(file_inventory(tmp.path()), files_before_unlock);

    // Smoke a real operation that requires the keystore password, using only
    // the credential restored into this fresh in-memory state by unlock.
    let password = store.get(&account).await.expect("unlock restored password");
    let runner = Arc::new(HyphaeRunner::new(
        VerifiedBinary::install(&source, expected, &tmp.path().join("verified-bin-followup"))
            .await
            .expect("install locked CLI for follow-up operation"),
        home,
        Duration::from_secs(15),
    ));
    let operation = runner
        .run(Invocation {
            args: vec![
                OsString::from("identity"),
                OsString::from("create"),
                OsString::from("--nickname"),
                OsString::from("unlock-followup"),
            ],
            password: Some(password),
            timeout: None,
        })
        .await
        .expect("run password-required identity create");
    assert!(matches!(operation, Envelope::Ok { .. }), "{operation:?}");
}
