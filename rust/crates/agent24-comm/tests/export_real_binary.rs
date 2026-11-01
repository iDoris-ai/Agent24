//! `POST /identity/export` against the real, locked Hyphae CLI (which only
//! prompts for the keystore password on a terminal). Ignored in normal runs:
//! set `HYPHAE_TEST_BIN` (embedded-lock CLI) and optionally
//! `HYPHAE_EXPORT_OUT` (a directory to leave the 0600 backup in, so an
//! external check can re-derive the npub from the key).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
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

#[tokio::test]
#[ignore = "requires the current embedded-lock Hyphae CLI; set HYPHAE_TEST_BIN and pass --ignored"]
async fn export_writes_a_0600_backup_with_the_real_cli() {
    let source = PathBuf::from(std::env::var("HYPHAE_TEST_BIN").expect("HYPHAE_TEST_BIN"))
        .canonicalize()
        .unwrap();
    let expected = HyphaeLock::embedded()
        .unwrap()
        .expected_for(&current_platform())
        .unwrap();
    assert_eq!(
        agent24_comm::binary::sha256_of(&std::fs::read(&source).unwrap()),
        expected
    );
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("hyphae-home");
    tokio::fs::create_dir_all(&home).await.unwrap();
    let verified = VerifiedBinary::install(&source, expected, &tmp.path().join("bin"))
        .await
        .unwrap();
    let runner = Arc::new(HyphaeRunner::new(
        verified,
        home.clone(),
        Duration::from_secs(20),
    ));
    let secret = "export integration password";
    let created = runner
        .run(Invocation {
            args: ["identity", "create", "--nickname", "exp", "--default"]
                .map(OsString::from)
                .to_vec(),
            password: Some(Password::new(secret.as_bytes().to_vec()).unwrap()),
            timeout: None,
        })
        .await
        .unwrap();
    let Envelope::Ok { data } = created else {
        panic!("create failed")
    };
    let npub = data["npub"].as_str().unwrap().to_owned();
    let keystore: Value =
        serde_json::from_slice(&std::fs::read(home.join(".hyphae").join("keystore.json")).unwrap())
            .unwrap();
    let store = Arc::new(MemoryPasswordStore::new());
    store
        .put(
            &Account::from_salt(keystore["salt"].as_str().unwrap()),
            &Password::new(secret.as_bytes().to_vec()).unwrap(),
        )
        .await
        .unwrap();
    let app = router(CommState::ready(runner, store, home));
    let app_again = app.clone();

    let out_dir = std::env::var("HYPHAE_EXPORT_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| tmp.path().join("out"));
    std::fs::create_dir_all(&out_dir).unwrap();
    let dest = out_dir.join("exp.ncryptsec");
    let request = Request::builder()
        .method("POST")
        .uri("/identity/export")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(
                &json!({"nickname": "exp", "path": dest, "backup_password": "backup passphrase 1"}),
            )
            .unwrap(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        !text.contains("nsec1")
            && !text.contains("ncryptsec1")
            && !text.contains("backup passphrase")
    );
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["data"]["npub"], npub.as_str());
    assert_eq!(
        std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let key = std::fs::read_to_string(&dest).unwrap();
    assert!(key.starts_with("ncryptsec1") && key.ends_with('\n') && !key.contains("nsec1"));
    // an existing target is refused and left alone
    let again = app_again;
    let request = Request::builder()
        .method("POST")
        .uri("/identity/export")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(
                &json!({"nickname": "exp", "path": dest, "backup_password": "other passphrase"}),
            )
            .unwrap(),
        ))
        .unwrap();
    assert_eq!(
        again.oneshot(request).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), key);
    println!("EXPORT_NPUB={npub}");
}
