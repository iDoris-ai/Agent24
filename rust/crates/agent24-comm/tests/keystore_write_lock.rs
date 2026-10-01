//! COMM-1b: proves (and then fixes) the concurrent-keystore-write race
//! `KeystoreWriteLock` exists to close (COMM-HYPHAE.md §3, H3/G8).
//!
//! Requires `HYPHAE_TEST_BIN` (see `tests/real_binary.rs`); without it both
//! tests print why they're skipping and return early. Neither test ever
//! touches the real `~/.hyphae` — `HOME` is always a throwaway `tempdir()`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    Envelope, HyphaeLock, HyphaeRunner, Invocation, RunnerError, Sha256Digest, VerifiedBinary,
    current_platform,
};

/// The Hyphae proposal's (unreproducible) acceptance binary — see
/// `tests/real_binary.rs`. Accepted here too, via the same test-only lock
/// override, alongside whatever the embedded `hyphae.lock.json` itself
/// already recognizes for the current platform: `HYPHAE_TEST_BIN` may be
/// either one, and both are legitimate "a real Hyphae binary" for this
/// test's purposes (it only needs a real `identity create`/`identity list`,
/// not a specific provenance).
const REFERENCE_SHA256: &str = "bc30dcf7bcf8b5c1865a3e995518c2bdab224bd8d6a3a064c4d7fc780de5e2b7";
const CONCURRENT_CREATES: usize = 10;

/// Builds a runner against `HYPHAE_TEST_BIN`, or returns `None` (having
/// already explained why) so callers can bail out early exactly like
/// `tests/real_binary.rs` does.
async fn runner_from_env(home: PathBuf) -> Option<HyphaeRunner> {
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
        // Not the legacy unreproducible reference binary — fall back to
        // whatever this platform's embedded, lock-verified hash is (e.g. a
        // binary built straight from the lock's own recipe).
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

    let install_dir = home.parent().expect("home has a parent").join("bin");
    let bin = VerifiedBinary::install(&source, expected, &install_dir)
        .await
        .expect("HYPHAE_TEST_BIN must match the hash it was just selected against");
    Some(HyphaeRunner::new(bin, home, Duration::from_secs(15)))
}

async fn identity_list_count(runner: &HyphaeRunner) -> usize {
    let envelope = runner
        .run(Invocation {
            args: vec![OsString::from("identity"), OsString::from("list")],
            password: None,
            timeout: None,
        })
        .await
        .expect("identity list should succeed");
    match envelope {
        Envelope::Ok { data } => data.as_array().map(|a| a.len()).unwrap_or(0),
        Envelope::Failed { error, message, .. } => {
            panic!("identity list failed: {error}: {message}")
        }
    }
}

fn create_invocation(nickname: &str) -> Invocation {
    Invocation {
        args: vec![
            OsString::from("identity"),
            OsString::from("create"),
            OsString::from("--nickname"),
            OsString::from(nickname),
        ],
        password: None,
        timeout: None,
    }
}

/// Runs `CONCURRENT_CREATES` `identity create` calls truly concurrently
/// (real `tokio::spawn`ed tasks, not just interleaved `.await`s), each
/// either through `run_keystore_write` (locked) or `run` (unlocked).
async fn spawn_concurrent_creates(
    runner: Arc<HyphaeRunner>,
    prefix: &str,
    locked: bool,
) -> Vec<Result<Envelope, RunnerError>> {
    let mut handles = Vec::with_capacity(CONCURRENT_CREATES);
    for i in 0..CONCURRENT_CREATES {
        let runner = runner.clone();
        let nickname = format!("{prefix}-{i}");
        handles.push(tokio::spawn(async move {
            let inv = create_invocation(&nickname);
            if locked {
                runner.run_keystore_write(inv).await
            } else {
                runner.run(inv).await
            }
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.await.expect("task must not panic"));
    }
    results
}

/// **The fix**: every `identity create` goes through `run_keystore_write`,
/// which holds `KeystoreWriteLock` for the full spawn-to-exit lifetime of
/// each invocation. With callers serialized this way, each subsequent
/// `identity create` only starts after the previous one's `SaveKeyStore` has
/// already been flushed to disk, so there is no read-modify-write window for
/// two of them to race in. Not `#[ignore]`d: this is the regression test —
/// deterministic, and safe to run whenever `HYPHAE_TEST_BIN` happens to be
/// set (CI doesn't set it, so it just skips there, matching
/// `tests/real_binary.rs`'s existing convention).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_identity_create_with_lock_keeps_all_identities() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("hyphae-home");
    tokio::fs::create_dir_all(&home).await.expect("mkdir home");

    let Some(runner) = runner_from_env(home).await else {
        eprintln!(
            "skipping concurrent_identity_create_with_lock_keeps_all_identities: \
             HYPHAE_TEST_BIN is not set"
        );
        return;
    };
    let runner = Arc::new(runner);

    let results = spawn_concurrent_creates(runner.clone(), "locked", true).await;
    for (i, result) in results.iter().enumerate() {
        match result {
            Ok(Envelope::Ok { .. }) => {}
            other => panic!("identity create locked-{i} did not succeed: {other:?}"),
        }
    }

    let count = identity_list_count(&runner).await;
    assert_eq!(
        count, CONCURRENT_CREATES,
        "expected all {CONCURRENT_CREATES} identities to survive under the lock, found {count}"
    );
}

/// **The proof**: the same 10 concurrent `identity create` calls, but
/// through the *unlocked* `run` — reproducing the bug `KeystoreWriteLock`
/// exists to fix. Hyphae's `SaveKeyStore` (`internal/identity/keystore.go`)
/// is read-modify-write with no locking of its own: each of the 10 child
/// processes loads `keystore.json`, adds its own one identity to its own
/// in-memory copy, and writes the whole file back; whichever process's
/// atomic rename lands last wins outright, silently discarding every other
/// process's newly-created identity (and that identity's private key).
///
/// `#[ignore]`d rather than part of the default run: unlike the lock-fixed
/// test above, nothing here *should* be deterministic — the whole point is
/// that it depends on how much the 10 child processes' read/compute/write
/// windows overlap, which varies with OS scheduling load. Run explicitly
/// with `cargo test -p agent24-comm --test keystore_write_lock -- --ignored`
/// (with `HYPHAE_TEST_BIN` set) to reproduce.
///
/// Observed locally (darwin-arm64, `HYPHAE_TEST_BIN` = the lock-verified
/// darwin-arm64 binary), two ways:
/// - A plain shell loop spawning the binary directly (10 concurrent
///   `identity create` child processes against one fresh HOME, no Rust
///   involved at all), run 3 times: **2 of 10** identities survived, every
///   time.
/// - This test itself (`cargo test -- --ignored`), run 3 times: **1 of 10**
///   survived, every time.
///
/// Either way, well under all 10 — the assertion below only checks
/// `count < CONCURRENT_CREATES` (i.e. *some* loss happened) rather than
/// hard-coding a specific survivor count, since the exact number is a race
/// outcome (and visibly sensitive to how the 10 processes get spawned), not
/// a fixed property of the bug.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "demonstrates a real race; not deterministic enough for the default run"]
async fn concurrent_identity_create_without_lock_loses_identities() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("hyphae-home");
    tokio::fs::create_dir_all(&home).await.expect("mkdir home");

    let Some(runner) = runner_from_env(home).await else {
        eprintln!(
            "skipping concurrent_identity_create_without_lock_loses_identities: \
             HYPHAE_TEST_BIN is not set"
        );
        return;
    };
    let runner = Arc::new(runner);

    let results = spawn_concurrent_creates(runner.clone(), "unlocked", false).await;
    // Some individual creates may themselves fail (e.g. a `rename` racing a
    // concurrent reader) rather than just silently lose to a later writer;
    // either way is evidence of the same missing-lock bug, so only log it.
    for (i, result) in results.into_iter().enumerate() {
        if let Err(e) = result {
            eprintln!("unlocked-{i} create itself errored (also a symptom): {e}");
        }
    }

    let count = identity_list_count(&runner).await;
    eprintln!("without KeystoreWriteLock: {count}/{CONCURRENT_CREATES} identities survived");
    assert!(
        count < CONCURRENT_CREATES,
        "expected the unlocked race to lose at least one identity, but all \
         {CONCURRENT_CREATES} survived this run — the race is real but isn't \
         guaranteed to reproduce on every run/machine; rerun a few times"
    );
}
