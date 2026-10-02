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
//!
//! **Known limitation (PR #635 R4, 挂账待 Hyphae 版本升级)**: step 2 also
//! probes `daemon.lock` (see [`try_lock_daemon`]) — Hyphae's own
//! lifetime-of-the-process single-instance lock
//! (`internal/daemon/lock_unix.go`'s `acquireDaemonHomeLock`), as opposed to
//! `outbox.json.lock`, which Hyphae only holds for the duration of a single
//! outbox read/write and therefore cannot prove an otherwise-idle daemon
//! isn't still running. `daemon.lock` shipped in Hyphae
//! [#104](https://github.com/iDoris-ai/Hyphae/pull/104) (merged into
//! Hyphae's own `main` on 2026-10-01), but this crate is still pinned
//! (`hyphae.lock.json`) to source `a4aa606`, four commits BEHIND that merge
//! — a source HOME last touched by a Hyphae build that old has no
//! `daemon.lock` to find, so a currently-running old-Hyphae daemon on that
//! HOME is undetectable until the pin is upgraded past #104. Unlike the
//! outbox probe, `try_lock_daemon` never creates the file (no `O_CREAT`) —
//! a missing `daemon.lock` is treated as "nothing to probe", not an error,
//! precisely to stay compatible with every Hyphae HOME predating #104.
//! `ImportRequest`'s and the CLI's own docs tell the caller to stop the
//! source HOME's `hyphae` daemon before importing regardless — that's the
//! only defense against this gap until the pin moves.

use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rustix::fd::OwnedFd;
use rustix::fs::{FlockOperation, Mode, OFlags, flock};
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
    /// to explicitly ask for the preview to run). **Before setting this,
    /// stop the `hyphae` daemon that is using `from` as its HOME** (or
    /// confirm nothing else is running against it): step 2's occupation
    /// probes catch a daemon that is actively mid-operation on
    /// `outbox.json`, and — when `from` was last touched by a Hyphae build
    /// that ships `daemon.lock` (Hyphae #104 or later; this crate's own
    /// pinned source predates it, see the module doc's "known limitation"
    /// note) — an idle-but-running one too, but neither can prove a daemon
    /// mid-operation on `messages.db`/`keystore.json` outside those two
    /// files has fully stopped.
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

    // ---- step 1a: validate `from` before ANYTHING below touches
    // `staging_home` --------------------------------------------------
    // The symlink/owner check on `from` itself MUST run on the path as
    // given, before `canonicalize` — canonicalize resolves away every
    // symlink in the path, including a symlinked final component, which
    // would make "the source directory is a symlink" unreachable if lstat
    // only ever saw the already-resolved result (COMM-2b's own acceptance
    // criterion requires this rejection to actually fire).
    //
    // Codex 挑战 High #1: this whole block runs here, in `import()`,
    // BEFORE the lock is acquired and before `run_import` is ever called
    // — not only before `run_import`'s own stale-staging wipe, but also
    // before THIS function's own `result.is_err()` cleanup below, which
    // would otherwise still wipe `staging_home` whenever `run_import`
    // fails for any reason at all, including "because `from` pointed at
    // `staging_home` itself". Once this passes, `source_root` is handed
    // to `run_import` as an already-resolved path — `run_import` and the
    // fd-pinned opens inside it never trust this check's result for
    // anything past this point, though (Codex High #2): they each
    // independently reopen and re-validate `source_hyphae` and its files.
    check_not_symlink_and_owned(&req.from).await?;
    let source_root = tokio::fs::canonicalize(&req.from).await.map_err(|e| {
        CommError::Invalid(format!(
            "from {:?} is not a readable directory: {e}",
            req.from
        ))
    })?;
    reject_source_overlapping_comm_dir(&source_root, &comm_dir).await?;

    let _guard = runner.keystore_lock().acquire().await;
    let result = run_import(
        runner,
        password_store,
        home,
        &comm_dir,
        &staging_home,
        &source_root,
        req,
    )
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&staging_home).await;
    }
    result
}

#[cfg(test)]
mod copy_test_hook {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    use tokio::sync::Notify;

    struct Barrier {
        reached: Arc<Notify>,
        resume: Arc<Notify>,
    }

    fn barriers() -> &'static Mutex<HashMap<(PathBuf, u8), Barrier>> {
        static BARRIERS: OnceLock<Mutex<HashMap<(PathBuf, u8), Barrier>>> = OnceLock::new();
        BARRIERS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn locked_barriers() -> MutexGuard<'static, HashMap<(PathBuf, u8), Barrier>> {
        match barriers().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(super) fn install(source: &Path, attempt: u8) -> (Arc<Notify>, Arc<Notify>) {
        let reached = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        let previous = locked_barriers().insert(
            (source.to_path_buf(), attempt),
            Barrier {
                reached: Arc::clone(&reached),
                resume: Arc::clone(&resume),
            },
        );
        assert!(previous.is_none(), "duplicate import copy barrier");
        (reached, resume)
    }

    pub(super) async fn pause_after_copy(source: &Path, attempt: u8) {
        let barrier = locked_barriers().remove(&(source.to_path_buf(), attempt));
        let Some(barrier) = barrier else {
            return;
        };
        barrier.reached.notify_one();
        barrier.resume.notified().await;
    }
}

async fn run_import(
    runner: &HyphaeRunner,
    password_store: &dyn PasswordStore,
    home: &Path,
    comm_dir: &Path,
    staging_home: &Path,
    source_root: &Path,
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
    // though the current source doesn't have one at all). Safe to run now
    // that `from` is confirmed not to overlap `comm_dir`.
    let _ = tokio::fs::remove_dir_all(staging_home).await;

    let source_hyphae = source_root.join(".hyphae");
    // Codex 挑战 High #2: 这里的检查结果不会被后面任何一步「信任」——
    // `fingerprint_all`/`copy_import_files` 各自独立地用
    // `O_NOFOLLOW`+`fstat` 重新打开 `source_hyphae` 和六个文件，而不是
    // 「现在 stat 一下路径，后面再按同一个路径重新 open」。这里只是为了
    // 在拿锁、复制之前给出一个干净的早失败。
    drop(open_source_dir(&source_hyphae).await?);
    ensure_target_empty(home).await?;

    // ---- step 2: occupation probes + flocks, held across the copy -------
    // Two independent probes, both held until the copy (and its retry, if
    // any) is done:
    //   - `outbox.json.lock`: proves no Hyphae command is mid-outbox-op
    //     right now. Hyphae only holds this for the duration of a single
    //     outbox read/write, so it does NOT prove an idle daemon isn't
    //     running (R4 of PR #635's review).
    //   - `daemon.lock` (see `try_lock_daemon`'s doc): Hyphae's own
    //     lifetime-of-the-process single-instance lock, which DOES catch an
    //     idle daemon — but only on a Hyphae HOME last touched by a build
    //     that ships it (module doc's "known limitation").
    let outbox_lock = try_lock_outbox(source_hyphae.join("outbox.json.lock")).await?;
    let daemon_lock = try_lock_daemon(&source_hyphae).await?;
    let staging_hyphae = staging_home.join(".hyphae");
    let mut before = fingerprint_all(&source_hyphae).await?;
    let mut db_files = copy_import_files(&source_hyphae, &staging_hyphae).await?;
    #[cfg(test)]
    copy_test_hook::pause_after_copy(&source_hyphae, 1).await;
    let mut after = fingerprint_all(&source_hyphae).await?;
    if after != before {
        before = after;
        // Codex 挑战 Medium #3: 不清空直接重试，`copy_import_files` 只会
        // 跳过源里已经消失的文件（它从不删除目标里的旧文件），于是第一
        // 趟复制留下的过期文件（例如已被 checkpoint 掉的
        // messages.db-wal）会原样留在 staging 里，最终随 rename 混进提
        // 交结果。重试前整个清空 staging，让下面的 `copy_import_files`
        // 从零重建。
        let _ = tokio::fs::remove_dir_all(staging_home).await;
        db_files = copy_import_files(&source_hyphae, &staging_hyphae).await?;
        #[cfg(test)]
        copy_test_hook::pause_after_copy(&source_hyphae, 2).await;
        after = fingerprint_all(&source_hyphae).await?;
        if after != before {
            drop(outbox_lock);
            drop(daemon_lock);
            return Err(CommError::Conflict(
                "source_changed: the source .hyphae directory changed while it was being \
                 imported; nothing was committed, try again once nothing else is using it"
                    .to_owned(),
                None,
            ));
        }
    }
    // Codex 挑战 Medium #4: 在锁还持有的时候就记录源 outbox 的条目数快
    // 照，而不是等到锁释放、验证阶段才去读 —— 这样比较的才是刚刚复制进
    // staging 的那份字节，不是锁放开之后源目录可能已经变化的内容。
    let expected_outbox_entries = count_source_outbox_entries(&source_hyphae).await?;
    // Copy is done: release both source locks now, not held through
    // verification/commit below (COMM-HYPHAE.md §4.1 step 2: "成功则一直持
    // 有到复制结束").
    drop(outbox_lock);
    drop(daemon_lock);

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

    if expected_outbox_entries != outbox.len() {
        return Err(CommError::Upstream(format!(
            "outbox entry count mismatch after import: source outbox.json has \
             {expected_outbox_entries} entries, hyphae reports {}",
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

/// Rejects a symlink or a path not owned by the current process's uid.
/// Only ever called on `req.from` itself — the path as given by the
/// caller, before `canonicalize` — never again on any path derived from
/// it; everything below this point re-validates for itself on the actual
/// fd it is about to use (Codex 挑战 High #2).
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

/// Rejects a `from` whose canonical path coincides with, lies inside, or
/// contains `comm_dir` — the directory holding both this module's
/// well-known staging path (`hyphae-home.staging`) and the real `home`
/// itself. Codex 挑战 High #1: this MUST run before anything deletes
/// `staging_home` — a `from` that pointed at it (or anywhere else under
/// `comm_dir`) would otherwise have its own data wiped by the
/// stale-staging cleanup, before `from` had even been looked at.
/// `comm_dir` is canonicalized fresh here (never trusted from an earlier
/// call) and compared against the already-canonical `source_root` with
/// `Path::starts_with`, which only inspects components both sides already
/// resolved — no additional TOCTOU window.
async fn reject_source_overlapping_comm_dir(
    source_root: &Path,
    comm_dir: &Path,
) -> Result<(), CommError> {
    let comm_dir_canon = tokio::fs::canonicalize(comm_dir)
        .await
        .map_err(|e| CommError::Upstream(format!("canonicalizing {comm_dir:?}: {e}")))?;
    if source_root == comm_dir_canon
        || source_root.starts_with(&comm_dir_canon)
        || comm_dir_canon.starts_with(source_root)
    {
        return Err(CommError::Invalid(format!(
            "from {source_root:?} overlaps with comm's own state directory {comm_dir_canon:?} \
             (which holds both the real hyphae-home and this import's own staging area); \
             refusing to import from inside or above it"
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
                return Err(CommError::Conflict(
                    format!("{target:?} already exists and is not empty"),
                    None,
                ));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CommError::Upstream(format!("checking {target:?}: {e}"))),
    }
}

// ---------------------------------------------------------------------
// atomic, fd-pinned opens (Codex 挑战 High #2 / Medium #5)
//
// Every open below resolves its path exactly once and validates the exact
// fd it got back (via `fstat` on that fd, never a separate `stat()` on a
// path followed by a later, independently-resolved open) — closing the
// window where the source directory or one of its files could be swapped
// out between "check" and "use". `O_NOFOLLOW` turns a symlinked
// component into an open-time `ELOOP` instead of silently following it;
// `O_NONBLOCK` on the per-file opens means a FIFO planted at one of
// `IMPORT_FILES` (or at `outbox.json.lock`) makes `open()` fail outright
// instead of blocking forever and starving `KeystoreWriteLock` (Medium
// #5) — the `fstat` right after still re-confirms it is a regular file,
// since a non-blocking open of a FIFO *can* succeed immediately when a
// reader/writer is already present on the other end.
// ---------------------------------------------------------------------

/// Maps an `open`/`openat` errno to the `CommError` it should surface as.
/// Shared by every atomic open below — `NOENT` is handled by each caller
/// individually (it means "missing", not an error, for every optional
/// `IMPORT_FILES` entry).
fn open_errno_to_comm_error(e: rustix::io::Errno, path: &Path) -> CommError {
    match e {
        rustix::io::Errno::LOOP => CommError::Invalid(format!(
            "{path:?} is a symlink; refusing to import through one"
        )),
        rustix::io::Errno::NOTDIR => CommError::Invalid(format!("{path:?} is not a directory")),
        rustix::io::Errno::NOENT => CommError::Invalid(format!("{path:?} does not exist")),
        // `ENXIO`: opening a FIFO `O_WRONLY | O_NONBLOCK` with no reader on
        // the other end fails immediately instead of blocking (POSIX) —
        // exactly the FIFO case `try_lock_outbox` guards against (Codex
        // 挑战 Medium #5); a read-only open of the same FIFO would instead
        // succeed and get caught by the regular-file `fstat` check below.
        rustix::io::Errno::NXIO => CommError::Invalid(format!(
            "{path:?} is a FIFO with no reader; refusing to import"
        )),
        _ => CommError::Upstream(format!("opening {path:?}: {e}")),
    }
}

/// `fstat`s `fd` (not a path) and rejects unless it is owned by the
/// current uid.
fn reject_unless_owned(fd: &OwnedFd, path_for_error: &Path) -> Result<(), CommError> {
    let stat = rustix::fs::fstat(fd)
        .map_err(|e| CommError::Upstream(format!("fstat {path_for_error:?}: {e}")))?;
    let current_uid = rustix::process::getuid().as_raw();
    if stat.st_uid != current_uid {
        return Err(CommError::Invalid(format!(
            "{path_for_error:?} is not owned by the current user (uid {current_uid}); refusing \
             to import"
        )));
    }
    Ok(())
}

/// `fstat`s `fd` (not a path) and rejects unless it is a regular file —
/// in particular, not a FIFO (Codex 挑战 Medium #5).
fn reject_unless_regular_file(fd: &OwnedFd, path_for_error: &Path) -> Result<(), CommError> {
    let stat = rustix::fs::fstat(fd)
        .map_err(|e| CommError::Upstream(format!("fstat {path_for_error:?}: {e}")))?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
        return Err(CommError::Invalid(format!(
            "{path_for_error:?} is not a regular file (e.g. a FIFO); refusing to import"
        )));
    }
    Ok(())
}

/// Opens `source_hyphae` with `O_DIRECTORY | O_NOFOLLOW` (so a non-dir or
/// a symlink to one fails the open itself, atomically) and confirms via
/// `fstat` on the resulting fd that it is owned by the current uid.
fn open_source_dir_blocking(source_hyphae: &Path) -> Result<OwnedFd, CommError> {
    let fd = rustix::fs::open(
        source_hyphae,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| open_errno_to_comm_error(e, source_hyphae))?;
    reject_unless_owned(&fd, source_hyphae)?;
    Ok(fd)
}

/// Async wrapper around [`open_source_dir_blocking`] for callers that just
/// want the early, fail-fast validation (step 1) — the fd is dropped
/// immediately by the caller.
async fn open_source_dir(source_hyphae: &Path) -> Result<OwnedFd, CommError> {
    let source_hyphae = source_hyphae.to_path_buf();
    tokio::task::spawn_blocking(move || open_source_dir_blocking(&source_hyphae))
        .await
        .map_err(|e| CommError::Upstream(format!("open task panicked: {e}")))?
}

/// Opens `name` inside the directory `dir_fd` already points at, with
/// `O_NOFOLLOW | O_NONBLOCK`, and confirms via `fstat` on the resulting fd
/// that it is owned by the current uid and a regular file. Returns `None`
/// only for a genuinely missing file (the normal case for e.g.
/// `outbox.json` before anything has ever been queued) — every other
/// rejection is an error.
fn open_source_file(dir_fd: &OwnedFd, name: &str) -> Result<Option<std::fs::File>, CommError> {
    let name_path = Path::new(name);
    match rustix::fs::openat(
        dir_fd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => {
            reject_unless_owned(&fd, name_path)?;
            reject_unless_regular_file(&fd, name_path)?;
            Ok(Some(std::fs::File::from(fd)))
        }
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(e) => Err(open_errno_to_comm_error(e, name_path)),
    }
}

// ---------------------------------------------------------------------
// occupation probes
// ---------------------------------------------------------------------

/// Non-blockingly `flock`s `lock_path` (creating it with `O_CREAT` if it
/// does not exist yet, mirroring Hyphae's own lazy creation of
/// `outbox.json.lock`). `O_NOFOLLOW` rejects a symlinked lock path;
/// `O_NONBLOCK` plus the `fstat`-based regular-file check right after
/// (Codex 挑战 Medium #5) means a FIFO planted at this path can't block
/// this open forever and starve `KeystoreWriteLock`. Returns the open fd —
/// hold it for as long as the lock must be held; dropping it releases the
/// lock when the fd closes.
async fn try_lock_outbox(lock_path: PathBuf) -> Result<OwnedFd, CommError> {
    tokio::task::spawn_blocking(move || {
        let fd = rustix::fs::open(
            &lock_path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| open_errno_to_comm_error(e, &lock_path))?;
        reject_unless_regular_file(&fd, &lock_path)?;
        match flock(&fd, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(fd),
            // `WOULDBLOCK` and `AGAIN` are the same errno value on every
            // platform this crate targets (macOS, Linux) — matching only one
            // avoids an unreachable-pattern warning.
            Err(rustix::io::Errno::WOULDBLOCK) => Err(CommError::Conflict(
                format!(
                    "source_in_use: {lock_path:?} is already locked by another process; stop \
                     using that hyphae HOME before importing it"
                ),
                None,
            )),
            Err(e) => Err(CommError::Upstream(format!("flock {lock_path:?}: {e}"))),
        }
    })
    .await
    .map_err(|e| CommError::Upstream(format!("lock task panicked: {e}")))?
}

/// Best-effort probe for a still-running Hyphae daemon: a non-blocking
/// `flock(LOCK_EX)` on `daemon.lock`, mirroring Hyphae's own single-instance
/// lock (`internal/daemon/lock_unix.go`'s `acquireDaemonHomeLock`, confirmed
/// against Hyphae #104 — merged into Hyphae's own `main` 2026-10-01). Unlike
/// `try_lock_outbox`, this is held for the daemon's ENTIRE lifetime, not
/// just a single outbox op, so a successful lock here really does prove no
/// daemon is running against `source_hyphae` right now — PR #635's review
/// R4 pointed out `outbox.json.lock` alone cannot prove that for an
/// otherwise-idle daemon.
///
/// Deliberately never creates `daemon.lock` (no `O_CREAT`, unlike
/// `try_lock_outbox`'s lazy creation of `outbox.json.lock`): an old Hyphae
/// HOME that predates #104 simply has no such file, and this importer has
/// no business manufacturing Hyphae's own lock file inside someone else's
/// HOME. A missing file is `Ok(None)` — "nothing to probe, skip" — not an
/// error, which is exactly what keeps this compatible with every
/// pre-#104 Hyphae HOME (the module doc's "known limitation": this crate's
/// own pinned source, `a4aa606`, is itself one of those, so this probe
/// currently never actually fires against it). Reuses
/// `open_source_dir_blocking` and `open_source_file` for the same
/// `O_NOFOLLOW`/`O_NONBLOCK`/owner/regular-file checks every other atomic
/// open in this module makes (Codex 挑战 High #2 / Medium #5): a FIFO or
/// symlink planted at `daemon.lock` is rejected the same way, not blocked
/// on or followed.
async fn try_lock_daemon(source_hyphae: &Path) -> Result<Option<std::fs::File>, CommError> {
    let source_hyphae = source_hyphae.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let dir_fd = open_source_dir_blocking(&source_hyphae)?;
        let Some(file) = open_source_file(&dir_fd, "daemon.lock")? else {
            return Ok(None);
        };
        let lock_path = source_hyphae.join("daemon.lock");
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(file)),
            Err(rustix::io::Errno::WOULDBLOCK) => Err(CommError::Conflict(
                format!(
                    "source_in_use: {lock_path:?} is held — a hyphae daemon appears to still be \
                     running against this HOME; stop it before importing"
                ),
                None,
            )),
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

/// Fingerprints every entry in [`IMPORT_FILES`] by opening the source
/// directory and each file atomically (see the "atomic, fd-pinned opens"
/// section above) rather than `stat()`-ing a path — each call here is a
/// fresh, independently-validated snapshot, which is what lets the
/// before/after comparison in `run_import` actually detect a directory or
/// file swapped out between the two calls, rather than trusting a check
/// made long before.
async fn fingerprint_all(source_hyphae: &Path) -> Result<Vec<Option<Fingerprint>>, CommError> {
    let source_hyphae = source_hyphae.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let dir_fd = open_source_dir_blocking(&source_hyphae)?;
        let mut out = Vec::with_capacity(IMPORT_FILES.len());
        for name in IMPORT_FILES {
            let fp = match open_source_file(&dir_fd, name)? {
                Some(file) => {
                    let meta = file.metadata().map_err(|e| {
                        CommError::Upstream(format!("fstat (metadata) {name}: {e}"))
                    })?;
                    Some(Fingerprint {
                        size: meta.len(),
                        mtime: meta.modified().ok(),
                    })
                }
                None => None,
            };
            out.push(fp);
        }
        Ok(out)
    })
    .await
    .map_err(|e| CommError::Upstream(format!("fingerprint task panicked: {e}")))?
}

/// Copies every present file in [`IMPORT_FILES`] from `source_hyphae` into
/// `dest_hyphae` (created if missing, mode 0700), setting each destination
/// file to mode 0600. Returns the names that were actually present and
/// copied (missing source files — e.g. `outbox.json` before anything has
/// ever been queued — are skipped, not an error).
///
/// `dest_hyphae` is expected to be empty before this runs — `run_import`
/// guarantees that by wiping `staging_home` wholesale before the first
/// call, and again before any retry (Codex 挑战 Medium #3) — this
/// function itself only ever adds files, never removes one, so calling it
/// twice against a destination that already has stale content from a
/// previous pass would let that stale content survive uncopied-over.
async fn copy_import_files(
    source_hyphae: &Path,
    dest_hyphae: &Path,
) -> Result<Vec<String>, CommError> {
    let source_hyphae = source_hyphae.to_path_buf();
    let dest_hyphae = dest_hyphae.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<Vec<String>, CommError> {
        std::fs::create_dir_all(&dest_hyphae)
            .map_err(|e| CommError::Upstream(format!("creating {dest_hyphae:?}: {e}")))?;
        std::fs::set_permissions(&dest_hyphae, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| CommError::Upstream(format!("chmod {dest_hyphae:?}: {e}")))?;

        let dir_fd = open_source_dir_blocking(&source_hyphae)?;
        let mut copied = Vec::new();
        for name in IMPORT_FILES {
            let Some(mut src_file) = open_source_file(&dir_fd, name)? else {
                continue;
            };
            let dst_path = dest_hyphae.join(name);
            let mut dst_file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&dst_path)
                .map_err(|e| CommError::Upstream(format!("creating {dst_path:?}: {e}")))?;
            std::io::copy(&mut src_file, &mut dst_file)
                .map_err(|e| CommError::Upstream(format!("copying {name}: {e}")))?;
            copied.push((*name).to_owned());
        }
        Ok(copied)
    })
    .await
    .map_err(|e| CommError::Upstream(format!("copy task panicked: {e}")))?
}

/// Parses the source's own `outbox.json` directly (independent of whatever
/// Hyphae itself reports) so the two can be cross-checked (H2's own
/// acceptance criterion: "导入前后 outbox 条目数一致"). The real on-disk
/// shape (`internal/messaging/outbox.go`'s `types.Outbox`, confirmed
/// against the locked Hyphae source, commit `a4aa606`) is a top-level
/// object, `{"entries": [...]}` — Codex 挑战 Medium #4: the previous
/// version only accepted a bare top-level array, which no real
/// `outbox.json` has ever been, so this cross-check silently no-op'd
/// (returned `None`, "skip") against every real file. Any other shape —
/// including the legacy bare-array shape, which is not what Hyphae
/// actually writes — is now a hard error rather than a silent skip. A
/// missing file counts as zero entries (never queued, the normal case for
/// a never-used Hyphae HOME).
async fn count_source_outbox_entries(source_hyphae: &Path) -> Result<usize, CommError> {
    let path = source_hyphae.join("outbox.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(CommError::Upstream(format!("reading {path:?}: {e}"))),
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| CommError::Upstream(format!("{path:?} is not valid json: {e}")))?;
    value
        .get("entries")
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| {
            CommError::Upstream(format!(
                "{path:?} does not have the expected {{\"entries\": [...]}} shape"
            ))
        })
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
        // Real shape, confirmed against the locked Hyphae source
        // (`internal/messaging/outbox.go`'s `types.Outbox`): a top-level
        // object, `{"entries": [...]}`, never a bare array.
        let outbox_items: Vec<String> = (0..outbox_len)
            .map(|i| format!("{{\"queue_id\":\"q{i}\",\"id\":\"e{i}\",\"status\":\"pending\"}}"))
            .collect();
        tokio::fs::write(
            hyphae_dir.join("outbox.json"),
            format!("{{\"entries\":[{}]}}", outbox_items.join(",")),
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
            matches!(err, CommError::Conflict(ref m, _) if m.contains("source_in_use")),
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
        assert!(matches!(err, CommError::Conflict(..)), "{err:?}");
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

    // ---- Codex 挑战 High #1: `from` overlapping comm's own staging/home --

    #[tokio::test]
    async fn import_from_the_staging_directory_itself_is_rejected_without_wiping_it() {
        let tmp = tempfile::tempdir().unwrap();
        let comm_dir = tmp.path().join("comm");
        let home = comm_dir.join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        // A caller who (by mistake, or malice) points `from` at comm's own
        // well-known staging path — pre-populate it with data that MUST
        // survive the rejection untouched.
        let staging_home = comm_dir.join("hyphae-home.staging");
        write_source_fixture(&staging_home, 1).await;
        let before = fingerprint_hashes(&staging_home.join(".hyphae")).await;

        let runner = build_runner(tmp.path(), NEVER_RUN_SCRIPT).await;
        let store = MemoryPasswordStore::new();

        let err = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from: staging_home.clone(),
                confirm: true,
                password: Some(Password::new(b"irrelevant".to_vec()).unwrap()),
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CommError::Invalid(ref m) if m.contains("overlaps")),
            "{err:?}"
        );

        // The whole point of the fix: nothing got wiped before the
        // rejection fired — without it, `run_import`'s unconditional
        // stale-staging cleanup (and/or `import()`'s own
        // cleanup-on-error) would have deleted this directory's contents
        // even though the error has nothing to do with staleness.
        let after = fingerprint_hashes(&staging_home.join(".hyphae")).await;
        assert_eq!(
            before, after,
            "from == staging_home must not have its data wiped"
        );
    }

    #[tokio::test]
    async fn dry_run_also_rejects_from_overlapping_comm_dir_without_wiping_it() {
        // The bug this guards against fired for dry_run too (the task's
        // own wording: "dry_run 也会触发") — the cleanup path doesn't care
        // whether a commit was ever going to happen.
        let tmp = tempfile::tempdir().unwrap();
        let comm_dir = tmp.path().join("comm");
        let home = comm_dir.join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let staging_home = comm_dir.join("hyphae-home.staging");
        write_source_fixture(&staging_home, 0).await;
        let before = fingerprint_hashes(&staging_home.join(".hyphae")).await;

        let runner = build_runner(tmp.path(), NEVER_RUN_SCRIPT).await;
        let store = MemoryPasswordStore::new();

        let err = import(
            &runner,
            &store,
            &home,
            ImportRequest {
                from: staging_home.clone(),
                confirm: true,
                password: None,
                dry_run: true,
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CommError::Invalid(ref m) if m.contains("overlaps")),
            "{err:?}"
        );
        let after = fingerprint_hashes(&staging_home.join(".hyphae")).await;
        assert_eq!(before, after);
    }

    // ---- Codex 挑战 High #2: no stale check trusted past the point where --
    // ---- the source could have been swapped ------------------------------

    #[tokio::test]
    async fn a_source_directory_swapped_for_a_symlink_after_the_early_check_is_still_caught() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;
        let source_hyphae = from.join(".hyphae");

        // The early, fail-fast check `run_import` does right after
        // resolving `source_root` passes against the real directory.
        drop(open_source_dir(&source_hyphae).await.unwrap());

        // Swap it for a symlink elsewhere — exactly the kind of
        // substitution a one-time, never-re-checked validation would
        // miss.
        let elsewhere = tmp.path().join("elsewhere");
        tokio::fs::create_dir_all(&elsewhere).await.unwrap();
        tokio::fs::remove_dir_all(&source_hyphae).await.unwrap();
        std::os::unix::fs::symlink(&elsewhere, &source_hyphae).unwrap();

        // `fingerprint_all` (used for both the pre- and post-copy
        // snapshot in `run_import`) independently reopens `source_hyphae`
        // from scratch and must reject it fresh, rather than trusting the
        // earlier, now-stale successful check.
        let err = fingerprint_all(&source_hyphae).await.unwrap_err();
        // `O_NOFOLLOW | O_DIRECTORY` on a symlink rejects it atomically
        // either way — the exact errno (`ELOOP` vs. `ENOTDIR`) is
        // platform-dependent (observed `ENOTDIR` on this macOS host),
        // but both map to a hard `Invalid` rejection, never a silent
        // follow-through.
        assert!(
            matches!(err, CommError::Invalid(ref m) if m.contains("symlink") || m.contains("not a directory")),
            "{err:?}"
        );
    }

    // ---- Codex 挑战 Medium #3: a retry must rebuild staging from scratch -

    #[tokio::test]
    async fn retry_without_clearing_staging_leaves_a_removed_source_file_behind_but_clearing_fixes_it()
     {
        // Directly exercises the mechanism `run_import`'s retry branch
        // relies on: `copy_import_files` only ever ADDS files, it never
        // removes one the source no longer has. Calling it twice against
        // a destination that already holds the FIRST pass's output is
        // not a safe "retry" once the source changed in between — the
        // fix is for the caller to wipe the staging directory before
        // calling this again.
        let tmp = tempfile::tempdir().unwrap();
        let source_hyphae = tmp.path().join("source").join(".hyphae");
        let staging_dir = tmp.path().join("staging");
        let dest_hyphae = staging_dir.join(".hyphae");
        tokio::fs::create_dir_all(&source_hyphae).await.unwrap();
        tokio::fs::write(source_hyphae.join("keystore.json"), b"keystore-bytes")
            .await
            .unwrap();
        tokio::fs::write(source_hyphae.join("messages.db-wal"), b"wal-bytes")
            .await
            .unwrap();

        // Pass 1: the wal is present and gets copied.
        let copied1 = copy_import_files(&source_hyphae, &dest_hyphae)
            .await
            .unwrap();
        assert!(copied1.iter().any(|n| n == "messages.db-wal"));
        assert!(
            tokio::fs::try_exists(dest_hyphae.join("messages.db-wal"))
                .await
                .unwrap()
        );

        // The source's wal disappears between pass 1 and the retry (e.g.
        // a WAL checkpoint) — exactly what the before/after fingerprint
        // comparison in `run_import` is designed to notice and retry on.
        tokio::fs::remove_file(source_hyphae.join("messages.db-wal"))
            .await
            .unwrap();

        // Bug reproduction: WITHOUT wiping staging first, the stale wal
        // from pass 1 survives even though `copied2` correctly omits it
        // from the report.
        let copied2_without_the_fix = copy_import_files(&source_hyphae, &dest_hyphae)
            .await
            .unwrap();
        assert!(
            !copied2_without_the_fix
                .iter()
                .any(|n| n == "messages.db-wal")
        );
        assert!(
            tokio::fs::try_exists(dest_hyphae.join("messages.db-wal"))
                .await
                .unwrap(),
            "bug reproduction: the stale wal should still be sitting in staging here"
        );

        // The fix `run_import` actually applies: wipe staging, then
        // recopy. The stale file is gone.
        tokio::fs::remove_dir_all(&staging_dir).await.unwrap();
        let copied3_with_the_fix = copy_import_files(&source_hyphae, &dest_hyphae)
            .await
            .unwrap();
        assert!(!copied3_with_the_fix.iter().any(|n| n == "messages.db-wal"));
        assert!(
            !tokio::fs::try_exists(dest_hyphae.join("messages.db-wal"))
                .await
                .unwrap(),
            "after clearing staging and recopying, no stale wal must remain"
        );
    }

    // ---- PR #635 R4 阻塞项 1: the SAME retry mechanism, exercised through --
    // ---- `import()`'s own control flow (not hand-composed in the test) ----
    //
    // The unit test above proves `copy_import_files` + a staging wipe is
    // enough; these two prove `run_import`'s own `if after != before { .. }`
    // branch actually calls that wipe before retrying, and actually takes
    // the `source_changed` branch when a second mismatch happens too — the
    // exact two gaps R4 flagged as untested. Both drive `import()`'s real
    // fingerprint/copy sequence (not a replay of it), but use the test-only
    // post-copy barrier above so each source mutation is guaranteed to land
    // before the corresponding post-copy fingerprint. Production builds do
    // not compile this barrier.

    #[tokio::test]
    async fn import_retries_once_when_the_source_wal_and_shm_vanish_mid_copy_and_excludes_them_from_the_result()
     {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 1).await;
        let source_hyphae = from.join(".hyphae");

        let script = fake_hyphae_script(true, "correct-pass", 1);
        let runner = build_runner(tmp.path(), &script).await;
        let store = MemoryPasswordStore::new();
        let comm_dir = tmp.path().join("comm");
        let home = comm_dir.join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let source_hyphae_canon = tokio::fs::canonicalize(&source_hyphae).await.unwrap();
        let (copy_done, resume_copy) = copy_test_hook::install(&source_hyphae_canon, 1);
        let mutate = async {
            copy_done.notified().await;
            tokio::fs::remove_file(source_hyphae.join("messages.db-wal"))
                .await
                .unwrap();
            tokio::fs::remove_file(source_hyphae.join("messages.db-shm"))
                .await
                .unwrap();
            resume_copy.notify_one();
        };
        let request = ImportRequest {
            from: from.clone(),
            confirm: true,
            password: Some(Password::new(b"correct-pass".to_vec()).unwrap()),
            dry_run: false,
        };
        let (result, ()) = tokio::join!(import(&runner, &store, &home, request), mutate);
        let report = result.unwrap();

        assert!(
            !report
                .db_files
                .iter()
                .any(|n| n == "messages.db-wal" || n == "messages.db-shm"),
            "a wal/shm that vanished mid-copy must not be reported as imported: {report:?}"
        );
        let home_hyphae = home.join(".hyphae");
        assert!(
            !tokio::fs::try_exists(home_hyphae.join("messages.db-wal"))
                .await
                .unwrap_or(false),
            "nor actually committed into the final home"
        );
        assert!(
            !tokio::fs::try_exists(home_hyphae.join("messages.db-shm"))
                .await
                .unwrap_or(false)
        );
        assert!(
            tokio::fs::try_exists(home_hyphae.join("messages.db"))
                .await
                .unwrap_or(false),
            "messages.db itself (which never vanished) must still have been committed"
        );
    }

    #[tokio::test]
    async fn import_reports_source_changed_when_the_source_keeps_changing_across_the_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;
        let source_hyphae = from.join(".hyphae");

        let script = fake_hyphae_script(true, "correct-pass", 0);
        let runner = build_runner(tmp.path(), &script).await;
        let store = MemoryPasswordStore::new();
        let comm_dir = tmp.path().join("comm");
        let home = comm_dir.join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();

        let source_hyphae_canon = tokio::fs::canonicalize(&source_hyphae).await.unwrap();
        let (first_copy_done, resume_first_copy) = copy_test_hook::install(&source_hyphae_canon, 1);
        let mutate = async {
            first_copy_done.notified().await;
            tokio::fs::remove_file(source_hyphae.join("messages.db-wal"))
                .await
                .unwrap();
            tokio::fs::remove_file(source_hyphae.join("messages.db-shm"))
                .await
                .unwrap();

            let (retry_copy_done, resume_retry_copy) =
                copy_test_hook::install(&source_hyphae_canon, 2);
            resume_first_copy.notify_one();
            retry_copy_done.notified().await;

            let path = source_hyphae.join("messages.db");
            let mut content = tokio::fs::read(&path).await.unwrap();
            content.extend_from_slice(b"-changed-again");
            tokio::fs::write(&path, &content).await.unwrap();
            resume_retry_copy.notify_one();
        };
        let request = ImportRequest {
            from: from.clone(),
            confirm: true,
            password: Some(Password::new(b"correct-pass".to_vec()).unwrap()),
            dry_run: false,
        };
        let (result, ()) = tokio::join!(import(&runner, &store, &home, request), mutate);
        let err = result.unwrap_err();

        assert!(
            matches!(err, CommError::Conflict(ref m, _) if m.contains("source_changed")),
            "{err:?}"
        );
        assert!(
            !tokio::fs::try_exists(home.join(".hyphae"))
                .await
                .unwrap_or(false),
            "nothing must be committed when the retry itself also sees a mismatch"
        );
    }

    // ---- Codex 挑战 Medium #4: outbox.json's real `{"entries": [...]}` ---
    // ---- shape, parsed strictly (not silently skipped) -------------------

    #[tokio::test]
    async fn count_source_outbox_entries_rejects_the_legacy_bare_array_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let hyphae_dir = tmp.path().join(".hyphae");
        tokio::fs::create_dir_all(&hyphae_dir).await.unwrap();
        // The shape this module used to (wrongly) accept — no real Hyphae
        // `outbox.json` has ever looked like this; confirmed against the
        // locked Hyphae source (`internal/messaging/outbox.go`'s
        // `types.Outbox{ Entries []OutboxEntry `json:"entries"` }`,
        // commit a4aa606).
        tokio::fs::write(hyphae_dir.join("outbox.json"), br#"[{"event_id":"e0"}]"#)
            .await
            .unwrap();
        let err = count_source_outbox_entries(&hyphae_dir).await.unwrap_err();
        assert!(
            matches!(err, CommError::Upstream(ref m) if m.contains("entries")),
            "a bare array must be a hard error now, not a silently-skipped None: {err:?}"
        );
    }

    #[tokio::test]
    async fn count_source_outbox_entries_parses_the_real_entries_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let hyphae_dir = tmp.path().join(".hyphae");
        tokio::fs::create_dir_all(&hyphae_dir).await.unwrap();
        tokio::fs::write(
            hyphae_dir.join("outbox.json"),
            br#"{"entries":[{"id":"a"},{"id":"b"}]}"#,
        )
        .await
        .unwrap();
        assert_eq!(count_source_outbox_entries(&hyphae_dir).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn import_fails_when_hyphae_reports_fewer_outbox_entries_than_the_source_file_has() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        // Source outbox.json really has 3 entries (real
        // `{"entries":[...]}` shape, via `write_source_fixture`).
        write_source_fixture(&from, 3).await;

        // The fake hyphae CLI only reports 2 — the real mismatch this
        // cross-check exists to catch (H2's acceptance criterion: "导入
        // 前后 outbox 条目数一致"). Before the format fix, this check
        // silently no-op'd against every real `outbox.json` (bare-array
        // parsing never matched, so the comparison was skipped
        // entirely) — this test would NOT have caught a real mismatch
        // under the old code.
        let script = fake_hyphae_script(true, "correct-pass", 2);
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
            matches!(err, CommError::Upstream(ref m) if m.contains("outbox entry count mismatch")),
            "{err:?}"
        );
    }

    // ---- Codex 挑战 Medium #5: a FIFO must be rejected, never block -------

    #[tokio::test]
    async fn fingerprint_rejects_a_fifo_in_place_of_a_tracked_source_file_without_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;
        let source_hyphae = from.join(".hyphae");

        let relays_path = source_hyphae.join("relays.json");
        tokio::fs::remove_file(&relays_path).await.unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(&relays_path)
            .status()
            .expect("mkfifo must be available on darwin/linux CI");
        assert!(status.success(), "mkfifo failed");

        // Bounded by a timeout: before the fix, opening a FIFO for
        // reading without `O_NONBLOCK` is fine (a read-only open of a
        // FIFO never blocks on its own), but the OLD code's separate
        // `check_not_symlink_and_owned`+later-`tokio::fs::copy`/`metadata`
        // pattern never checked the file TYPE at all, so a FIFO sailed
        // through fingerprinting; this test pins the new, strict
        // behavior with a hard timeout as a safety net regardless.
        let result = tokio::time::timeout(Duration::from_secs(5), fingerprint_all(&source_hyphae))
            .await
            .expect("fingerprint_all must not block on a FIFO");
        let err = result.unwrap_err();
        assert!(
            matches!(err, CommError::Invalid(ref m) if m.contains("not a regular file")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn try_lock_outbox_rejects_a_fifo_instead_of_blocking_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("outbox.json.lock");
        let status = std::process::Command::new("mkfifo")
            .arg(&lock_path)
            .status()
            .expect("mkfifo must be available on darwin/linux CI");
        assert!(status.success(), "mkfifo failed");

        // Opening a FIFO `O_WRONLY` with no reader on the other end would
        // block forever without `O_NONBLOCK` — and since this call holds
        // `KeystoreWriteLock` for its whole duration in `run_import`, a
        // hang here would starve every other comm operation, not just
        // this one import. Bounded by a timeout as a safety net
        // regardless.
        let result = tokio::time::timeout(Duration::from_secs(5), try_lock_outbox(lock_path))
            .await
            .expect("try_lock_outbox must not block on a FIFO");
        let err = result.unwrap_err();
        assert!(
            matches!(err, CommError::Invalid(ref m) if m.contains("FIFO") || m.contains("not a regular file")),
            "{err:?}"
        );
    }

    // ---- PR #635 R4 阻塞项 2: `daemon.lock` probe --------------------------

    #[tokio::test]
    async fn try_lock_daemon_skips_silently_when_the_source_has_no_daemon_lock_file() {
        // The compatibility case this probe exists to preserve: every
        // Hyphae HOME predating #104 (including the source this crate is
        // currently pinned to, `a4aa606`) has no `daemon.lock` at all —
        // `try_lock_daemon` must treat that as "nothing to probe", not an
        // error, and critically must NOT create the file itself (checked
        // below).
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;
        let source_hyphae = from.join(".hyphae");

        let locked = try_lock_daemon(&source_hyphae).await.unwrap();
        assert!(locked.is_none());
        assert!(
            !tokio::fs::try_exists(source_hyphae.join("daemon.lock"))
                .await
                .unwrap_or(false),
            "the probe must never manufacture daemon.lock inside the source HOME"
        );
    }

    #[tokio::test]
    async fn import_reports_source_in_use_when_the_source_daemon_lock_is_already_held() {
        // Simulates a currently-running Hyphae daemon (built against a
        // Hyphae source that DOES ship `daemon.lock`, i.e. #104+): hold a
        // non-blocking exclusive flock on the source's `daemon.lock` from a
        // separate open file description, exactly like the existing
        // `outbox.json.lock` conflict test above, and confirm `import`
        // reports `source_in_use` instead of silently proceeding.
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;
        let lock_path = from.join(".hyphae").join("daemon.lock");

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
            matches!(err, CommError::Conflict(ref m, _) if m.contains("source_in_use")),
            "{err:?}"
        );
        drop(held);
    }

    #[tokio::test]
    async fn try_lock_daemon_rejects_a_fifo_at_the_lock_path_without_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("old-home");
        write_source_fixture(&from, 0).await;
        let source_hyphae = from.join(".hyphae");
        let lock_path = source_hyphae.join("daemon.lock");
        let status = std::process::Command::new("mkfifo")
            .arg(&lock_path)
            .status()
            .expect("mkfifo must be available on darwin/linux CI");
        assert!(status.success(), "mkfifo failed");

        let result = tokio::time::timeout(Duration::from_secs(5), try_lock_daemon(&source_hyphae))
            .await
            .expect("try_lock_daemon must not block on a FIFO");
        let err = result.unwrap_err();
        assert!(
            matches!(err, CommError::Invalid(ref m) if m.contains("FIFO") || m.contains("not a regular file")),
            "{err:?}"
        );
    }
}
