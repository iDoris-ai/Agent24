//! COMM-2b: import an existing `~/.hyphae` HOME into comm's dedicated
//! Hyphae home (COMM-HYPHAE.md §4.1, D3; H2/H3 per §10's evaluation table).
//!
//! The whole flow holds [`crate::keystore_lock::KeystoreWriteLock`] (via
//! [`HyphaeRunner::keystore_lock`]) for its entire duration — this also
//! serializes concurrent `import` calls against each other, since both use
//! the same well-known staging path under `home`'s parent directory. On any
//! failure the staging directory is removed and the source is left
//! untouched: every mutation this module makes to `from` is read-only except
//! for one non-destructive side effect — a non-blocking `flock` on
//! `outbox.json.lock`, created with `O_CREAT` if it does not already exist
//! (mirroring Hyphae's own lazy creation of that file), which never touches
//! any of the six files whose bytes/mtimes this task's acceptance criteria
//! pin as unchanged.

use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rustix::fs::{FlockOperation, flock};
use serde_json::Value;

use crate::error::{CommError, map_envelope_failure, map_runner_error};
use crate::password::Password;
use crate::password_store::{Account, PasswordStore};
use crate::runner::{Envelope, HyphaeRunner, Invocation};

/// The six files COMM-HYPHAE.md §2/§4.1 names explicitly. Order is stable
/// and used for both the pre/post-copy fingerprint comparison and the
/// actual copy step.
const IMPORT_FILES: &[&str] = &[
    "keystore.json",
    "relays.json",
    "outbox.json",
    "messages.db",
    "messages.db-wal",
    "messages.db-shm",
];

/// A fresh, locally-unique suffix for the one-off verify HOME
/// (`<state_dir>/comm/verify-<rand>/`, §4.1 step 5). Not a secret — only
/// ever used as a throwaway directory name — so, like
/// `password_store::Account::new_pending`, plain OS-seeded randomness via
/// two independent `RandomState` hashes is enough, with no new dependency.
fn random_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    let a = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let b = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    format!("{a:016x}{b:016x}")
}

/// `POST /comm/import`'s request, already validated/decoded by `router.rs`.
pub struct ImportRequest {
    /// The old Hyphae HOME to import — the directory that USED to be
    /// `$HOME` when `hyphae` ran unmanaged, i.e. the parent of its own
    /// `.hyphae/` (not `.hyphae` itself).
    pub from: PathBuf,
    /// Must be `true`, checked first, before anything else runs (§4's
    /// `confirm_required` row: import is always a confirm-required
    /// operation, `dry_run` or not — a caller previewing counts still has
    /// to explicitly ask for the preview to run).
    pub confirm: bool,
    /// The imported keystore's own password. Required unless `dry_run`
    /// (step 5 never runs for a dry run, so there is nothing to verify it
    /// against).
    pub password: Option<Password>,
    /// When `true`, only steps 1–4 run (validate, lock+copy into a staging
    /// HOME, verify) and the report is returned without ever touching the
    /// password store or `home` itself — nothing is committed.
    pub dry_run: bool,
}

/// `{identities,contacts,outbox,db_files}` — §4's `POST /comm/import` data
/// shape.
#[derive(Debug, Clone)]
pub struct ImportReport {
    pub identities: usize,
    pub contacts: usize,
    pub outbox: usize,
    /// Which of `messages.db`/`messages.db-wal`/`messages.db-shm` were
    /// actually present in the source and copied.
    pub db_files: Vec<String>,
}

/// Runs the full import flow (COMM-HYPHAE.md §4.1) against `home` (comm's
/// real Hyphae HOME — the same path `runner` already points at). `runner`
/// is only used for its [`HyphaeRunner::keystore_lock`] and as the template
/// [`HyphaeRunner::with_home`] clones from; every actual Hyphae invocation
/// this function makes runs against a throwaway staging/verify HOME, never
/// against `home` itself (nothing needs Hyphae's help to `rename` a
/// directory).
pub async fn import(
    runner: &HyphaeRunner,
    password_store: &dyn PasswordStore,
    home: &Path,
    req: ImportRequest,
) -> Result<ImportReport, CommError> {
    if !req.confirm {
        return Err(CommError::ConfirmRequired);
    }
    let comm_dir = home
        .parent()
        .ok_or_else(|| CommError::Upstream("comm home has no parent directory".to_owned()))?
        .to_path_buf();
    // A single, well-known staging path (not a random one): the whole flow
    // runs under `KeystoreWriteLock`, which already serializes concurrent
    // imports against each other, so there is never more than one writer to
    // this path at a time.
    let staging_home = comm_dir.join("hyphae-home.staging");

    let _guard = runner.keystore_lock().acquire().await;
    let result = run_import(runner, password_store, home, &comm_dir, &staging_home, req).await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&staging_home).await;
    }
    result
}

async fn run_import(
    runner: &HyphaeRunner,
    password_store: &dyn PasswordStore,
    home: &Path,
    comm_dir: &Path,
    staging_home: &Path,
    req: ImportRequest,
) -> Result<ImportReport, CommError> {
    // A stale `hyphae-home.staging` can only survive here if a PREVIOUS
    // process was killed mid-import, after this function's own cleanup had
    // no chance to run (the `import()` wrapper's cleanup-on-error only
    // fires for an error returned to IT, not a hard kill). Wiping it before
    // doing anything else matters because `copy_import_files` SKIPS a file
    // the new source doesn't have (so a stale leftover, e.g. an
    // `outbox.json` from a crashed run whose source legitimately had one,
    // would otherwise silently survive into THIS import's result even
    // though the current source doesn't have one at all).
    let _ = tokio::fs::remove_dir_all(staging_home).await;

    // ---- step 1: path validation ---------------------------------------
    // The symlink/owner check on `from` itself MUST run on the path as
    // given, before `canonicalize` — canonicalize resolves away every
    // symlink in the path, including a symlinked final component, which
    // would make "the source directory is a symlink" unreachable if lstat
    // only ever saw the already-resolved result (COMM-2b's own acceptance
    // criterion requires this rejection to actually fire).
    check_not_symlink_and_owned(&req.from).await?;
    let source_root = tokio::fs::canonicalize(&req.from).await.map_err(|e| {
        CommError::Invalid(format!(
            "from {:?} is not a readable directory: {e}",
            req.from
        ))
    })?;
    let source_hyphae = source_root.join(".hyphae");
    check_not_symlink_and_owned(&source_hyphae).await?;
    for name in IMPORT_FILES {
        let p = source_hyphae.join(name);
        if path_exists(&p).await {
            check_not_symlink_and_owned(&p).await?;
        }
    }
    ensure_target_empty(home).await?;

    // ---- step 2: occupation probe + flock, held across the copy --------
    let lock_file = try_lock_outbox(source_hyphae.join("outbox.json.lock")).await?;
    let staging_hyphae = staging_home.join(".hyphae");
    let mut before = fingerprint_all(&source_hyphae).await?;
    let mut db_files = copy_import_files(&source_hyphae, &staging_hyphae).await?;
    let mut after = fingerprint_all(&source_hyphae).await?;
    if after != before {
        before = after;
        db_files = copy_import_files(&source_hyphae, &staging_hyphae).await?;
        after = fingerprint_all(&source_hyphae).await?;
        if after != before {
            drop(lock_file);
            return Err(CommError::Conflict(
                "source_changed: the source .hyphae directory changed while it was being \
                 imported; nothing was committed, try again once nothing else is using it"
                    .to_owned(),
            ));
        }
    }
    // Copy is done: release the source's outbox lock now, not held through
    // verification/commit below (COMM-HYPHAE.md §4.1 step 2: "成功则一直持
    // 有到复制结束").
    drop(lock_file);

    // ---- step 4: verify against the staging HOME ------------------------
    let staging_runner = runner.with_home(staging_home.to_path_buf());

    let identities = run_list(&staging_runner, &["identity", "list"]).await?;
    for identity in &identities {
        let encrypted = identity
            .get("encrypted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !encrypted {
            return Err(CommError::NotConfigured(
                "源 keystore 未加密。请先在源目录用终端执行 `HOME=<源 HOME> hyphae identity \
                 change-password` 设置口令，再重新导入"
                    .to_owned(),
            ));
        }
    }

    let contacts = run_list(&staging_runner, &["contact", "list"]).await?;
    let outbox = run_list(&staging_runner, &["storage", "outbox", "list"]).await?;

    if let Some(expected) = count_source_outbox_entries(&source_hyphae).await?
        && expected != outbox.len()
    {
        return Err(CommError::Upstream(format!(
            "outbox entry count mismatch after import: source outbox.json has {expected} \
             entries, hyphae reports {}",
            outbox.len()
        )));
    }

    match staging_runner
        .run(read_invocation(&["history", "inbox", "--limit", "1"]))
        .await
        .map_err(map_runner_error)?
    {
        Envelope::Ok { .. } => {}
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => return Err(map_envelope_failure(&error, &message, data)),
    }

    let report = ImportReport {
        identities: identities.len(),
        contacts: contacts.len(),
        outbox: outbox.len(),
        db_files: db_files
            .into_iter()
            .filter(|n| n.starts_with("messages.db"))
            .collect(),
    };

    if req.dry_run {
        let _ = tokio::fs::remove_dir_all(staging_home).await;
        return Ok(report);
    }

    // ---- step 5: password verification, then (only now) the keychain ---
    let password = req.password.ok_or_else(|| {
        CommError::Invalid(
            "password is required to complete the import (omit it only together with dry_run)"
                .to_owned(),
        )
    })?;
    let salt = read_keystore_salt(staging_home).await?.ok_or_else(|| {
        CommError::Invalid("the imported keystore.json has no salt field".to_owned())
    })?;

    let verify_home = comm_dir.join(format!("verify-{}", random_suffix()));
    verify_password(runner, &staging_hyphae, &verify_home, &password).await?;

    // Only reached once the password has been confirmed correct — a wrong
    // password returns above, before this ever runs, so the keystore
    // password store never gains an entry for a wrong password (COMM-2b's
    // own acceptance criterion).
    let account = Account::from_salt(&salt);
    password_store
        .put(&account, &password)
        .await
        .map_err(crate::error::map_store_error)?;

    // ---- step 6: commit ---------------------------------------------------
    tokio::fs::create_dir_all(home)
        .await
        .map_err(|e| CommError::Upstream(format!("creating {}: {e}", home.display())))?;
    let home_hyphae = home.join(".hyphae");
    tokio::fs::rename(&staging_hyphae, &home_hyphae)
        .await
        .map_err(|e| CommError::Upstream(format!("finalizing import (rename): {e}")))?;
    let _ = tokio::fs::remove_dir_all(staging_home).await;

    Ok(report)
}

/// Copies only `keystore.json` from the already-staged `.hyphae` directory
/// into a one-off HOME, runs `identity create --nickname __verify
/// --password-stdin` there, and always removes the one-off HOME afterward —
/// regardless of whether the password was right. Exit 0 confirms the
/// password; `auth_error` (exit 3) means it was wrong (COMM-HYPHAE.md §4.1
/// step 5, G1).
async fn verify_password(
    runner: &HyphaeRunner,
    staging_hyphae: &Path,
    verify_home: &Path,
    password: &Password,
) -> Result<(), CommError> {
    let verify_hyphae = verify_home.join(".hyphae");
    let result = async {
        tokio::fs::create_dir_all(&verify_hyphae)
            .await
            .map_err(|e| {
                CommError::Upstream(format!("creating {}: {e}", verify_hyphae.display()))
            })?;
        tokio::fs::copy(
            staging_hyphae.join("keystore.json"),
            verify_hyphae.join("keystore.json"),
        )
        .await
        .map_err(|e| CommError::Upstream(format!("staging the verify keystore: {e}")))?;

        let verify_runner = runner.with_home(verify_home.to_path_buf());
        // A fresh `Password` built from the same bytes: `Invocation` takes
        // ownership (it writes the bytes to the child's stdin), but the
        // caller still needs the original `password` afterward to file it
        // under the keychain — `Password` deliberately has no `Clone` (to
        // keep copies of the secret to a minimum), so this is the one place
        // its bytes are duplicated, and only in memory that is itself
        // zeroized on drop.
        let password_for_invocation =
            Password::new(password.as_bytes().to_vec()).map_err(crate::error::map_runner_error)?;
        verify_runner
            .run(Invocation {
                args: vec![
                    OsString::from("identity"),
                    OsString::from("create"),
                    OsString::from("--nickname"),
                    OsString::from("__verify"),
                ],
                password: Some(password_for_invocation),
                timeout: None,
            })
            .await
            .map_err(crate::error::map_runner_error)
    }
    .await;

    let _ = tokio::fs::remove_dir_all(verify_home).await;

    match result? {
        Envelope::Ok { .. } => Ok(()),
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => Err(map_envelope_failure(&error, &message, data)),
    }
}

// ---------------------------------------------------------------------
// path validation
// ---------------------------------------------------------------------

async fn path_exists(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path).await.is_ok()
}

/// Rejects a symlink or a path not owned by the current process's uid.
/// Called only on paths that are confirmed to exist — a missing optional
/// file (e.g. `outbox.json` before anything has ever been queued) is simply
/// skipped by the caller.
async fn check_not_symlink_and_owned(path: &Path) -> Result<(), CommError> {
    let meta = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|e| CommError::Invalid(format!("cannot stat {path:?}: {e}")))?;
    if meta.file_type().is_symlink() {
        return Err(CommError::Invalid(format!(
            "{path:?} is a symlink; refusing to import through one"
        )));
    }
    let current_uid = rustix::process::getuid().as_raw();
    if meta.uid() != current_uid {
        return Err(CommError::Invalid(format!(
            "{path:?} is not owned by the current user (uid {current_uid}); refusing to import"
        )));
    }
    Ok(())
}

async fn ensure_target_empty(home: &Path) -> Result<(), CommError> {
    let target = home.join(".hyphae");
    match tokio::fs::read_dir(&target).await {
        Ok(mut entries) => {
            let has_entry = entries
                .next_entry()
                .await
                .map_err(|e| CommError::Upstream(format!("reading {target:?}: {e}")))?
                .is_some();
            if has_entry {
                return Err(CommError::Conflict(format!(
                    "{target:?} already exists and is not empty"
                )));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CommError::Upstream(format!("checking {target:?}: {e}"))),
    }
}

// ---------------------------------------------------------------------
// occupation probe
// ---------------------------------------------------------------------

/// Non-blockingly `flock`s `lock_path` (creating it with `O_CREAT` if it
/// does not exist yet, mirroring Hyphae's own lazy creation of
/// `outbox.json.lock`). Returns the open file — hold it for as long as the
/// lock must be held; dropping it releases the lock when the fd closes.
async fn try_lock_outbox(lock_path: PathBuf) -> Result<std::fs::File, CommError> {
    tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| CommError::Upstream(format!("opening {lock_path:?}: {e}")))?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(file),
            // `WOULDBLOCK` and `AGAIN` are the same errno value on every
            // platform this crate targets (macOS, Linux) — matching only one
            // avoids an unreachable-pattern warning.
            Err(rustix::io::Errno::WOULDBLOCK) => Err(CommError::Conflict(format!(
                "source_in_use: {lock_path:?} is already locked by another process; stop \
                     using that hyphae HOME before importing it"
            ))),
            Err(e) => Err(CommError::Upstream(format!("flock {lock_path:?}: {e}"))),
        }
    })
    .await
    .map_err(|e| CommError::Upstream(format!("lock task panicked: {e}")))?
}

// ---------------------------------------------------------------------
// fingerprint + copy
// ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Fingerprint {
    size: u64,
    mtime: Option<SystemTime>,
}

async fn fingerprint_all(source_hyphae: &Path) -> Result<Vec<Option<Fingerprint>>, CommError> {
    let mut out = Vec::with_capacity(IMPORT_FILES.len());
    for name in IMPORT_FILES {
        let p = source_hyphae.join(name);
        let fp = match tokio::fs::metadata(&p).await {
            Ok(m) => Some(Fingerprint {
                size: m.len(),
                mtime: m.modified().ok(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(CommError::Upstream(format!("stat {p:?}: {e}"))),
        };
        out.push(fp);
    }
    Ok(out)
}

/// Copies every present file in [`IMPORT_FILES`] from `source_hyphae` into
/// `dest_hyphae` (created if missing, mode 0700), setting each destination
/// file to mode 0600. Returns the names that were actually present and
/// copied (missing source files — e.g. `outbox.json` before anything has
/// ever been queued — are skipped, not an error).
async fn copy_import_files(
    source_hyphae: &Path,
    dest_hyphae: &Path,
) -> Result<Vec<String>, CommError> {
    tokio::fs::create_dir_all(dest_hyphae)
        .await
        .map_err(|e| CommError::Upstream(format!("creating {dest_hyphae:?}: {e}")))?;
    tokio::fs::set_permissions(dest_hyphae, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|e| CommError::Upstream(format!("chmod {dest_hyphae:?}: {e}")))?;

    let mut copied = Vec::new();
    for name in IMPORT_FILES {
        let src = source_hyphae.join(name);
        let dst = dest_hyphae.join(name);
        match tokio::fs::copy(&src, &dst).await {
            Ok(_) => {
                tokio::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o600))
                    .await
                    .map_err(|e| CommError::Upstream(format!("chmod {dst:?}: {e}")))?;
                copied.push((*name).to_owned());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(CommError::Upstream(format!("copying {src:?}: {e}"))),
        }
    }
    Ok(copied)
}

/// Parses the source's own `outbox.json` directly (independent of whatever
/// Hyphae itself reports) so the two can be cross-checked (H2's own
/// acceptance criterion: "导入前后 outbox 条目数一致"). `None` means the
/// file's shape couldn't be read as a plain JSON array — the cross-check is
/// then skipped rather than guessed at. A missing file counts as zero
/// entries (never queued, the normal case for a never-used Hyphae HOME).
async fn count_source_outbox_entries(source_hyphae: &Path) -> Result<Option<usize>, CommError> {
    let path = source_hyphae.join("outbox.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(0)),
        Err(e) => return Err(CommError::Upstream(format!("reading {path:?}: {e}"))),
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| CommError::Upstream(format!("{path:?} is not valid json: {e}")))?;
    Ok(value.as_array().map(Vec::len))
}

/// `keystore.json`'s `salt` field, read directly from the (already copied)
/// staging home — the same single field `router.rs`'s own
/// `read_keystore_salt` reads from the live home, duplicated here rather
/// than shared across modules to keep this file's only coupling to
/// `router.rs` at the public `CommState`/`CommError` surface.
async fn read_keystore_salt(staging_home: &Path) -> Result<Option<String>, CommError> {
    let path = staging_home.join(".hyphae").join("keystore.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(CommError::Upstream(format!("reading {path:?}: {e}"))),
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| CommError::Upstream(format!("{path:?} is not valid json: {e}")))?;
    Ok(value.get("salt").and_then(Value::as_str).map(str::to_owned))
}

// ---------------------------------------------------------------------
// small runner helpers (mirrors router.rs's own, duplicated rather than
// made `pub(crate)` there to keep this module's coupling to router.rs
// minimal)
// ---------------------------------------------------------------------

fn read_invocation(parts: &[&str]) -> Invocation {
    Invocation {
        args: parts.iter().map(|p| OsString::from(*p)).collect(),
        password: None,
        timeout: None,
    }
}

/// Runs a read-only `list`-shaped command against `runner` and returns its
/// `data` array (or a `CommError` for anything else — a non-array `data`,
/// or a failed envelope).
async fn run_list(runner: &HyphaeRunner, argv: &[&str]) -> Result<Vec<Value>, CommError> {
    match runner
        .run(read_invocation(argv))
        .await
        .map_err(map_runner_error)?
    {
        Envelope::Ok { data } => Ok(data.as_array().cloned().unwrap_or_default()),
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => Err(map_envelope_failure(&error, &message, data)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::time::Duration;

    use super::*;
    use crate::binary::{VerifiedBinary, sha256_of};
    use crate::password_store::{MemoryPasswordStore, StoreError};

    async fn install_fixture(dir: &Path, script: &str) -> VerifiedBinary {
        let source_dir = dir.join("src");
        tokio::fs::create_dir_all(&source_dir).await.unwrap();
        let source = source_dir.join("hyphae-fake.sh");
        tokio::fs::write(&source, script).await.unwrap();
        let bytes = tokio::fs::read(&source).await.unwrap();
        let expected = sha256_of(&bytes);
        VerifiedBinary::install(&source, expected, &dir.join("bin"))
            .await
            .unwrap()
    }

    /// A fake `hyphae` that answers every command the import flow issues
    /// against a staging/verify HOME: `identity list` (one identity, whose
    /// `encrypted` flag is `encrypted`), `identity create --nickname
    /// __verify --password-stdin` (succeeds iff the piped password equals
    /// `correct_password`), `contact list` (one contact), `storage outbox
    /// list` (`outbox_len` synthetic entries), and `history inbox --limit
    /// 1` (always succeeds). Anything else fails loudly — nothing in this
    /// flow should ever call a command this fixture doesn't know about.
    fn fake_hyphae_script(encrypted: bool, correct_password: &str, outbox_len: usize) -> String {
        let outbox_items: String = (0..outbox_len)
            .map(|i| format!("{{\"event_id\":\"e{i}\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        const TEMPLATE: &str = r#"#!/bin/sh
case "$1 $2" in
"identity list")
  echo '{"ok":true,"data":[{"nickname":"a","npub":"npub1x","encrypted":__ENCRYPTED__}]}'
  ;;
"identity create")
  pw=$(cat)
  if [ "$pw" = "__PASSWORD__" ]; then
    echo '{"ok":true,"data":{"nickname":"__verify"}}'
  else
    echo '{"ok":false,"error":"auth_error","message":"incorrect password"}' >&2
    exit 3
  fi
  ;;
"contact list")
  echo '{"ok":true,"data":[{"nickname":"bob","npub":"npub1y"}]}'
  ;;
"storage outbox")
  echo '{"ok":true,"data":[__OUTBOX_ITEMS__]}'
  ;;
"history inbox")
  echo '{"ok":true,"data":[]}'
  ;;
*)
  echo '{"ok":false,"error":"other_error","message":"unhandled fake-hyphae command"}' >&2
  exit 4
  ;;
esac
"#;
        TEMPLATE
            .replace("__ENCRYPTED__", &encrypted.to_string())
            .replace("__PASSWORD__", correct_password)
            .replace("__OUTBOX_ITEMS__", &outbox_items)
    }

    /// A script that fails the test if it is ever invoked — used to prove a
    /// rejection happens before any subprocess runs.
    const NEVER_RUN_SCRIPT: &str = "#!/bin/sh\necho '{\"ok\":false,\"error\":\"other_error\",\"message\":\"must never run\"}' >&2\nexit 4\n";

    async fn build_runner(tmp: &Path, script: &str) -> HyphaeRunner {
        let bin = install_fixture(tmp, script).await;
        HyphaeRunner::new(bin, tmp.join("unused-runner-home"), Duration::from_secs(5))
    }

    /// Writes a synthetic old-style Hyphae HOME at `from/.hyphae/...` with
    /// the six tracked files populated with distinguishable byte content,
    /// plus `outbox.json` containing `outbox_len` entries (so the
    /// source-file-based cross-check in `count_source_outbox_entries` has
    /// something real to parse).
    async fn write_source_fixture(from: &Path, outbox_len: usize) {
        let hyphae_dir = from.join(".hyphae");
        tokio::fs::create_dir_all(&hyphae_dir).await.unwrap();
        tokio::fs::write(
            hyphae_dir.join("keystore.json"),
            br#"{"salt":"c2FsdA==","identities":[{"nickname":"a"}]}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            hyphae_dir.join("relays.json"),
            br#"{"relays":["wss://relay.example"],"source":"config"}"#,
        )
        .await
        .unwrap();
        let outbox_items: Vec<String> = (0..outbox_len)
            .map(|i| format!("{{\"event_id\":\"e{i}\"}}"))
            .collect();
        tokio::fs::write(
            hyphae_dir.join("outbox.json"),
            format!("[{}]", outbox_items.join(",")),
        )
        .await
        .unwrap();
        tokio::fs::write(hyphae_dir.join("messages.db"), b"sqlite-fixture-bytes")
            .await
            .unwrap();
        tokio::fs::write(hyphae_dir.join("messages.db-wal"), b"wal-fixture-bytes")
            .await
            .unwrap();
        tokio::fs::write(hyphae_dir.join("messages.db-shm"), b"shm-fixture-bytes")
            .await
            .unwrap();
    }

    async fn fingerprint_hashes(hyphae_dir: &Path) -> Vec<(String, Option<Vec<u8>>)> {
        let mut out = Vec::new();
        for name in IMPORT_FILES {
            let p = hyphae_dir.join(name);
            let hash = match tokio::fs::read(&p).await {
                Ok(bytes) => Some(sha256_of(&bytes).to_hex().into_bytes()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => panic!("reading {p:?}: {e}"),
            };
            out.push(((*name).to_owned(), hash));
        }
        out
    }

    // ---- confirm_required ------------------------------------------------

    #[tokio::test]
    async fn import_without_confirm_returns_confirm_required_before_any_subprocess_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = build_runner(tmp.path(), NEVER_RUN_SCRIPT).await;
        let store = MemoryPasswordStore::new();
        let home = tmp.path().join("comm").join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let err = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from: tmp.path().join("old-home"),
                confirm: false,
                password: None,
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CommError::ConfirmRequired), "{err:?}");
    }

    // ---- symlink / ownership rejection ------------------------------------

    #[tokio::test]
    async fn import_rejects_a_symlinked_source() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real-old-home");
        write_source_fixture(&real_dir, 0).await;
        let symlinked_from = tmp.path().join("old-home-symlink");
        std::os::unix::fs::symlink(&real_dir, &symlinked_from).unwrap();

        let runner = build_runner(tmp.path(), NEVER_RUN_SCRIPT).await;
        let store = MemoryPasswordStore::new();
        let home = tmp.path().join("comm").join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let err = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from: symlinked_from,
                confirm: true,
                password: Some(Password::new(b"irrelevant".to_vec()).unwrap()),
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CommError::Invalid(ref m) if m.contains("symlink")),
            "{err:?}"
        );
    }

    // ---- full round trip: hashes/mtimes/counts preserved ------------------

    #[tokio::test]
    async fn import_preserves_source_hashes_mtimes_and_reports_matching_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 2).await;
        let source_hyphae = from.join(".hyphae");

        let before = fingerprint_hashes(&source_hyphae).await;
        let before_meta = fingerprint_all(&source_hyphae).await.unwrap();

        let script = fake_hyphae_script(true, "correct-pass", 2);
        let runner = build_runner(tmp.path(), &script).await;
        let store = MemoryPasswordStore::new();
        let home = tmp.path().join("comm").join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let report = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from: from.clone(),
                confirm: true,
                password: Some(Password::new(b"correct-pass".to_vec()).unwrap()),
                dry_run: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(report.identities, 1);
        assert_eq!(report.contacts, 1);
        assert_eq!(report.outbox, 2);
        let mut db_files = report.db_files.clone();
        db_files.sort();
        assert_eq!(
            db_files,
            vec![
                "messages.db".to_owned(),
                "messages.db-shm".to_owned(),
                "messages.db-wal".to_owned(),
            ]
        );

        // The source's six files must be byte-for-byte and mtime-for-mtime
        // unchanged — the import never writes to anything but
        // `outbox.json.lock` (created here since the fixture doesn't ship
        // one), and that is not one of the six tracked files.
        let after = fingerprint_hashes(&source_hyphae).await;
        assert_eq!(before, after, "source file hashes changed across import");
        let after_meta = fingerprint_all(&source_hyphae).await.unwrap();
        assert_eq!(
            before_meta, after_meta,
            "source file (size, mtime) changed across import"
        );

        // And the final home actually has the imported keystore, proving
        // the staging->home rename landed.
        let committed = tokio::fs::read(home.join(".hyphae").join("keystore.json"))
            .await
            .unwrap();
        let original = tokio::fs::read(source_hyphae.join("keystore.json"))
            .await
            .unwrap();
        assert_eq!(committed, original);

        // The password must have been filed under the keystore's salt
        // account.
        let account = Account::from_salt("c2FsdA==");
        assert!(store.get(&account).await.is_ok());
    }

    // ---- dry_run: no commit, no password store write ----------------------

    #[tokio::test]
    async fn dry_run_reports_counts_without_writing_a_password_or_committing() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 3).await;

        let script = fake_hyphae_script(true, "correct-pass", 3);
        let runner = build_runner(tmp.path(), &script).await;
        let store = MemoryPasswordStore::new();
        let home = tmp.path().join("comm").join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let report = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from: from.clone(),
                confirm: true,
                password: None,
                dry_run: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(report.outbox, 3);

        assert!(
            !tokio::fs::try_exists(home.join(".hyphae"))
                .await
                .unwrap_or(false),
            "dry_run must never create the target .hyphae directory"
        );
        let account = Account::from_salt("c2FsdA==");
        assert!(matches!(
            store.get(&account).await.unwrap_err(),
            StoreError::NotFound
        ));
    }

    // ---- plaintext keystore rejected, with a change-password hint --------

    #[tokio::test]
    async fn plaintext_keystore_is_rejected_with_a_change_password_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;

        let script = fake_hyphae_script(false, "correct-pass", 0);
        let runner = build_runner(tmp.path(), &script).await;
        let store = MemoryPasswordStore::new();
        let home = tmp.path().join("comm").join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let err = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from,
                confirm: true,
                password: Some(Password::new(b"correct-pass".to_vec()).unwrap()),
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CommError::NotConfigured(ref m) if m.contains("change-password")),
            "{err:?}"
        );
    }

    // ---- wrong password: no keychain entry, auth_error surfaced ----------

    #[tokio::test]
    async fn wrong_password_leaves_no_new_keychain_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;

        let script = fake_hyphae_script(true, "correct-pass", 0);
        let runner = build_runner(tmp.path(), &script).await;
        let store = MemoryPasswordStore::new();
        let home = tmp.path().join("comm").join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let err = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from,
                confirm: true,
                password: Some(Password::new(b"wrong-password".to_vec()).unwrap()),
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CommError::Locked(_)), "{err:?}");

        let account = Account::from_salt("c2FsdA==");
        assert!(matches!(
            store.get(&account).await.unwrap_err(),
            StoreError::NotFound
        ));
        // Nothing must have been committed either.
        assert!(
            !tokio::fs::try_exists(home.join(".hyphae"))
                .await
                .unwrap_or(false)
        );
    }

    // ---- source_in_use: a held outbox.json.lock is respected --------------

    #[tokio::test]
    async fn import_reports_conflict_when_the_source_outbox_lock_is_already_held() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;
        let lock_path = from.join(".hyphae").join("outbox.json.lock");

        // Simulate "something else is using this HOME right now": hold a
        // non-blocking exclusive flock on the source's own outbox lock
        // file, from a SEPARATE open file description (flock is scoped to
        // the open file description, not the process, so this genuinely
        // conflicts with import's own, independent `open()` of the same
        // path).
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .unwrap();
        rustix::fs::flock(&held, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();

        let script = fake_hyphae_script(true, "correct-pass", 0);
        let runner = build_runner(tmp.path(), &script).await;
        let store = MemoryPasswordStore::new();
        let home = tmp.path().join("comm").join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let err = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from,
                confirm: true,
                password: Some(Password::new(b"correct-pass".to_vec()).unwrap()),
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CommError::Conflict(ref m) if m.contains("source_in_use")),
            "{err:?}"
        );
        drop(held);
    }

    // ---- target non-empty: conflict ---------------------------------------

    #[tokio::test]
    async fn import_rejects_a_non_empty_target_hyphae_home() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;

        let runner = build_runner(tmp.path(), NEVER_RUN_SCRIPT).await;
        let store = MemoryPasswordStore::new();
        let home = tmp.path().join("comm").join("hyphae-home");
        tokio::fs::create_dir_all(home.join(".hyphae"))
            .await
            .unwrap();
        tokio::fs::write(home.join(".hyphae").join("keystore.json"), b"already-here")
            .await
            .unwrap();

        let err = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from,
                confirm: true,
                password: Some(Password::new(b"correct-pass".to_vec()).unwrap()),
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CommError::Conflict(_)), "{err:?}");
    }

    // ---- a stale leftover staging dir (crashed previous import) must ------
    // ---- never leak a file the CURRENT source doesn't have ----------------

    #[tokio::test]
    async fn a_stale_staging_leftover_from_a_crashed_import_is_wiped_first() {
        let tmp = tempfile::tempdir().unwrap();
        // A genuinely minimal source: ONLY keystore.json + relays.json —
        // `write_source_fixture` always writes an `outbox.json` too (an
        // empty array when `outbox_len` is 0), which would make this
        // source indistinguishable from "has outbox.json". A fresh
        // `~/.hyphae` that has never queued a message really does lack the
        // file entirely (COMM-HYPHAE.md §2: "outbox.json 在首次入队时才会
        // 生成"), so this is also the realistic case.
        let from = tmp.path().join("old-home");
        let source_hyphae = from.join(".hyphae");
        tokio::fs::create_dir_all(&source_hyphae).await.unwrap();
        tokio::fs::write(
            source_hyphae.join("keystore.json"),
            br#"{"salt":"c2FsdA==","identities":[{"nickname":"a"}]}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            source_hyphae.join("relays.json"),
            br#"{"relays":[],"source":"default"}"#,
        )
        .await
        .unwrap();

        // Simulate a staging directory left behind by a PREVIOUS import that
        // was hard-killed before it could clean up — crucially, containing
        // an `outbox.json` the current source does not have.
        let comm_dir = tmp.path().join("comm");
        let stale_staging_hyphae = comm_dir.join("hyphae-home.staging").join(".hyphae");
        tokio::fs::create_dir_all(&stale_staging_hyphae)
            .await
            .unwrap();
        tokio::fs::write(
            stale_staging_hyphae.join("outbox.json"),
            br#"[{"event_id":"stale-leftover"}]"#,
        )
        .await
        .unwrap();

        let script = fake_hyphae_script(true, "correct-pass", 0);
        let runner = build_runner(tmp.path(), &script).await;
        let store = MemoryPasswordStore::new();
        let home = comm_dir.join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let report = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from,
                confirm: true,
                password: Some(Password::new(b"correct-pass".to_vec()).unwrap()),
                dry_run: false,
            },
        )
        .await
        .unwrap();

        // The stale `outbox.json` must not have survived into the commit —
        // without the fix, `copy_import_files` skips `outbox.json` (the
        // current source has none) and the stale leftover rides along into
        // the final home untouched.
        assert_eq!(report.outbox, 0, "{report:?}");
        assert!(
            !tokio::fs::try_exists(home.join(".hyphae").join("outbox.json"))
                .await
                .unwrap_or(false),
            "a stale outbox.json from a previous crashed import leaked into this one"
        );
    }
}
