//! COMM-2b real-binary check: builds a genuine old-style Hyphae HOME with
//! the actual Hyphae CLI (not a fake shell script), imports it with
//! `agent24_comm::import::import`, and confirms the round trip against real
//! `identity`/`contact`/`storage outbox`/`history` output — not just the
//! fake-script-driven unit tests in `src/import.rs`.
//!
//! Requires `HYPHAE_TEST_BIN` (see `tests/real_binary.rs`'s own doc comment
//! for what it may point at); without it this prints why it's skipping and
//! returns early, same as every other `HYPHAE_TEST_BIN`-gated test in this
//! crate. Never touches the real `~/.hyphae` — both the "old" and "new"
//! homes here are throwaway `tempdir()`s.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use agent24_comm::password_store::{Account, MemoryPasswordStore, PasswordStore};
use agent24_comm::{
    HyphaeLock, HyphaeRunner, ImportRequest, Invocation, Password, Sha256Digest, VerifiedBinary,
    current_platform, import,
};

/// See `tests/real_binary.rs`'s own doc comment: `HYPHAE_TEST_BIN` may be
/// either the proposal's unreproducible acceptance binary (this hash) or a
/// binary matching the embedded `hyphae.lock.json` for the current platform.
const REFERENCE_SHA256: &str = "bc30dcf7bcf8b5c1865a3e995518c2bdab224bd8d6a3a064c4d7fc780de5e2b7";

async fn verified_bin_from_env(install_dir: &std::path::Path) -> Option<VerifiedBinary> {
    let test_bin = std::env::var("HYPHAE_TEST_BIN").ok()?;
    let source = PathBuf::from(&test_bin)
        .canonicalize()
        .unwrap_or_else(|e| panic!("HYPHAE_TEST_BIN={test_bin:?} is not readable: {e}"));
    let bytes = tokio::fs::read(&source)
        .await
        .unwrap_or_else(|e| panic!("HYPHAE_TEST_BIN={test_bin:?} is not readable: {e}"));
    let actual = agent24_comm::binary::sha256_of(&bytes);

    let platform = current_platform();
    let reference = Sha256Digest::from_hex(REFERENCE_SHA256).expect("valid reference sha256");
    let expected = if actual == reference {
        reference
    } else {
        HyphaeLock::embedded()
            .expect("embedded lock parses")
            .expected_for(&platform)
            .unwrap_or_else(|e| {
                panic!(
                    "HYPHAE_TEST_BIN (sha256 {}) matches neither the documented reference \
                     binary nor hyphae.lock.json's {platform} entry: {e}",
                    actual.to_hex()
                )
            })
    };
    Some(
        VerifiedBinary::install(&source, expected, install_dir)
            .await
            .expect("HYPHAE_TEST_BIN must match the hash it was just selected against"),
    )
}

#[tokio::test]
async fn importing_a_real_old_home_round_trips_identities_and_outbox() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some(bin) = verified_bin_from_env(&tmp.path().join("bin")).await else {
        eprintln!(
            "skipping importing_a_real_old_home_round_trips_identities_and_outbox: \
             HYPHAE_TEST_BIN is not set"
        );
        return;
    };

    // Build a genuine "old", unmanaged Hyphae HOME: run the real binary
    // directly against it (not through agent24-comm's own CommState/router
    // — that is exactly what COMM-2a already covers in
    // `tests/router_lifecycle.rs`), creating one encrypted identity.
    let old_home = tmp.path().join("old-home");
    tokio::fs::create_dir_all(&old_home)
        .await
        .expect("create old-home");
    let old_home_runner = HyphaeRunner::new(bin.clone(), old_home.clone(), Duration::from_secs(15));
    let old_home_password = Password::new(b"comm-2b-real-bin-test-password".to_vec()).unwrap();
    let envelope = old_home_runner
        .run(Invocation {
            args: vec![
                OsString::from("identity"),
                OsString::from("create"),
                OsString::from("--nickname"),
                OsString::from("old-primary"),
            ],
            password: Some(old_home_password),
            timeout: None,
        })
        .await
        .expect("identity create against the old home should succeed");
    match envelope {
        agent24_comm::Envelope::Ok { .. } => {}
        agent24_comm::Envelope::Failed { error, message, .. } => {
            panic!("seeding the old home failed: {error}: {message}")
        }
    }

    // Codex 挑战 Medium #4 real-binary coverage: seed a contact and a
    // genuinely non-empty outbox in the old home too, so the
    // `{"entries": [...]}`-shaped `outbox.json` the real `hyphae` binary
    // actually writes gets exercised end to end by this test, not just by
    // the fake-script-driven unit tests in `src/import.rs`. `bob`'s npub
    // below is a real, checksum-valid bech32 npub (generated once with
    // `hyphae identity create` against a throwaway second HOME while
    // writing this test) — any well-formed npub works for `contact add`,
    // it never has to resolve to a live identity.
    let envelope = old_home_runner
        .run(Invocation {
            args: vec![
                OsString::from("contact"),
                OsString::from("add"),
                OsString::from("--nickname"),
                OsString::from("bob"),
                OsString::from("--npub"),
                OsString::from("npub1kckqfw8hyqz0rgew5nq66k7uthcfskefzt3g305kgl5y89me5pxsajp9r3"),
            ],
            password: None,
            timeout: None,
        })
        .await
        .expect("contact add against the old home should succeed");
    match envelope {
        agent24_comm::Envelope::Ok { .. } => {}
        agent24_comm::Envelope::Failed { error, message, .. } => {
            panic!("seeding the old home's contact failed: {error}: {message}")
        }
    }

    // `agent msg` against a relay URL nothing is listening on fails the
    // publish immediately (connection refused) and queues the message for
    // retry — the real, fast way to get a genuinely non-empty
    // `outbox.json` out of the real binary without any network
    // dependency (confirmed against the locked Hyphae source: `agent msg`
    // always calls `AddToOutbox` before attempting to publish).
    let msg_password = Password::new(b"comm-2b-real-bin-test-password".to_vec()).unwrap();
    let envelope = old_home_runner
        .run(Invocation {
            args: vec![
                OsString::from("agent"),
                OsString::from("msg"),
                OsString::from("--from"),
                OsString::from("old-primary"),
                OsString::from("--to"),
                OsString::from("bob"),
                OsString::from("--content"),
                OsString::from("hello from COMM-2b's real-binary test"),
                OsString::from("--relay"),
                OsString::from("ws://127.0.0.1:1"),
            ],
            password: Some(msg_password),
            timeout: None,
        })
        .await
        .expect("agent msg against the old home should succeed (and queue for retry)");
    match envelope {
        agent24_comm::Envelope::Ok { .. } => {}
        agent24_comm::Envelope::Failed { error, message, .. } => {
            panic!("seeding the old home's outbox failed: {error}: {message}")
        }
    }

    // Now import it into a fresh, comm-managed home.
    let comm_dir = tmp.path().join("comm");
    let new_home = comm_dir.join("hyphae-home");
    tokio::fs::create_dir_all(&new_home)
        .await
        .expect("create the new (comm-managed) home");
    // This runner's own `home` is never read by `import` directly (every
    // actual invocation goes through a derived `with_home`), but it still
    // needs to point at the same verified binary — `bin.clone()` since a
    // third runner, pointed at the imported home, is built below from the
    // same verified copy.
    let runner = HyphaeRunner::new(
        bin.clone(),
        tmp.path().join("unused-home"),
        Duration::from_secs(15),
    );
    let store = MemoryPasswordStore::new();

    let report = import(
        &runner,
        &store,
        &new_home,
        ImportRequest {
            from: old_home.clone(),
            confirm: true,
            password: Some(Password::new(b"comm-2b-real-bin-test-password".to_vec()).unwrap()),
            dry_run: false,
        },
    )
    .await
    .expect("import against a freshly-seeded real old home should succeed");

    assert_eq!(report.identities, 1, "{report:?}");
    // Codex 挑战 Medium #4: now genuinely non-empty — this is what
    // exercises the real `{"entries": [...]}`-shaped `outbox.json` the
    // real binary writes against `count_source_outbox_entries`'s
    // cross-check end to end. A `report.outbox == 0` fixture (the
    // original version of this test) would never have caught the format
    // bug: a top-level object with zero "visible" entries and "not an
    // array at all" both happen to compare equal to Hyphae's own
    // (correctly empty) `storage outbox list` count by sheer coincidence.
    assert_eq!(report.contacts, 1, "{report:?}");
    assert_eq!(report.outbox, 1, "{report:?}");

    // The imported identity must be usable from the new home too.
    let new_home_runner = HyphaeRunner::new(bin, new_home.clone(), Duration::from_secs(15));
    let envelope = new_home_runner
        .run(Invocation {
            args: vec![OsString::from("identity"), OsString::from("list")],
            password: None,
            timeout: None,
        })
        .await
        .expect("identity list against the imported home should succeed");
    match envelope {
        agent24_comm::Envelope::Ok { data } => {
            let identities = data.as_array().expect("identity list is an array");
            assert_eq!(identities.len(), 1, "{identities:?}");
            assert_eq!(identities[0]["nickname"], "old-primary");
            assert_eq!(identities[0]["encrypted"], true);
        }
        agent24_comm::Envelope::Failed { error, message, .. } => {
            panic!("identity list against the imported home failed: {error}: {message}")
        }
    }

    // And the imported contact + outbox entry must both be visible from
    // the new, comm-managed home — not just counted in the report.
    let envelope = new_home_runner
        .run(Invocation {
            args: vec![OsString::from("contact"), OsString::from("list")],
            password: None,
            timeout: None,
        })
        .await
        .expect("contact list against the imported home should succeed");
    match envelope {
        agent24_comm::Envelope::Ok { data } => {
            let contacts = data.as_array().expect("contact list is an array");
            assert_eq!(contacts.len(), 1, "{contacts:?}");
            assert_eq!(contacts[0]["nickname"], "bob");
        }
        agent24_comm::Envelope::Failed { error, message, .. } => {
            panic!("contact list against the imported home failed: {error}: {message}")
        }
    }
    let envelope = new_home_runner
        .run(Invocation {
            args: vec![
                OsString::from("storage"),
                OsString::from("outbox"),
                OsString::from("list"),
            ],
            password: None,
            timeout: None,
        })
        .await
        .expect("storage outbox list against the imported home should succeed");
    match envelope {
        agent24_comm::Envelope::Ok { data } => {
            let outbox = data.as_array().expect("storage outbox list is an array");
            assert_eq!(outbox.len(), 1, "{outbox:?}");
            assert_eq!(
                outbox[0]["recipient_npub"],
                "npub1kckqfw8hyqz0rgew5nq66k7uthcfskefzt3g305kgl5y89me5pxsajp9r3"
            );
        }
        agent24_comm::Envelope::Failed { error, message, .. } => {
            panic!("storage outbox list against the imported home failed: {error}: {message}")
        }
    }

    // And the password must have been filed under the keystore's salt
    // account — reading it back proves `import` really did write it, not
    // just that the Hyphae-side check passed.
    let keystore_bytes = tokio::fs::read(new_home.join(".hyphae").join("keystore.json"))
        .await
        .expect("imported keystore.json should be readable");
    let keystore: serde_json::Value =
        serde_json::from_slice(&keystore_bytes).expect("keystore.json is valid json");
    let salt = keystore
        .get("salt")
        .and_then(serde_json::Value::as_str)
        .expect("keystore.json has a salt field");
    let account = Account::from_salt(salt);
    assert!(
        store.get(&account).await.is_ok(),
        "the imported keystore's password must be filed under its salt account"
    );
}
