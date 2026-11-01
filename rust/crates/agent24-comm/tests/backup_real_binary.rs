//! Key backup (`identity export`) and restore (`identity import`) against the
//! REAL released Hyphae v0.28.1 (the build pinned in `hyphae.lock.json`), and
//! `hyphae_too_old` against the previous release v0.27.2. Ignored in normal
//! runs; every home is an isolated temp dir.
//!
//! - `HYPHAE_TEST_BIN`: the released v0.28.1 (checked against the embedded lock).
//! - `HYPHAE_OLD_BIN`: v0.27.2 (no non-interactive export, no `identity import`).
//! - `HYPHAE_EXPORT_OUT`: optional directory in which the exported backup
//!   (password `backup passphrase 1`) is left, for an independent decrypt.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    Account, CommState, Envelope, HyphaeLock, HyphaeRunner, Invocation, MemoryPasswordStore,
    Password, PasswordStore, VerifiedBinary, current_platform, router,
};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const KEYSTORE_PW: &str = "keystore integration password";
const BACKUP_PW: &str = "backup passphrase 1";

struct Home {
    app: Router,
    runner: Arc<HyphaeRunner>,
    store: Arc<MemoryPasswordStore>,
    home: PathBuf,
    _tmp: tempfile::TempDir,
}

async fn install(
    source: &Path,
    expected: agent24_comm::Sha256Digest,
    dir: &Path,
) -> VerifiedBinary {
    VerifiedBinary::install(source, expected, &dir.join("bin"))
        .await
        .unwrap()
}

/// A fresh isolated home; with `create`, one identity called `create` already
/// exists and its keystore password is filed in the (in-memory) keychain.
async fn new_home(source: &Path, lock_pinned: bool, create: Option<&str>) -> Home {
    // `lock_pinned`: the binary must be exactly the one hyphae.lock.json names.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("hyphae-home");
    tokio::fs::create_dir_all(&home).await.unwrap();
    let bytes = std::fs::read(source).unwrap();
    let digest = agent24_comm::binary::sha256_of(&bytes);
    if lock_pinned {
        let expected = HyphaeLock::embedded()
            .unwrap()
            .expected_for(&current_platform())
            .unwrap();
        assert_eq!(
            digest, expected,
            "HYPHAE_TEST_BIN must be the locked v0.28.1 build"
        );
    }
    let verified = install(source, digest, tmp.path()).await;
    let runner = Arc::new(HyphaeRunner::new(
        verified,
        home.clone(),
        Duration::from_secs(30),
    ));
    let store = Arc::new(MemoryPasswordStore::new());
    if let Some(nick) = create {
        let created = runner
            .run(Invocation {
                args: ["identity", "create", "--nickname", nick, "--default"]
                    .map(OsString::from)
                    .to_vec(),
                password: Some(Password::new(KEYSTORE_PW.as_bytes().to_vec()).unwrap()),
                timeout: None,
            })
            .await
            .unwrap();
        assert!(matches!(created, Envelope::Ok { .. }), "{created:?}");
        let keystore: Value =
            serde_json::from_slice(&std::fs::read(home.join(".hyphae/keystore.json")).unwrap())
                .unwrap();
        store
            .put(
                &Account::from_salt(keystore["salt"].as_str().unwrap()),
                &Password::new(KEYSTORE_PW.as_bytes().to_vec()).unwrap(),
            )
            .await
            .unwrap();
    }
    let app = router(CommState::ready(
        runner.clone(),
        store.clone(),
        home.clone(),
    ));
    Home {
        app,
        runner,
        store,
        home,
        _tmp: tmp,
    }
}

async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn identities(h: &Home) -> Vec<(String, String)> {
    let Envelope::Ok { data } = h
        .runner
        .run(Invocation {
            args: vec!["identity".into(), "list".into()],
            password: None,
            timeout: None,
        })
        .await
        .unwrap()
    else {
        panic!("identity list failed")
    };
    data.as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["nickname"].as_str().unwrap().to_owned(),
                i["npub"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn preview_bin() -> PathBuf {
    PathBuf::from(std::env::var("HYPHAE_TEST_BIN").expect("HYPHAE_TEST_BIN"))
        .canonicalize()
        .unwrap()
}

#[tokio::test]
#[ignore = "requires the locked Hyphae v0.28.1; set HYPHAE_TEST_BIN and pass --ignored"]
async fn export_then_restore_round_trips_through_the_real_binary() {
    let bin = preview_bin();
    let a = new_home(&bin, true, Some("exp")).await;
    let npub = identities(&a).await[0].1.clone();

    // ---- export ----------------------------------------------------------
    let out_dir = std::env::var("HYPHAE_EXPORT_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| a._tmp.path().join("out"));
    std::fs::create_dir_all(&out_dir).unwrap();
    let backup = out_dir.join("exp.ncryptsec");
    let (status, body) = post(
        &a.app,
        "/identity/export",
        json!({"nickname": "exp", "path": backup, "backup_password": BACKUP_PW}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = body.to_string();
    assert!(!text.contains("ncryptsec1") && !text.contains("nsec1") && !text.contains(BACKUP_PW));
    assert_eq!(body["data"]["npub"], npub.as_str());
    assert_eq!(
        std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let file = std::fs::read_to_string(&backup).unwrap();
    assert!(file.starts_with("ncryptsec1") && file.ends_with('\n') && !file.contains("nsec1"));
    let staging: Vec<_> = std::fs::read_dir(&out_dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".agent24")
        })
        .collect();
    assert!(staging.is_empty(), "no staging file may remain");
    // an existing target is refused and left alone
    let (status, body) = post(
        &a.app,
        "/identity/export",
        json!({"nickname": "exp", "path": backup, "backup_password": "other passphrase"}),
    )
    .await;
    assert_eq!(
        (status, body["data"]["reason"].as_str()),
        (StatusCode::CONFLICT, Some("target_exists"))
    );
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), file);

    // ---- restore as the very first identity of another home ---------------
    let b = new_home(&bin, true, None).await;
    let (status, body) = post(
        &b.app,
        "/identity/import",
        json!({"nickname": "restored", "path": backup, "backup_password": BACKUP_PW}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            body["data"]["status"].as_str(),
            body["data"]["default"].as_bool()
        ),
        (Some("imported"), Some(true))
    );
    assert_eq!(
        identities(&b).await,
        vec![("restored".to_owned(), npub.clone())],
        "same npub"
    );
    let keystore: Value =
        serde_json::from_slice(&std::fs::read(b.home.join(".hyphae/keystore.json")).unwrap())
            .unwrap();
    assert_eq!(keystore["encrypted"], true);
    assert!(
        b.store
            .get(&Account::from_salt(keystore["salt"].as_str().unwrap()))
            .await
            .is_ok(),
        "generated keychain password was promoted"
    );

    // wrong backup password -> bad_backup_password, nothing changes
    let (status, body) = post(
        &b.app,
        "/identity/import",
        json!({"nickname": "other", "path": backup, "backup_password": "wrong passphrase"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_backup_password")),
        "{body}"
    );
    // same nickname -> nickname_exists
    let (status, body) = post(
        &b.app,
        "/identity/import",
        json!({"nickname": "restored", "path": backup, "backup_password": BACKUP_PW}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("nickname_exists")),
        "{body}"
    );
    // same key under a new nickname -> already_present, naming the existing one
    let (status, body) = post(
        &b.app,
        "/identity/import",
        json!({"nickname": "twin", "path": backup, "backup_password": BACKUP_PW}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"], "already_present");
    assert_eq!(
        (
            body["data"]["nickname"].as_str(),
            body["data"]["npub"].as_str()
        ),
        (Some("restored"), Some(npub.as_str()))
    );
    assert_eq!(identities(&b).await.len(), 1, "nothing was added");
    // a file that is not a backup
    let junk = out_dir.join("junk.txt");
    std::fs::write(&junk, "hello\n").unwrap();
    let (status, body) = post(
        &b.app,
        "/identity/import",
        json!({"nickname": "j", "path": junk, "backup_password": BACKUP_PW}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_key"))
    );

    // ---- restore into a home that already has a keystore ------------------
    let c = new_home(&bin, true, Some("local")).await;
    let (status, body) = post(&c.app, "/identity/import", json!({"nickname": "restored", "path": backup, "backup_password": BACKUP_PW, "default": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = identities(&c).await;
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&("restored".to_owned(), npub.clone())));

    // v0.28.1: with the keystore password AND the backup password both wrong,
    // import answers keystore_locked (not bad_backup_password); a wrong keychain
    // password with the right backup password is keystore_locked too.
    let wrong_keystore = Password::new(b"not the keystore password".to_vec()).unwrap();
    let salt = {
        let keystore: Value =
            serde_json::from_slice(&std::fs::read(c.home.join(".hyphae/keystore.json")).unwrap())
                .unwrap();
        keystore["salt"].as_str().unwrap().to_owned()
    };
    c.store
        .put(&Account::from_salt(&salt), &wrong_keystore)
        .await
        .unwrap();
    for bp in ["wrong passphrase", BACKUP_PW] {
        let (status, body) = post(
            &c.app,
            "/identity/import",
            json!({"nickname": "other", "path": backup, "backup_password": bp}),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::LOCKED, Some("keystore_locked")),
            "{bp}: {body}"
        );
    }

    if std::env::var("HYPHAE_EXPORT_OUT").is_err() {
        std::fs::remove_file(&backup).unwrap();
    }
    println!("EXPORT_NPUB={npub}");
}

#[tokio::test]
#[ignore = "requires Hyphae v0.27.2; set HYPHAE_OLD_BIN and pass --ignored"]
async fn both_endpoints_say_hyphae_too_old_on_the_previous_release() {
    let old = PathBuf::from(std::env::var("HYPHAE_OLD_BIN").expect("HYPHAE_OLD_BIN"))
        .canonicalize()
        .unwrap();
    let h = new_home(&old, false, Some("old")).await;
    let dir = h._tmp.path().join("out");
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("old.ncryptsec");
    let (status, body) = post(
        &h.app,
        "/identity/export",
        json!({"nickname": "old", "path": dest, "backup_password": BACKUP_PW}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("hyphae_too_old")),
        "{body}"
    );
    assert!(body["message"].as_str().unwrap().contains("v0.28.0"));
    let file = dir.join("in.ncryptsec");
    std::fs::write(&file, format!("ncryptsec1{}\n", "q".repeat(150))).unwrap();
    let (status, body) = post(
        &h.app,
        "/identity/import",
        json!({"nickname": "x", "path": file, "backup_password": BACKUP_PW}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("hyphae_too_old")),
        "{body}"
    );
    assert!(!dest.exists());
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "nothing was written"
    );
}
