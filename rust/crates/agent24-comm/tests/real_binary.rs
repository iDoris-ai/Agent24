//! COMM-1a real-binary check: drives the actual Hyphae CLI (not a fake
//! shell script) through `HyphaeRunner`, in a throwaway `HOME`, to confirm
//! the runner's envelope parsing matches real-world output.
//!
//! Requires the `HYPHAE_TEST_BIN` environment variable to point at a Hyphae
//! binary (see the task's documented baseline sha256). Without it, this test
//! prints why it's skipping and returns early — it never touches the real
//! `~/.hyphae` and never falls back to a binary found some other way.
//!
//! `HYPHAE_TEST_BIN` may be either of two legitimate "a real Hyphae binary"
//! choices: the Hyphae proposal's own acceptance binary (sha256
//! `bc30dcf7bcf8b5c1865a3e995518c2bdab224bd8d6a3a064c4d7fc780de5e2b7`,
//! COMM-HYPHAE.md baseline — not built with the crate's reproducible recipe,
//! so it is intentionally *not* in `hyphae.lock.json`, and is instead
//! substituted in via the test-only `HyphaeLock::override_for_test`, which
//! only compiles under `cfg(test)` / the `test-lock-override` feature and
//! has no production code path), or a binary that *does* match the embedded
//! `hyphae.lock.json`'s entry for the current platform (e.g. one built
//! straight from the lock's own recipe — COMM-1b's `hyphae-src` build did
//! exactly this for `darwin-arm64`/`linux-x64` and matched byte-for-byte).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use agent24_comm::{
    Envelope, ExitClass, HyphaeLock, HyphaeRunner, Invocation, Password, Sha256Digest,
    current_platform,
};

/// The Hyphae proposal's acceptance binary, as documented in
/// COMM-HYPHAE.md's baseline (source `a4aa606eb81d...`, `hyphae version dev`).
/// Not reproducibly built, so it lives only here — never in the embedded
/// lock — and is substituted in via the test-only lock override.
const REFERENCE_SHA256: &str = "bc30dcf7bcf8b5c1865a3e995518c2bdab224bd8d6a3a064c4d7fc780de5e2b7";

#[tokio::test]
async fn real_hyphae_binary_envelopes_round_trip() {
    let Ok(test_bin) = std::env::var("HYPHAE_TEST_BIN") else {
        eprintln!(
            "skipping real_hyphae_binary_envelopes_round_trip: HYPHAE_TEST_BIN is not set \
             (point it at a Hyphae binary matching sha256 {REFERENCE_SHA256} to run this test)"
        );
        return;
    };

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
        // Not the legacy unreproducible reference binary — accept it anyway
        // if it matches this platform's embedded, lock-verified hash.
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

    let tmp = tempfile::tempdir().expect("tempdir");
    // Never the real ~/.hyphae: HOME is forced to this throwaway directory,
    // and HyphaeRunner never reads or sets the ambient HOME itself.
    let home = tmp.path().join("hyphae-home");
    tokio::fs::create_dir_all(&home)
        .await
        .expect("create hyphae-home");
    let install_dir = tmp.path().join("bin");

    let bin = agent24_comm::VerifiedBinary::install(&source, expected, &install_dir)
        .await
        .expect("HYPHAE_TEST_BIN must match the hash it was just selected against");

    let runner = HyphaeRunner::new(bin, home, Duration::from_secs(15));

    // 1. `identity list` on a fresh HOME succeeds with an empty list.
    let envelope = runner
        .run(Invocation {
            args: vec![OsString::from("identity"), OsString::from("list")],
            password: None,
            timeout: None,
        })
        .await
        .expect("identity list should succeed");
    match envelope {
        Envelope::Ok { data } => assert!(
            data.as_array().is_some_and(|a| a.is_empty()),
            "expected an empty identity list, got {data:?}"
        ),
        Envelope::Failed { error, message, .. } => {
            panic!("identity list failed: {error}: {message}")
        }
    }

    // 2. `identity create --nickname t1 --password-stdin` succeeds.
    let envelope = runner
        .run(Invocation {
            args: vec![
                OsString::from("identity"),
                OsString::from("create"),
                OsString::from("--nickname"),
                OsString::from("t1"),
            ],
            password: Some(Password::new(b"comm-1a-real-bin-test".to_vec()).unwrap()),
            timeout: None,
        })
        .await
        .expect("identity create should succeed");
    match envelope {
        Envelope::Ok { data } => {
            assert_eq!(
                data.get("nickname").and_then(|v| v.as_str()),
                Some("t1"),
                "unexpected identity create data: {data:?}"
            );
        }
        Envelope::Failed { error, message, .. } => {
            panic!("identity create failed: {error}: {message}")
        }
    }

    // 3. A second `identity create` without a password must fail as
    //    `auth_error` (exit code 3).
    let envelope = runner
        .run(Invocation {
            args: vec![
                OsString::from("identity"),
                OsString::from("create"),
                OsString::from("--nickname"),
                OsString::from("t2"),
            ],
            password: None,
            timeout: None,
        })
        .await
        .expect("identity create without a password should still parse as a valid envelope");
    match envelope {
        Envelope::Failed { exit, error, .. } => {
            assert_eq!(exit, ExitClass::AuthError);
            assert_eq!(error, "auth_error");
        }
        Envelope::Ok { data } => {
            panic!("expected auth_error without --password-stdin, got Ok({data:?})")
        }
    }

    // 4. `contact add` with an invalid npub must fail as `other_error`
    //    (exit code 4) — Hyphae classifies this as other_error, not
    //    user_error (COMM-HYPHAE.md G6).
    let envelope = runner
        .run(Invocation {
            args: vec![
                OsString::from("contact"),
                OsString::from("add"),
                OsString::from("--nickname"),
                OsString::from("bob"),
                OsString::from("--npub"),
                OsString::from("not-a-valid-npub"),
            ],
            password: None,
            timeout: None,
        })
        .await
        .expect("contact add with a bad npub should still parse as a valid envelope");
    match envelope {
        Envelope::Failed { exit, error, .. } => {
            assert_eq!(exit, ExitClass::OtherError);
            assert_eq!(error, "other_error");
        }
        Envelope::Ok { data } => {
            panic!("expected other_error for an invalid npub, got Ok({data:?})")
        }
    }
}
