//! COMM-2a: REST routes for identity / contact / relay (COMM-HYPHAE.md §4).
//! `agent24d` nests [`router`] at `/api/v1/comm`; `agent24 comm ...` and the
//! desktop UI are both REST clients of exactly these routes — nothing here
//! is CLI- or UI-specific.
//!
//! [`CommState`] is NOT just a [`HyphaeRunner`] handle: `A24_HYPHAE_BIN` (or
//! its fallbacks, resolved by `agent24d`, not this crate) may simply name no
//! binary at all, and a daemon with comm unconfigured must still start and
//! answer every OTHER route (COMM-2a's own brief — "不能让 daemon 起不来").
//! [`CommState::unconfigured`] and [`CommState::binary_rejected`] build a
//! state whose every route reports the matching error without ever touching
//! a binary — two DIFFERENT facts, both in §4's closed set: "nothing was
//! ever configured" (`not_configured`, 409) is not "something was named and
//! failed verification" (`binary_rejected`, 503), and a caller debugging a
//! typo'd path needs to be able to tell them apart.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::daemon::RelayProbeStatus;
use crate::error::{CommError, map_envelope_failure, map_runner_error, map_store_error};
use crate::npub::is_valid_npub;
use crate::password::Password;
use crate::password_store::{Account, PasswordStore};
use crate::runner::{
    Envelope, ExitClass, HyphaeRunner, Invocation, RunnerError, hold_until_reaped,
};

type CommResult = Result<Json<Value>, CommError>;

/// How to answer a comm request. Only [`Backend::Ready`] ever spawns
/// Hyphae; the other two are terminal, documented facts about why it won't.
#[derive(Clone)]
enum Backend {
    Ready {
        runner: Arc<HyphaeRunner>,
        password_store: Arc<dyn PasswordStore>,
        home: Arc<PathBuf>,
    },
    Unconfigured {
        reason: Arc<String>,
    },
    BinaryRejected {
        reason: Arc<String>,
    },
}

/// Shared state for every comm route. Cheap to clone (everything inside is
/// an `Arc`), matching every other kernel/module state type in this
/// workspace (`agent24d::server::AppState`, `agent24_os_sdk`'s module
/// states).
#[derive(Clone)]
pub struct CommState {
    backend: Backend,
    /// COMM-4a: the Hyphae daemon supervisor, when one has been wired up by
    /// `agent24d` (`comm_routes.rs`). `None` on every test/`Unconfigured`/
    /// `BinaryRejected` state that never calls [`CommState::with_daemon`] —
    /// the `/comm/daemon*` routes then answer `not_configured` same as any
    /// other route on those backends.
    daemon: Option<Arc<crate::daemon::HyphaeDaemonSupervisor>>,
    /// One private-key export at a time (a second gets `export_in_progress`).
    export_gate: Arc<tokio::sync::Mutex<()>>,
}

impl CommState {
    /// A fully wired state: calls actually reach Hyphae. `home` is the
    /// Hyphae HOME the runner was built with (COMM-HYPHAE.md §2:
    /// `<state_dir>/comm/hyphae-home/`) — kept here too (not only inside
    /// `runner`, which does not expose it) because `identity create`'s
    /// first-identity flow (below) needs to peek `keystore.json`'s `salt`
    /// field directly.
    #[must_use]
    pub fn ready(
        runner: Arc<HyphaeRunner>,
        password_store: Arc<dyn PasswordStore>,
        home: PathBuf,
    ) -> Self {
        Self {
            backend: Backend::Ready {
                runner,
                password_store,
                home: Arc::new(home),
            },
            daemon: None,
            export_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Wires in the COMM-4a daemon supervisor. `agent24d` calls this right
    /// after `ready` whenever it was able to build one; tests that only
    /// exercise identity/contact/relay routes have no reason to.
    #[must_use]
    pub fn with_daemon(mut self, daemon: Arc<crate::daemon::HyphaeDaemonSupervisor>) -> Self {
        self.daemon = Some(daemon);
        self
    }

    /// Every route answers `not_configured`, message `reason` — no binary
    /// was ever named (`A24_HYPHAE_BIN` unset, no sibling `hyphae` found).
    #[must_use]
    pub fn unconfigured(reason: impl Into<String>) -> Self {
        Self {
            backend: Backend::Unconfigured {
                reason: Arc::new(reason.into()),
            },
            daemon: None,
            export_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Every route answers `binary_rejected`, message `reason` — a binary
    /// WAS named but failed hash verification (see `crate::binary`).
    #[must_use]
    pub fn binary_rejected(reason: impl Into<String>) -> Self {
        Self {
            backend: Backend::BinaryRejected {
                reason: Arc::new(reason.into()),
            },
            daemon: None,
            export_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) fn require_ready(
        &self,
    ) -> Result<(&HyphaeRunner, &dyn PasswordStore, &Path), CommError> {
        match &self.backend {
            Backend::Ready {
                runner,
                password_store,
                home,
            } => Ok((runner.as_ref(), password_store.as_ref(), home.as_path())),
            Backend::Unconfigured { reason } => Err(CommError::NotConfigured((**reason).clone())),
            Backend::BinaryRejected { reason } => {
                Err(CommError::BinaryRejected((**reason).clone()))
            }
        }
    }

    /// COMM-4a: the daemon supervisor, or `not_configured` if none was ever
    /// wired (an `Unconfigured`/`BinaryRejected` backend, per
    /// `require_ready`, or a `Ready` test state built without
    /// `with_daemon`).
    fn require_daemon(&self) -> Result<&crate::daemon::HyphaeDaemonSupervisor, CommError> {
        self.daemon.as_deref().ok_or_else(|| {
            CommError::NotConfigured("the hyphae daemon supervisor is not wired up".to_owned())
        })
    }
}

/// The comm router, relative to wherever the caller nests it (`agent24d`
/// nests it at `/api/v1/comm`). Returns `Router<()>` — the state is already
/// baked in via `with_state`, so this merges into any other router
/// regardless of ITS state type, same shape as
/// `agent24_os_sdk`'s module routers.
pub fn router(state: CommState) -> Router {
    Router::new()
        .route("/identity", get(list_identity).post(create_identity))
        .route("/identity/default", post(use_identity))
        .route("/identity/export", post(export_identity))
        .route("/identity/import", post(import_identity))
        .route("/unlock", post(unlock))
        .route("/contact", get(list_contact).post(add_contact))
        .route("/relay", get(list_relay).put(set_relay))
        .route("/relay/probe", post(probe_relay))
        .route("/import", post(import))
        .route("/daemon", get(daemon_status))
        .route("/daemon/start", post(daemon_start))
        .route("/daemon/stop", post(daemon_stop))
        .route("/send", post(send_message))
        .route("/history", get(get_history))
        .route("/inbox/pull", post(pull_inbox))
        .route("/outbox", get(list_outbox))
        .route("/outbox/{event_id}/retry", post(retry_outbox))
        .route("/outbox/clear", post(clear_outbox))
        .merge(crate::groupchat::routes())
        .with_state(state)
}

// ---------------------------------------------------------------------
// daemon (COMM-4a; COMM-HYPHAE.md §4, §5.2 trimmed to what this task tracks)
// ---------------------------------------------------------------------

/// COMM-4b: the full three-state object (COMM-HYPHAE.md §5.2) — `process`
/// (COMM-4a), plus `relay_probe` and `catch_up`, which this task adds.
/// `catch_up` is read fresh from the log file on every call: no route here
/// ever caches it across requests, matching `relay_probe`'s own "only on
/// demand" posture (M12 cut periodic probing/tailing).
async fn daemon_status_json(daemon: &crate::daemon::HyphaeDaemonSupervisor) -> Value {
    let s = daemon.status();
    let relay_probe = daemon.relay_probe_status();
    let catch_up = daemon.catch_up().await;
    json!({"ok": true, "data": {
        "process": {
            "state": s.state,
            "generation": s.generation,
            "consecutive_failures": s.consecutive_failures,
            "reason": s.reason,
        },
        "relay_probe": relay_probe.map(|p| json!({
            "url": p.url,
            "connected": p.connected,
            "at_ms": p.at_ms,
            "error": p.error,
        })),
        "catch_up": {
            "state": catch_up.state,
            "last_incomplete_at_ms": catch_up.last_incomplete_at_ms,
        },
    }})
}

async fn daemon_status(State(state): State<CommState>) -> CommResult {
    state.require_ready()?;
    let daemon = state.require_daemon()?;
    Ok(Json(daemon_status_json(daemon).await))
}

async fn daemon_start(State(state): State<CommState>) -> CommResult {
    state.require_ready()?;
    let daemon = state.require_daemon()?;
    daemon.start().await.map_err(|e| match e {
        crate::daemon::DaemonStartError::NotConfigured(m) => CommError::NotConfigured(m),
        crate::daemon::DaemonStartError::Locked(m) => CommError::Locked(m),
        crate::daemon::DaemonStartError::Failed(m) => CommError::Upstream(m),
    })?;
    Ok(Json(daemon_status_json(daemon).await))
}

async fn daemon_stop(State(state): State<CommState>) -> CommResult {
    state.require_ready()?;
    let daemon = state.require_daemon()?;
    daemon.stop().await;
    Ok(Json(daemon_status_json(daemon).await))
}

fn args(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(|p| OsString::from(*p)).collect()
}

fn read_invocation(parts: &[&str]) -> Invocation {
    Invocation {
        args: args(parts),
        password: None,
        timeout: None,
    }
}

/// `{"ok":true,"data":...}` / the closed-set error for a plain read.
fn envelope_response(envelope: Envelope) -> CommResult {
    match envelope {
        Envelope::Ok { data } => Ok(Json(json!({"ok": true, "data": data}))),
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => Err(map_envelope_failure(&error, &message, data)),
    }
}

async fn run_read(runner: &HyphaeRunner, argv: &[&str]) -> CommResult {
    let envelope = runner
        .run(read_invocation(argv))
        .await
        .map_err(map_runner_error)?;
    envelope_response(envelope)
}

fn invalid_if_blank(field: &'static str, value: &str) -> Result<(), CommError> {
    if value.trim().is_empty() {
        Err(CommError::Invalid(format!("{field} must not be empty")))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------
// identity
// ---------------------------------------------------------------------

async fn list_identity(State(state): State<CommState>) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    run_read(runner, &["identity", "list"]).await
}

#[derive(Deserialize)]
struct IdentityCreateReq {
    nickname: String,
    #[serde(default)]
    default: bool,
}

/// `keystore.json`'s `salt` field, read directly — the one field this crate
/// reads from Hyphae's own files rather than through the CLI. There is no
/// `identity list` field for it and no other documented way to recover
/// which keyring account a keystore's password is filed under
/// (`password_store::Account::from_salt`'s own doc already assumes this
/// read happens somewhere). Never a write, and never any field but this
/// one. `None` means no keystore exists yet at `home` (first identity).
async fn read_keystore_salt(home: &Path) -> Result<Option<String>, CommError> {
    let path = home.join(".hyphae").join("keystore.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(CommError::Upstream(format!(
                "reading {}: {e}",
                path.display()
            )));
        }
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| CommError::Upstream(format!("{} is not valid json: {e}", path.display())))?;
    Ok(value.get("salt").and_then(Value::as_str).map(str::to_owned))
}

/// `pub(crate)`, not private: COMM-4a's daemon supervisor (`daemon.rs`)
/// needs the same "which keyring account does this HOME's keystore use"
/// lookup to retrieve the daemon's own password before spawning.
pub(crate) async fn resolve_account(home: &Path) -> Result<Option<Account>, CommError> {
    Ok(read_keystore_salt(home)
        .await?
        .map(|salt| Account::from_salt(&salt)))
}

/// `POST /comm/identity`. Two flows (COMM-HYPHAE.md §6.4):
/// - **First identity** (no keystore yet at `home`): generate a password,
///   file it under a `Pending` account, run `identity create` holding
///   `KeystoreWriteLock` for the whole spawn-to-exit span, then — still
///   under that same lock, so no second "first identity" racer can land in
///   between — read the new keystore's salt and rename `Pending` to `Salt`.
///   A failure never blindly deletes the `Pending` entry: Hyphae's own
///   keystore write is an atomic rename partway through `identity create`'s
///   execution (COMM-HYPHAE.md §6.4), so a failure reported *after* that
///   rename has landed — a timeout, a signal, an oversized output stream,
///   or Hyphae itself failing a later step such as `--default` — does not
///   mean nothing happened. [`resolve_first_identity_failure`] re-checks
///   `keystore.json` (still under the lock) before ever deleting anything;
///   see its own doc comment (PR #622 High).
/// - **Every later identity**: the keystore (and its account) already
///   exists; fetch its password and reuse it.
async fn create_identity(
    State(state): State<CommState>,
    Json(req): Json<IdentityCreateReq>,
) -> CommResult {
    let (runner, password_store, home) = state.require_ready()?;
    invalid_if_bad_name("nickname", &req.nickname)?;
    check_nickname_len(&req.nickname)?;

    // Held for the ENTIRE decision+create+promote sequence below, not just
    // around the spawn — two concurrent requests both reading "no keystore
    // yet" before either held this lock would otherwise both decide they are
    // the first identity and each mint their OWN fresh password; whichever
    // loses the race to actually run `identity create` would then try to
    // unlock an already-created keystore with the WRONG password (observed
    // while proving this test: a second concurrent create failed
    // `locked{incorrect password}` before this lock was moved out here).
    // `KeystoreWriteLock` is documented (H3/G8) to close exactly this class
    // of race; it has to be held before the read that decides which
    // password to use, not just before the write.
    let guard = runner.keystore_lock().acquire().await;

    let existing = resolve_account(home).await?;
    let (account, password, is_first_identity) = match existing {
        Some(account) => {
            let password = password_store
                .get(&account)
                .await
                .map_err(map_store_error)?;
            (account, password, false)
        }
        None => {
            let pending = Account::new_pending();
            let mut random32 = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rng(), &mut random32);
            let password = Password::generate(random32);
            password_store
                .put(&pending, &password)
                .await
                .map_err(map_store_error)?;
            (pending, password, true)
        }
    };

    let mut argv: Vec<OsString> = vec![
        "identity".into(),
        "create".into(),
        "--nickname".into(),
        req.nickname.into(),
    ];
    if req.default {
        argv.push("--default".into());
    }

    let result = runner
        .run(Invocation {
            args: argv,
            password: Some(password),
            timeout: None,
        })
        .await;

    match result {
        Ok(Envelope::Ok { data }) => {
            // The promotion itself is real keychain I/O under production's
            // `KeyringPasswordStore` (every op there runs through
            // `spawn_blocking`, `password_store.rs`), so the guard is kept
            // until it finishes (PR #622 Medium) — otherwise a second
            // request parked on this lock can wake up, see the keystore
            // already written, and look its password up on the `Salt`
            // account before `rename`'s own write has landed there.
            if is_first_identity {
                promote_first_identity_password(home, password_store, &account).await;
            }
            drop(guard);
            // COMM-4a (COMM-HYPHAE.md §6.3): creating a new DEFAULT identity
            // changes what `daemon --identity` would use, same as
            // `/identity/default` below — restart it if it's running. Done
            // after the keystore write lock is released (PR #622 Medium
            // only needs the lock held through the Pending->Salt rename
            // above; daemon supervision is unrelated to that critical
            // section).
            if req.default
                && let Some(daemon) = state.daemon.clone()
            {
                daemon.on_config_changed().await;
            }
            Ok(Json(json!({"ok": true, "data": data})))
        }
        Ok(Envelope::Failed {
            error,
            message,
            data,
            ..
        }) => {
            let outcome = if is_first_identity {
                Some(resolve_first_identity_failure(home, password_store, &account).await)
            } else {
                None
            };
            drop(guard);
            Err(first_identity_failure_error(
                outcome,
                map_envelope_failure(&error, &message, data),
            ))
        }
        Err(e) => {
            // A child that could not be confirmed dead may still be writing the
            // keystore: keep the lock until it is reaped, decide nothing yet.
            if let RunnerError::StillTerminating(reaped) = &e {
                hold_until_reaped(guard, reaped.clone());
                return Err(map_runner_error(e));
            }
            let outcome = if is_first_identity {
                Some(resolve_first_identity_failure(home, password_store, &account).await)
            } else {
                None
            };
            drop(guard);
            Err(first_identity_failure_error(outcome, map_runner_error(e)))
        }
    }
}

/// First-identity success path: promote the `Pending` password onto its new
/// `Salt` account. Called while [`create_identity`] still holds
/// `KeystoreWriteLock` (PR #622 Medium — see the call site's comment).
async fn promote_first_identity_password(
    home: &Path,
    password_store: &dyn PasswordStore,
    account: &Account,
) {
    match read_keystore_salt(home).await {
        Ok(Some(salt)) => {
            let salt_account = Account::from_salt(&salt);
            if let Err(e) = password_store.rename(account, &salt_account).await {
                tracing::warn!(
                    "identity create succeeded but promoting the pending comm keystore \
                     password failed ({e}); it stays under the pending account until the \
                     next successful create or restart"
                );
            }
        }
        Ok(None) => {
            tracing::warn!(
                "identity create succeeded but keystore.json has no salt field; leaving the \
                 pending password entry in place"
            );
        }
        Err(e) => {
            tracing::warn!("could not read the new keystore's salt: {e}");
        }
    }
}

/// What happened to the first identity's `Pending` password when `identity
/// create` finished with a failure — independent of *why* it failed (a bad
/// envelope, a timeout, a signal, an oversized output stream: see
/// [`create_identity`]'s doc comment, PR #622 High). Hyphae's own keystore
/// write is an atomic rename partway through its execution, so a failure
/// reported after that rename has landed does not mean nothing happened:
/// deleting the password unconditionally in that case permanently locks the
/// keystore, because the `None => mint a new password` branch above is only
/// reachable while no keystore exists yet, which is no longer true once one
/// has been written. This resolves the only reliable ground truth available
/// without spawning Hyphae again: `keystore.json` itself, via the same
/// `read_keystore_salt` the success path already uses.
enum FirstIdentityFailureOutcome {
    /// `keystore.json` confirmed absent: the create genuinely never wrote
    /// anything. Deleting `Pending` is safe — nothing is being lost.
    NotWritten,
    /// `keystore.json` has a salt: the keystore WAS written despite the
    /// reported failure. The password was promoted onto its `Salt` account
    /// and must never be deleted.
    Promoted,
    /// Either the keystore was confirmed written but promoting the password
    /// itself failed (a keyring hiccup), or the keystore's state could not
    /// be determined at all (e.g. an unreadable/corrupt `keystore.json`).
    /// Both are treated the same way — never delete — because doing so
    /// risks deleting the password for a keystore that does in fact exist.
    Preserved,
}

/// Resolves and acts on [`FirstIdentityFailureOutcome`] for the first
/// identity's `Pending` account. Run while [`create_identity`] still holds
/// `KeystoreWriteLock`, so no concurrent request can observe (or race) a
/// half-decided password.
async fn resolve_first_identity_failure(
    home: &Path,
    password_store: &dyn PasswordStore,
    account: &Account,
) -> FirstIdentityFailureOutcome {
    match read_keystore_salt(home).await {
        Ok(None) => {
            // Confirmed: no keystore was ever written. Safe to delete.
            let _ = password_store.delete(account).await;
            FirstIdentityFailureOutcome::NotWritten
        }
        Ok(Some(salt)) => {
            // The keystore WAS written despite the reported failure.
            // Promote exactly like the success path would; never delete.
            let salt_account = Account::from_salt(&salt);
            match password_store.rename(account, &salt_account).await {
                Ok(()) => {
                    tracing::warn!(
                        "identity create reported a failure, but keystore.json was already \
                         written; the pending comm keystore password has been promoted onto \
                         its salt account rather than deleted"
                    );
                    FirstIdentityFailureOutcome::Promoted
                }
                Err(e) => {
                    tracing::warn!(
                        "identity create reported a failure and keystore.json was already \
                         written, but promoting the pending comm keystore password failed \
                         ({e}); keeping it under the pending account rather than risk \
                         deleting the only copy of a working keystore's password"
                    );
                    FirstIdentityFailureOutcome::Preserved
                }
            }
        }
        Err(e) => {
            // Could not determine whether the keystore was written (e.g.
            // keystore.json exists but is not valid json). Treat this the
            // same as "written": never delete on an unproven guess.
            tracing::warn!(
                "identity create reported a failure and keystore.json's state could not be \
                 read ({e}); keeping the pending comm keystore password rather than risk \
                 deleting the only copy of a working keystore's password"
            );
            FirstIdentityFailureOutcome::Preserved
        }
    }
}

/// Builds the response for a failed `identity create` on the first-identity
/// path. When the `Pending` password was preserved rather than confirmed
/// deleted, the original failure is reported as `partial` (COMM-HYPHAE.md
/// §4's partial/upstream class) with an explicit `password_preserved` note,
/// so a caller does not read a generic error and assume the password is
/// gone (PR #622 High). When nothing needed preserving (not a first
/// identity, or the keystore was confirmed never written), the original
/// error is returned unchanged.
fn first_identity_failure_error(
    outcome: Option<FirstIdentityFailureOutcome>,
    original: CommError,
) -> CommError {
    match outcome {
        None | Some(FirstIdentityFailureOutcome::NotWritten) => original,
        Some(FirstIdentityFailureOutcome::Promoted)
        | Some(FirstIdentityFailureOutcome::Preserved) => CommError::Partial(json!({
            "password_preserved": true,
            "original_error": original.to_string(),
        })),
    }
}

#[derive(Deserialize)]
struct IdentityDefaultReq {
    nickname: String,
}

/// `POST /comm/identity/default` → `identity use --nickname N` (§4: 🔒, no
/// ⚿ — switching the default identity does not need the keystore password).
async fn use_identity(
    State(state): State<CommState>,
    Json(req): Json<IdentityDefaultReq>,
) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    invalid_if_bad_name("nickname", &req.nickname)?;
    let envelope = runner
        .run_keystore_write(read_invocation(&[
            "identity",
            "use",
            "--nickname",
            &req.nickname,
        ]))
        .await
        .map_err(map_runner_error)?;
    // COMM-4a: the default identity just changed — restart the daemon if
    // it's running (COMM-HYPHAE.md §6.3), without touching its failure count.
    if matches!(envelope, Envelope::Ok { .. })
        && let Some(daemon) = state.daemon.clone()
    {
        daemon.on_config_changed().await;
    }
    envelope_response(envelope)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityExportReq {
    nickname: String,
    path: String,
    /// Encrypts the backup (NIP-49, done by Hyphae). Held zeroizing, never
    /// logged or echoed.
    backup_password: UnlockSecret,
}

/// Unicode general category Cf (format controls), which includes the bidi
/// overrides and isolates (U+202A-202E, U+2066-2069) that can make a name read
/// differently from what it is. U+200C/U+200D (joiners) are allowed: scripts
/// such as Persian and Indic need them for ordinary words.
pub(crate) fn is_format_control(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2
        | 0x180E | 0x200B | 0x200E..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064
        | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD
        | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A
        | 0xE0001 | 0xE0020..=0xE007F)
}

/// The nickname contract for NEW names (create, contact add, restore) and for
/// export: at most 128 characters and 512 UTF-8 bytes, the same as the desktop.
/// Existing longer names still list and work for everything except export
/// (`default` deliberately does not check it); export and restore of such a
/// name answer `nickname_too_long`.
pub(crate) const NICKNAME_MAX_CHARS: usize = 128;
pub(crate) const NICKNAME_MAX_BYTES: usize = 512;

fn check_nickname_len(value: &str) -> Result<(), CommError> {
    if value.chars().count() > NICKNAME_MAX_CHARS || value.len() > NICKNAME_MAX_BYTES {
        return Err(CommError::Coded(
            axum::http::StatusCode::BAD_REQUEST,
            "nickname_too_long",
            format!(
                "a nickname may have at most {NICKNAME_MAX_CHARS} characters ({NICKNAME_MAX_BYTES} bytes)"
            ),
        ));
    }
    Ok(())
}

/// Identity and contact names end up in Hyphae's line-oriented output and in
/// argv; a control character (a newline above all) has no legitimate use.
fn invalid_if_bad_name(field: &'static str, value: &str) -> Result<(), CommError> {
    invalid_if_blank(field, value)?;
    if value
        .chars()
        .any(|c| c.is_control() || is_format_control(c))
    {
        return Err(CommError::Invalid(format!(
            "{field} must not contain control or invisible formatting characters"
        )));
    }
    Ok(())
}

/// `POST /comm/identity/export` makes Hyphae write an NIP-49 password-encrypted
/// (`ncryptsec1…`) backup of the identity's key, then publishes it at `path`
/// without ever overwriting anything (see `export.rs`). Answers
/// `{nickname, npub, path, format}` only: neither the key, the backup nor the
/// backup password is in a response or a log line. Needs Hyphae >= v0.28.0
/// (`hyphae_too_old` otherwise).
async fn export_identity(
    State(state): State<CommState>,
    payload: Result<Json<IdentityExportReq>, JsonRejection>,
) -> CommResult {
    let Json(req) = payload.map_err(|_| CommError::Invalid("invalid export request".to_owned()))?;
    let (runner, password_store, home) = state.require_ready()?;
    invalid_if_bad_name("nickname", &req.nickname)?;
    check_nickname_len(&req.nickname)?;
    invalid_if_blank("path", &req.path)?;
    crate::export::validate_backup_password(&req.backup_password.0)?;
    let dest = PathBuf::from(&req.path);
    // One export at a time: Hyphae's scrypt work, the keychain read and the
    // staged file are heavy and secret-bearing; a second caller is refused.
    let single = Arc::clone(&state.export_gate)
        .try_lock_owned()
        .map_err(|_| {
            CommError::Conflict(
                "export_in_progress: another key backup is already running".to_owned(),
                Some(json!({"reason": "export_in_progress"})),
            )
        })?;
    crate::export::require_feature(runner, crate::export::Feature::Export).await?;
    let staged = crate::export::Staged::begin(&dest, home)?;

    // The identity's npub comes from `identity list`: Hyphae must report
    // exactly this one for the export to be accepted.
    let npub = identity_npub(runner, &req.nickname)
        .await?
        .ok_or_else(|| CommError::NotFound("identity not found".to_owned()))?;
    let account = resolve_account(home).await?.ok_or_else(|| {
        CommError::NotConfigured("no keystore exists yet; create an identity first".to_owned())
    })?;
    let keystore_password = password_store
        .get(&account)
        .await
        .map_err(map_store_error)?;
    let outcome = crate::export::export_identity_backup(
        runner,
        keystore_password,
        &req.nickname,
        &npub,
        &req.backup_password.0,
        staged,
        single,
    )
    .await?;
    tracing::info!(nickname = %req.nickname, "identity key backup exported");
    // The backup is published and stays published; `warnings` tells the user
    // about the two things that could not be confirmed afterwards.
    let mut data = json!({
        "nickname": req.nickname,
        "npub": npub,
        "path": req.path,
        "format": "ncryptsec",
    });
    if let Some(warnings) = outcome.warnings_json() {
        data["warnings"] = warnings;
    }
    Ok(Json(json!({"ok": true, "data": data})))
}

/// `(npub, is_default)` of the identity called `nickname` according to
/// `identity list`.
async fn identity_entry(
    runner: &HyphaeRunner,
    nickname: &str,
) -> Result<Option<(String, bool)>, CommError> {
    let listed = match runner
        .run(read_invocation(&["identity", "list"]))
        .await
        .map_err(map_runner_error)?
    {
        Envelope::Ok { data } => data,
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => {
            return Err(map_envelope_failure(&error, &message, data));
        }
    };
    Ok(listed.as_array().and_then(|items| {
        items
            .iter()
            .find(|i| i.get("nickname").and_then(Value::as_str) == Some(nickname))
            .and_then(|i| {
                Some((
                    i.get("npub").and_then(Value::as_str)?.to_owned(),
                    i.get("default").and_then(Value::as_bool).unwrap_or(false),
                ))
            })
    }))
}

/// `npub` of the identity called `nickname` according to `identity list`.
async fn identity_npub(runner: &HyphaeRunner, nickname: &str) -> Result<Option<String>, CommError> {
    Ok(identity_entry(runner, nickname)
        .await?
        .map(|(npub, _)| npub))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityImportReq {
    nickname: String,
    path: String,
    backup_password: UnlockSecret,
    #[serde(default)]
    default: bool,
}

/// `POST /comm/identity/import` restores an identity from an NIP-49 backup
/// file: `hyphae identity import --nickname=N --password-stdin
/// --ncryptsec-stdin [--default]` with stdin `keystore password`, `backup
/// password`, `ncryptsec1…` (one per line, that order). The keystore follows
/// the same two flows as `create_identity` (first identity: generate a keychain
/// password under `KeystoreWriteLock` and promote it on success; later: reuse
/// the stored one), with the lock held for the whole import.
///
/// The same key already being present is not an error: it answers
/// `{status:"already_present", nickname?, npub?}`.
async fn import_identity(
    State(state): State<CommState>,
    payload: Result<Json<IdentityImportReq>, JsonRejection>,
) -> CommResult {
    let Json(req) =
        payload.map_err(|_| CommError::Invalid("invalid restore request".to_owned()))?;
    let (runner, password_store, home) = state.require_ready()?;
    invalid_if_bad_name("nickname", &req.nickname)?;
    check_nickname_len(&req.nickname)?;
    invalid_if_blank("path", &req.path)?;
    crate::export::validate_restore_password(&req.backup_password.0)?;
    let ncryptsec = crate::export::read_backup_file(Path::new(&req.path))?;
    crate::export::require_feature(runner, crate::export::Feature::Import).await?;

    let guard = runner.keystore_lock().acquire().await;
    let existing = resolve_account(home).await?;
    let (account, keystore_password, is_first_identity) = match existing {
        Some(account) => {
            let password = password_store
                .get(&account)
                .await
                .map_err(map_store_error)?;
            (account, password, false)
        }
        None => {
            let pending = Account::new_pending();
            let mut random32 = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rng(), &mut random32);
            let password = Password::generate(random32);
            password_store
                .put(&pending, &password)
                .await
                .map_err(map_store_error)?;
            (pending, password, true)
        }
    };
    let stdin = crate::export::import_stdin(&keystore_password, &req.backup_password.0, &ncryptsec);
    drop(keystore_password);
    let stdin = match stdin {
        Ok(stdin) => stdin,
        Err(e) => {
            if is_first_identity {
                let _ = resolve_first_identity_failure(home, password_store, &account).await;
            }
            return Err(e);
        }
    };
    let mut argv: Vec<OsString> = vec![
        "identity".into(),
        "import".into(),
        format!("--nickname={}", req.nickname).into(),
        "--ncryptsec-stdin".into(),
    ];
    if req.default {
        argv.push("--default".into());
    }
    let result = runner
        .run(Invocation {
            args: argv,
            password: Some(stdin),
            timeout: Some(Duration::from_secs(60)),
        })
        .await;

    match result {
        Ok(Envelope::Ok { data }) => {
            if is_first_identity {
                promote_first_identity_password(home, password_store, &account).await;
            }
            drop(guard);
            // Whether it ended up the default is read back from Hyphae, not
            // inferred from the request: Hyphae also makes an identity the
            // default when the keystore had none.
            let imported_nickname = data
                .get("nickname")
                .and_then(Value::as_str)
                .unwrap_or(&req.nickname)
                .to_owned();
            let became_default = identity_entry(runner, &imported_nickname)
                .await
                .ok()
                .flatten()
                .map_or(req.default || is_first_identity, |(_, is_default)| {
                    is_default
                });
            if became_default && let Some(daemon) = state.daemon.clone() {
                daemon.on_config_changed().await;
            }
            Ok(Json(json!({"ok": true, "data": {
                "status": "imported",
                "nickname": data.get("nickname").and_then(Value::as_str).unwrap_or(&req.nickname),
                "npub": data.get("npub").cloned().unwrap_or(Value::Null),
                "default": became_default,
            }})))
        }
        Ok(Envelope::Failed {
            error,
            message,
            data,
            ..
        }) => {
            let outcome = if is_first_identity {
                Some(resolve_first_identity_failure(home, password_store, &account).await)
            } else {
                None
            };
            drop(guard);
            match crate::export::classify_import_failure(&error, &message, data) {
                crate::export::ImportFailure::AlreadyPresent { message } => {
                    already_present(runner, &message).await
                }
                crate::export::ImportFailure::Failed(e) => {
                    Err(first_identity_failure_error(outcome, e))
                }
            }
        }
        Err(e) => {
            if let RunnerError::StillTerminating(reaped) = &e {
                hold_until_reaped(guard, reaped.clone());
                return Err(map_runner_error(e));
            }
            let outcome = if is_first_identity {
                Some(resolve_first_identity_failure(home, password_store, &account).await)
            } else {
                None
            };
            drop(guard);
            Err(first_identity_failure_error(outcome, map_runner_error(e)))
        }
    }
}

/// `{status:"already_present"}` plus the existing identity, when Hyphae's
/// message names one that `identity list` confirms.
async fn already_present(runner: &HyphaeRunner, message: &str) -> CommResult {
    let named = crate::export::nickname_from_npub_exists(message);
    let npub = match named {
        Some(nickname) => identity_npub(runner, nickname).await.ok().flatten(),
        None => None,
    };
    let nickname = npub.as_ref().and(named);
    Ok(Json(json!({"ok": true, "data": {
        "status": "already_present",
        "nickname": nickname,
        "npub": npub,
    }})))
}

struct UnlockSecret(Zeroizing<String>);

impl<'de> serde::Deserialize<'de> for UnlockSecret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(|value| Self(Zeroizing::new(value)))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnlockReq {
    password: UnlockSecret,
    #[serde(default)]
    remember: bool,
}

/// `POST /comm/unlock` verifies the supplied password with Hyphae's real,
/// read-only `identity check-password --password-stdin` before registering it
/// under the current keystore salt. No daemon or mutating Hyphae command is
/// involved.
async fn unlock(
    State(state): State<CommState>,
    payload: Result<Json<UnlockReq>, JsonRejection>,
) -> CommResult {
    let Json(req) = payload.map_err(|_| CommError::Invalid("invalid unlock request".to_owned()))?;
    let (runner, password_store, home) = state.require_ready()?;
    let password = Password::new(req.password.0.as_bytes().to_vec())
        .map_err(|e| CommError::Invalid(e.to_string()))?;
    let remember = req.remember;

    // Serialize against in-process identity creation/import and hold the
    // account stable until verification and registration have completed.
    let _guard = runner.keystore_lock().acquire().await;
    let account = resolve_account(home)
        .await?
        .ok_or_else(|| CommError::NotConfigured("no keystore exists yet".to_owned()))?;

    let envelope = runner
        .run(Invocation {
            args: vec!["identity".into(), "check-password".into()],
            password: Some(password.duplicate()),
            timeout: None,
        })
        .await
        .map_err(map_runner_error)?;
    match envelope {
        Envelope::Ok { data }
            if data.get("encrypted").and_then(Value::as_bool) == Some(true)
                && data.get("valid").and_then(Value::as_bool) == Some(true) => {}
        Envelope::Ok { .. } => {
            return Err(CommError::Upstream(
                "Hyphae check-password returned an invalid success envelope".to_owned(),
            ));
        }
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => return Err(map_envelope_failure(&error, &message, data)),
    }

    let verified_account = resolve_account(home).await?.ok_or_else(|| {
        CommError::Conflict(
            "keystore changed during password verification".to_owned(),
            None,
        )
    })?;
    if verified_account != account {
        return Err(CommError::Conflict(
            "keystore changed during password verification".to_owned(),
            None,
        ));
    }

    let remembered = password_store
        .register_verified(&verified_account, &password, remember)
        .await
        .map_err(map_store_error)?;
    Ok(Json(json!({"ok": true, "data": {
        "unlocked": true,
        "remembered": remembered,
    }})))
}

// ---------------------------------------------------------------------
// contact
// ---------------------------------------------------------------------

async fn list_contact(State(state): State<CommState>) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    run_read(runner, &["contact", "list"]).await
}

#[derive(Deserialize)]
struct ContactAddReq {
    nickname: String,
    npub: String,
    #[serde(default)]
    role: Option<String>,
}

/// `POST /comm/contact` → `contact add --nickname --npub [--role]` (§4: 🔒,
/// no ⚿). The npub is checked here, before Hyphae ever runs (G6: Hyphae
/// itself would report `other_error`, which the closed set would have to
/// render as the unhelpful `upstream`).
async fn add_contact(State(state): State<CommState>, Json(req): Json<ContactAddReq>) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    invalid_if_bad_name("nickname", &req.nickname)?;
    check_nickname_len(&req.nickname)?;
    if !is_valid_npub(&req.npub) {
        return Err(CommError::Invalid(format!(
            "{:?} is not a well-formed npub",
            req.npub
        )));
    }
    let mut argv: Vec<OsString> = vec![
        "contact".into(),
        "add".into(),
        "--nickname".into(),
        req.nickname.into(),
        "--npub".into(),
        req.npub.into(),
    ];
    if let Some(role) = req.role {
        argv.push("--role".into());
        argv.push(role.into());
    }
    let envelope = runner
        .run_keystore_write(Invocation {
            args: argv,
            password: None,
            timeout: None,
        })
        .await
        .map_err(map_runner_error)?;
    envelope_response(envelope)
}

// ---------------------------------------------------------------------
// relay
// ---------------------------------------------------------------------

/// `relay list`/`relay set` both answer the same shape (§4): Hyphae's own
/// `{relays,source}` plus a `configured` flag comm derives from `source`
/// (§9 R1: Hyphae falls back to a built-in default relay when none has been
/// set and reports `source:"default"` when it does — only
/// `source=="config"` means the user actually configured one). This is the
/// one place `relay`'s "not configured" fact is EXPOSED, not errored: `GET
/// /comm/relay`'s whole job is to report config state, so it always answers
/// 200 — a downstream operation that actually NEEDS a relay (`send`, daemon
/// start; both later tasks) is what returns `not_configured`.
fn relay_envelope_response(envelope: Envelope) -> CommResult {
    match envelope {
        Envelope::Ok { data } => {
            let configured = data.get("source").and_then(Value::as_str) == Some("config");
            let mut data = data;
            if let Value::Object(map) = &mut data {
                map.insert("configured".to_owned(), Value::Bool(configured));
            }
            Ok(Json(json!({"ok": true, "data": data})))
        }
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => Err(map_envelope_failure(&error, &message, data)),
    }
}

async fn list_relay(State(state): State<CommState>) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    let envelope = runner
        .run(read_invocation(&["relay", "list"]))
        .await
        .map_err(map_runner_error)?;
    relay_envelope_response(envelope)
}

const MAX_RELAYS: usize = 8;

#[derive(Deserialize)]
struct RelaySetReq {
    relays: Vec<String>,
}

/// `PUT /comm/relay` → **one call**: `relay set --relay U1 --relay U2 …`
/// (§4: a full replace, not an incremental add — COMM-0 r2's own
/// "Low" processing note: "relay set 改为一次调用").
async fn set_relay(State(state): State<CommState>, Json(req): Json<RelaySetReq>) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    if req.relays.is_empty() || req.relays.len() > MAX_RELAYS {
        return Err(CommError::Invalid(format!(
            "relays must have 1..={MAX_RELAYS} entries, got {}",
            req.relays.len()
        )));
    }
    for url in &req.relays {
        if !(url.starts_with("ws://") || url.starts_with("wss://")) {
            return Err(CommError::Invalid(format!(
                "{url:?} is not a ws:// or wss:// url"
            )));
        }
    }
    let mut argv: Vec<OsString> = vec!["relay".into(), "set".into()];
    for url in req.relays {
        argv.push("--relay".into());
        argv.push(url.into());
    }
    let envelope = runner
        .run(Invocation {
            args: argv,
            password: None,
            timeout: None,
        })
        .await
        .map_err(map_runner_error)?;
    // COMM-4a: the relay set just changed — restart the daemon if it's
    // running (COMM-HYPHAE.md §6.3), without touching its failure count.
    if matches!(envelope, Envelope::Ok { .. })
        && let Some(daemon) = state.daemon.clone()
    {
        daemon.on_config_changed().await;
    }
    relay_envelope_response(envelope)
}

#[derive(Deserialize, Default)]
struct RelayProbeReq {
    url: Option<String>,
}

/// Resolves the address `probe_relay` is about to probe when the caller
/// didn't name one explicitly, mirroring Hyphae's OWN `relay info`/`relay
/// list` resolution precedence (explicit > config > built-in default,
/// `internal/relayconfig`'s `Resolver::Resolve`, `internal/nostr/relay.go`'s
/// `relayInfo`: `result.Relays[0]` of that same resolution) by asking `relay
/// list` and taking its first address.
///
/// Codex COMM-4b review, Medium #4: without this, a probe with no explicit
/// `url` left the target entirely implicit — fine on success (Hyphae's own
/// success envelope echoes back the `url` it actually used), but a
/// `network_error` failure's envelope carries no `url` at all, so the
/// recorded probe ended up with `url: null` even though a real address was
/// dialed. Resolving it here means `probe_relay` always has an address to
/// pass explicitly to `relay info` and to record, on either outcome.
///
/// Codex COMM-4b review, round 2, Low: an explicit-but-empty `requested`
/// (`{"url": ""}`) is treated exactly like `None`, not like a real target —
/// Hyphae's own `relayInfo` (`internal/nostr/relay.go:70`, locked source:
/// `url := relayInfoURLArg; if url == "" { … resolve the default … }`) falls
/// through to its own config resolution when the CLI arg is empty, rather
/// than treating `""` as an address to dial. Before this fix, `Some("")`
/// matched the `if let Some(url) = requested` arm just like any other
/// non-empty string, short-circuiting straight past the `relay list`
/// fallback below and sending a literal empty string on to `relay info`.
///
/// Best-effort: if this auxiliary `relay list` call itself fails for any
/// reason, this returns `None` and `probe_relay` falls back to its old
/// behavior of leaving the explicit arg off `relay info`, letting Hyphae
/// resolve it internally — a failure here is not reason to fail the probe
/// itself.
async fn effective_probe_target(runner: &HyphaeRunner, requested: Option<&str>) -> Option<String> {
    if let Some(url) = requested.filter(|url| !url.is_empty()) {
        return Some(url.to_owned());
    }
    let envelope = runner.run(read_invocation(&["relay", "list"])).await.ok()?;
    let Envelope::Ok { data } = envelope else {
        return None;
    };
    data.get("relays")?
        .as_array()?
        .first()?
        .as_str()
        .map(str::to_owned)
}

/// `POST /comm/relay/probe` → `relay info <U> --timeout 5`, run only when a
/// caller explicitly asks (§4, §5.2: comm does not probe periodically —
/// that was cut, M12). Always expects a JSON body (`{}` when no `url`).
/// `<U>` is always passed explicitly now (via [`effective_probe_target`]),
/// even when the caller didn't name one — see that function's doc comment.
///
/// COMM-4b: a relay actually being down must not surface as an HTTP error —
/// Hyphae's own `relay info` contract (`docs/agent/cli-communication-contract.md`:
/// "失败走网络错误信封和退出码 2") is itself the probe's *negative result*,
/// not a failure of the probe operation. Only that specific `network_error`
/// exit is folded into `{connected:false}`; every other failure (bad args,
/// auth, an unresponsive/timing-out child, …) still surfaces as the matching
/// `CommError`, same as every other route. The result — success or
/// network-down — is always written into the daemon's `relay_probe` state
/// (COMM-HYPHAE.md §5.2) when a daemon supervisor is wired up; it is skipped
/// (not an error) on a `Ready` state built without one, same as
/// `set_relay`'s `on_config_changed` call.
async fn probe_relay(State(state): State<CommState>, Json(req): Json<RelayProbeReq>) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    let target = effective_probe_target(runner, req.url.as_deref()).await;
    let mut argv: Vec<OsString> = vec!["relay".into(), "info".into()];
    if let Some(url) = &target {
        argv.push(url.clone().into());
    }
    argv.push("--timeout".into());
    argv.push("5".into());
    let envelope = runner
        .run(Invocation {
            args: argv,
            password: None,
            timeout: Some(Duration::from_secs(7)),
        })
        .await
        .map_err(map_runner_error)?;

    let at_ms = crate::daemon::wall_clock_ms();
    let probe = match envelope {
        Envelope::Ok { data } => RelayProbeStatus {
            url: data
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| target.clone()),
            connected: data
                .get("connected")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            at_ms,
            error: None,
        },
        Envelope::Failed {
            exit: ExitClass::NetworkError,
            message,
            ..
        } => RelayProbeStatus {
            url: target.clone(),
            connected: false,
            at_ms,
            error: Some(message),
        },
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => return Err(map_envelope_failure(&error, &message, data)),
    };

    if let Some(daemon) = state.daemon.clone() {
        daemon.record_relay_probe(probe.clone());
    }
    Ok(Json(json!({
        "ok": true,
        "data": {"url": probe.url, "connected": probe.connected, "error": probe.error},
    })))
}

// ---------------------------------------------------------------------
// import (COMM-2b, COMM-HYPHAE.md §4.1)
// ---------------------------------------------------------------------

#[derive(Deserialize)]
struct ImportReq {
    from: String,
    #[serde(default)]
    confirm: bool,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

/// `POST /comm/import` → `crate::import::import` (§4.1). The confirm check
/// and the password-string-to-[`Password`] decode both happen here, before
/// the real work starts, same as every other route's own request
/// validation (`invalid_if_blank`, the npub check in `add_contact`, …) —
/// [`crate::import::import`] itself also checks `confirm` (defense in depth
/// for its other, non-HTTP callers), but a bad password length should read
/// as `invalid`, not bubble up from deep inside the import flow.
async fn import(State(state): State<CommState>, Json(req): Json<ImportReq>) -> CommResult {
    let (runner, password_store, home) = state.require_ready()?;
    if !req.confirm {
        return Err(CommError::ConfirmRequired);
    }
    let password = match req.password {
        Some(s) => {
            Some(Password::new(s.into_bytes()).map_err(|e| CommError::Invalid(e.to_string()))?)
        }
        None => None,
    };
    let report = crate::import::import(
        runner,
        password_store,
        home,
        crate::import::ImportRequest {
            from: PathBuf::from(req.from),
            confirm: req.confirm,
            password,
            dry_run: req.dry_run,
        },
    )
    .await?;
    Ok(Json(json!({"ok": true, "data": {
        "identities": report.identities,
        "contacts": report.contacts,
        "outbox": report.outbox,
        "db_files": report.db_files,
    }})))
}

// ---------------------------------------------------------------------
// send / history / inbox / outbox (COMM-3, COMM-HYPHAE.md §4, §5.1, §5.3)
// ---------------------------------------------------------------------

/// `from` / `as` default resolution (§4: "`from`/`as` 缺省时，取 `identity
/// list` 中 `default:true` 的那一项"). Shared by `send`, `history`, and
/// `inbox/pull`.
async fn resolve_default_nickname(runner: &HyphaeRunner) -> Result<String, CommError> {
    let envelope = runner
        .run(read_invocation(&["identity", "list"]))
        .await
        .map_err(map_runner_error)?;
    let data = match envelope {
        Envelope::Ok { data } => data,
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => return Err(map_envelope_failure(&error, &message, data)),
    };
    let identities = data
        .as_array()
        .ok_or_else(|| CommError::Upstream("identity list did not return an array".to_owned()))?;
    identities
        .iter()
        .find(|item| item.get("default").and_then(Value::as_bool) == Some(true))
        .and_then(|item| item.get("nickname").and_then(Value::as_str))
        .map(str::to_owned)
        .ok_or_else(|| CommError::NotConfigured("no default identity is configured".to_owned()))
}

/// §4's `not_configured` row ("relay 来源不是 `config`", R1): `send` must not
/// silently fall back to Hyphae's own built-in default relay. Returns the
/// configured relay count too, for the `5s × relay 数 + 10s` timeout rule
/// (§3).
async fn require_relay_configured(runner: &HyphaeRunner) -> Result<usize, CommError> {
    let envelope = runner
        .run(read_invocation(&["relay", "list"]))
        .await
        .map_err(map_runner_error)?;
    let data = match envelope {
        Envelope::Ok { data } => data,
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => return Err(map_envelope_failure(&error, &message, data)),
    };
    if data.get("source").and_then(Value::as_str) != Some("config") {
        return Err(CommError::NotConfigured(
            "no relay is configured (COMM-HYPHAE.md §9 R1)".to_owned(),
        ));
    }
    let count = data
        .get("relays")
        .and_then(Value::as_array)
        .map(|a| a.len().max(1))
        .unwrap_or(1);
    Ok(count)
}

/// Same relay-count lookup as [`require_relay_configured`], but for `outbox
/// retry`, which must still be attempted even if a caller cleared the relay
/// list after the item was queued — only the *timeout* depends on relay
/// count there, not a hard precondition. Any failure (including
/// `not_configured`) falls back to a one-relay timeout rather than
/// propagating.
async fn relay_count_best_effort(runner: &HyphaeRunner) -> usize {
    require_relay_configured(runner).await.unwrap_or(1)
}

fn send_timeout(relay_count: usize) -> Duration {
    Duration::from_secs(5 * relay_count as u64 + 10)
}

#[derive(Deserialize)]
struct SendReq {
    to: String,
    content: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default = "default_encrypt")]
    encrypt: bool,
}

fn default_encrypt() -> bool {
    true
}

/// §5.3's "part success" classification, applied only to `send`'s own data
/// shape (`published_to`/`history_stored`/…) — `retry`'s data shape is
/// different (`attempted`/`sent`/`queued`/`marked_failed`) and is never run
/// through this. `Ok(data)` carries an added `"layer"` field ("L1"/"L2",
/// §5.1); `Err` is the `partial` row of §4's closed set, data kept intact.
fn classify_send_result(data: Value) -> Result<Value, CommError> {
    let published_to = data
        .get("published_to")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let history_stored_false = data.get("history_stored").and_then(Value::as_bool) == Some(false);
    let queue_state_unknown =
        data.get("queue_state_unknown").and_then(Value::as_bool) == Some(true);
    // Hyphae's real `audit_error` field is a string (Go `AuditError string
    // `json:"audit_error,omitempty"``), not a bool — set to the underlying
    // error's message when `audit.LogAction` fails, and omitted entirely
    // otherwise. A failed audit log does not make Hyphae's own exit code
    // non-zero (it only prints a stderr warning outside `--json` mode), so
    // this is the only signal distinguishing "sent cleanly" from "sent, but
    // local bookkeeping may be wrong" (§5.3).
    let audit_error = data
        .get("audit_error")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if published_to > 0 && (history_stored_false || queue_state_unknown || audit_error) {
        return Err(CommError::Partial(data));
    }
    let layer = if published_to > 0 { "L2" } else { "L1" };
    let mut data = data;
    if let Value::Object(map) = &mut data {
        map.insert("layer".to_owned(), Value::String(layer.to_owned()));
    }
    Ok(data)
}

/// `POST /comm/send` → `agent msg --from F --to T --content C
/// [--encrypt=false] --password-stdin` (§4). There is deliberately no
/// "resend" anywhere in this router (§5.3: "任何情形都不提供「重发」") — the
/// only retry path for an already-sent message is `POST
/// /comm/outbox/{event_id}/retry`, which reuses the original event_id
/// rather than minting a new one.
async fn send_message(State(state): State<CommState>, Json(req): Json<SendReq>) -> CommResult {
    let (runner, password_store, home) = state.require_ready()?;
    invalid_if_blank("to", &req.to)?;
    invalid_if_blank("content", &req.content)?;

    let from = match req.from {
        Some(nickname) => nickname,
        None => resolve_default_nickname(runner).await?,
    };
    let relay_count = require_relay_configured(runner).await?;
    let account = resolve_account(home).await?.ok_or_else(|| {
        CommError::NotConfigured("no keystore exists yet; create an identity first".to_owned())
    })?;
    let password = password_store
        .get(&account)
        .await
        .map_err(map_store_error)?;

    let mut argv: Vec<OsString> = vec![
        "agent".into(),
        "msg".into(),
        "--from".into(),
        from.into(),
        "--to".into(),
        req.to.into(),
        "--content".into(),
        req.content.into(),
    ];
    if !req.encrypt {
        argv.push("--encrypt=false".into());
    }

    let envelope = runner
        .run(Invocation {
            args: argv,
            password: Some(password),
            timeout: Some(send_timeout(relay_count)),
        })
        .await
        .map_err(map_runner_error)?;

    match envelope {
        Envelope::Ok { data } => Ok(Json(
            json!({"ok": true, "data": classify_send_result(data)?}),
        )),
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => Err(map_envelope_failure(&error, &message, data)),
    }
}

#[derive(Deserialize, Default)]
struct HistoryQuery {
    #[serde(rename = "as")]
    as_: Option<String>,
    limit: Option<u32>,
}

/// `GET /comm/history?as=&limit=` → `history inbox --as A --limit N` (§4).
/// Pull-based, not push-based (JOINT-ROUND1 F1): a caller that wants fresh
/// inbound messages reflected here must call `POST /comm/inbox/pull` first
/// when no daemon is polling on its behalf.
async fn get_history(
    State(state): State<CommState>,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    let Query(q) = query.map_err(|e| CommError::Invalid(e.body_text()))?;
    if let Some(limit) = q.limit
        && !(1..=200).contains(&limit)
    {
        return Err(CommError::Invalid(format!(
            "limit must be in 1..=200, got {limit}"
        )));
    }
    let as_nickname = match q.as_ {
        Some(nickname) => nickname,
        None => resolve_default_nickname(runner).await?,
    };
    let limit_str = q.limit.map(|limit| limit.to_string());
    let mut argv: Vec<&str> = vec!["history", "inbox", "--as", &as_nickname];
    if let Some(limit_str) = &limit_str {
        argv.push("--limit");
        argv.push(limit_str);
    }
    run_read(runner, &argv).await
}

#[derive(Deserialize, Default)]
struct InboxPullReq {
    #[serde(rename = "as", default)]
    as_: Option<String>,
}

/// `POST /comm/inbox/pull` → `agent inbox --as A --password-stdin`.
///
/// **Not in COMM-HYPHAE.md §4's original route table** — added here per
/// JOINT-ROUND1 (`docs/comm/JOINT-ROUND1.md` F1): `history inbox` is purely
/// pull-based, so without a daemon polling `agent inbox` on a schedule
/// (COMM-4a's `--watch-interval`), nothing would ever make a newly-arrived
/// message show up under `GET /comm/history`. This route exists ONLY for
/// the no-daemon / manual case; once COMM-4a's daemon is running it already
/// calls `agent inbox` on its own interval and callers should not need
/// this. See `docs/design/COMM-HYPHAE.md` §4 for the added table row.
async fn pull_inbox(State(state): State<CommState>, Json(req): Json<InboxPullReq>) -> CommResult {
    let (runner, password_store, home) = state.require_ready()?;
    let as_nickname = match req.as_ {
        Some(nickname) => nickname,
        None => resolve_default_nickname(runner).await?,
    };
    let account = resolve_account(home).await?.ok_or_else(|| {
        CommError::NotConfigured("no keystore exists yet; create an identity first".to_owned())
    })?;
    let password = password_store
        .get(&account)
        .await
        .map_err(map_store_error)?;
    let envelope = runner
        .run(Invocation {
            args: vec![
                "agent".into(),
                "inbox".into(),
                "--as".into(),
                as_nickname.into(),
            ],
            password: Some(password),
            timeout: None,
        })
        .await
        .map_err(map_runner_error)?;
    envelope_response(envelope)
}

#[derive(Deserialize, Default)]
struct OutboxQuery {
    failed_only: Option<bool>,
}

/// `GET /comm/outbox?failed_only=` → `storage outbox list [--failed-only]`.
async fn list_outbox(
    State(state): State<CommState>,
    query: Result<Query<OutboxQuery>, axum::extract::rejection::QueryRejection>,
) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    let Query(q) = query.map_err(|e| CommError::Invalid(e.body_text()))?;
    if q.failed_only == Some(true) {
        run_read(runner, &["storage", "outbox", "list", "--failed-only"]).await
    } else {
        run_read(runner, &["storage", "outbox", "list"]).await
    }
}

/// Looks an outbox entry up by id before acting on it (§4's `not_found` row:
/// "outbox 条目不存在（comm 在调用前查证）"), and hands the matching entry
/// back — not just a boolean — so the caller can read its own `relays`
/// (Go's `types.OutboxEntry.Relays`) rather than only confirming existence.
async fn find_outbox_entry(runner: &HyphaeRunner, event_id: &str) -> Result<Value, CommError> {
    let envelope = runner
        .run(read_invocation(&["storage", "outbox", "list"]))
        .await
        .map_err(map_runner_error)?;
    let data = match envelope {
        Envelope::Ok { data } => data,
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => return Err(map_envelope_failure(&error, &message, data)),
    };
    let found = data.as_array().and_then(|entries| {
        entries
            .iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(event_id))
            .cloned()
    });
    found.ok_or_else(|| CommError::NotFound(format!("no outbox entry with id {event_id:?}")))
}

/// §5.1/§3's retry timeout rule ("`5s × relay 数 + 10s`") is defined in
/// terms of how many relays THIS attempt will actually dial — and Hyphae's
/// own `storage outbox retry` (`internal/messaging/outbox_commands.go`)
/// prefers the entry's own queued `relays` over whatever `relay list`
/// reports right now, falling back to the current config only when the
/// entry has none of its own. Computing the timeout from the current
/// config instead would silently under- or over-estimate it after the
/// caller reconfigures relays between enqueue and retry.
async fn relay_count_for_retry(runner: &HyphaeRunner, entry: &Value) -> usize {
    match entry.get("relays").and_then(Value::as_array) {
        Some(relays) if !relays.is_empty() => relays.len(),
        _ => relay_count_best_effort(runner).await,
    }
}

/// Forces `data.event_id` to `event_id` regardless of what Hyphae itself
/// echoed back (or omitted) — the structural guarantee behind "重试必须沿用
/// 原 event_id" (§4, §5.3): the id this router hands back is always the one
/// the caller asked to retry, never derived from trusting the subprocess.
fn with_event_id(mut data: Value, event_id: &str) -> Value {
    if let Value::Object(map) = &mut data {
        map.insert("event_id".to_owned(), Value::String(event_id.to_owned()));
    }
    data
}

/// `POST /comm/outbox/{event_id}/retry` → `storage outbox retry --id E`
/// (§4). No `--password-stdin`: retry re-signs nothing, it only re-attempts
/// publishing an already-signed, already-queued event (JOINT-ROUND1 §2 step
/// 4 confirms this runs unauthenticated against a real binary). This is the
/// ONLY retry/resend entry point in the whole comm router (§5.3).
async fn retry_outbox(
    State(state): State<CommState>,
    AxumPath(event_id): AxumPath<String>,
) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    invalid_if_blank("event_id", &event_id)?;
    let entry = find_outbox_entry(runner, &event_id).await?;
    let relay_count = relay_count_for_retry(runner, &entry).await;
    let envelope = runner
        .run(Invocation {
            args: vec![
                "storage".into(),
                "outbox".into(),
                "retry".into(),
                "--id".into(),
                event_id.clone().into(),
            ],
            password: None,
            timeout: Some(send_timeout(relay_count)),
        })
        .await
        .map_err(map_runner_error)?;
    match envelope {
        Envelope::Ok { data } => Ok(Json(
            json!({"ok": true, "data": with_event_id(data, &event_id)}),
        )),
        Envelope::Failed {
            error,
            message,
            data,
            ..
        } => Err(map_envelope_failure(
            &error,
            &message,
            data.map(|d| with_event_id(d, &event_id)),
        )),
    }
}

#[derive(Deserialize)]
struct OutboxClearReq {
    #[serde(default)]
    confirm: bool,
    #[serde(default)]
    min_failures: Option<u32>,
}

/// `POST /comm/outbox/clear` → `storage outbox clear --failed --yes
/// [--min-failures N]` (§4). Refuses without `confirm:true` (§4's
/// `confirm_required` row) — this only clears LOCAL outbox bookkeeping, it
/// never un-sends an event a relay already accepted (§4's own note on this
/// row, and §5.3: no operation here ever constructs a new event_id).
async fn clear_outbox(
    State(state): State<CommState>,
    Json(req): Json<OutboxClearReq>,
) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    if !req.confirm {
        return Err(CommError::ConfirmRequired);
    }
    let mut argv: Vec<OsString> = vec![
        "storage".into(),
        "outbox".into(),
        "clear".into(),
        "--failed".into(),
        "--yes".into(),
    ];
    if let Some(min_failures) = req.min_failures {
        argv.push("--min-failures".into());
        argv.push(min_failures.to_string().into());
    }
    let envelope = runner
        .run(Invocation {
            args: argv,
            password: None,
            timeout: None,
        })
        .await
        .map_err(map_runner_error)?;
    envelope_response(envelope)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::binary::{VerifiedBinary, sha256_of};
    use crate::password_store::{MemoryPasswordStore, StoreError};

    fn fixture_stage_flags(home: &std::path::Path) -> [bool; 4] {
        [
            "fixture-started",
            "fixture-after-stdin",
            "fixture-emitting",
            "fixture-emitted",
        ]
        .map(|name| home.join(name).is_file())
    }

    async fn install_fixture(dir: &Path, name: &str, script: &str) -> VerifiedBinary {
        let source_dir = dir.join("src");
        tokio::fs::create_dir_all(&source_dir).await.unwrap();
        let source = source_dir.join(name);
        tokio::fs::write(&source, script).await.unwrap();
        let bytes = tokio::fs::read(&source).await.unwrap();
        let expected = sha256_of(&bytes);
        VerifiedBinary::install(&source, expected, &dir.join("bin"))
            .await
            .unwrap()
    }

    async fn ready_state_with_script(dir: &Path, script: &str) -> CommState {
        let home = dir.join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let bin = install_fixture(dir, "hyphae-fake.sh", script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        CommState::ready(runner, Arc::new(MemoryPasswordStore::new()), home)
    }

    async fn call(
        router: Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let body = match body {
            Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
            None => Body::empty(),
        };
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body)
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    // ---- identity export / import (non-interactive Hyphae >= 0.28) ---------

    const FLAGS_EXPORT: &str = "--nickname --password-stdin --format --out";
    const FLAGS_IMPORT: &str = "--nickname --ncryptsec-stdin --password-stdin --default";

    /// A fake `hyphae`: answers `--help` probes, `identity list`, and the two
    /// new commands, logging every call's argv and stdin lines to `log`.
    struct Fake {
        log: PathBuf,
        export_help: String,
        import_help: String,
        export_body: String,
        import_body: String,
    }

    impl Fake {
        fn new(dir: &Path) -> Self {
            let npub = crate::export::testkit::npub();
            Self {
                log: dir.join("calls.log"),
                export_help: FLAGS_EXPORT.to_owned(),
                import_help: FLAGS_IMPORT.to_owned(),
                export_body: Fake::export_ok(&crate::export::testkit::sk(), "backup-pass-1", &npub),
                import_body: format!(
                    "echo '{{\"ok\":true,\"data\":{{\"nickname\":\"'\"$nick\"'\",\"npub\":\"{npub}\"}}}}'\n"
                ),
            }
        }

        /// Hyphae's `--format ncryptsec` answer: ciphertext on stdout, no file.
        fn export_ok(secret: &[u8; 32], password: &str, npub: &str) -> String {
            let nc = crate::export::testkit::encrypt_nip49(secret, password, 4);
            Self::export_json(npub, &nc)
        }

        fn export_json(npub: &str, ncryptsec: &str) -> String {
            format!(
                "echo '{{\"ok\":true,\"data\":{{\"nickname\":\"t1\",\"npub\":\"{npub}\",\"ncryptsec\":\"{ncryptsec}\"}}}}'\n"
            )
        }

        fn fail_import(mut self, code: &str, message: &str, exit: u8) -> Self {
            self.import_body = format!(
                "echo \"{{\\\"ok\\\":false,\\\"error\\\":\\\"{code}\\\",\\\"message\\\":\\\"{message}\\\"}}\" >&2\nexit {exit}\n"
            );
            self
        }

        fn script(&self) -> String {
            let npub = crate::export::testkit::npub();
            format!(
                r#"#!/bin/sh
LOG="{log}"
cmd="$1 $2"; all="$*"
if [ "$3" = "--help" ]; then
  if [ "$2" = "export" ]; then echo "{eh}"; else echo "{ih}"; fi
  exit 0
fi
if [ "$cmd" = "identity list" ]; then
  echo '{{"ok":true,"data":[{{"nickname":"t1","npub":"{npub}","default":true,"encrypted":true}},{{"nickname":"r1","npub":"{npub}","default":false,"encrypted":true}},{{"nickname":"auto","npub":"{npub}","default":true,"encrypted":true}}]}}'
  exit 0
fi
out=""; nick=""
for a in "$@"; do
  case "$a" in --nickname=*) nick="${{a#--nickname=}}";; esac
done
prev=""
for a in "$@"; do
  if [ "$prev" = "--out" ]; then out="$a"; fi
  prev="$a"
done
if [ "$cmd" = "identity export" ]; then
  IFS= read -r ks; IFS= read -r bp
  echo "export|$all|$ks|$bp" >> "$LOG"
{export_body}
  exit 0
fi
if [ "$cmd" = "identity import" ]; then
  IFS= read -r ks; IFS= read -r bp; IFS= read -r nc
  echo "import|$all|$ks|$bp|$nc" >> "$LOG"
  mkdir -p "$HOME/.hyphae"
  [ -f "$HOME/.hyphae/keystore.json" ] || echo '{{"salt":"c2FsdC1mb3ItdGVzdA=="}}' > "$HOME/.hyphae/keystore.json"
{import_body}
  exit 0
fi
echo '{{"ok":false,"error":"other_error","message":"unexpected: '"$all"'"}}' >&2; exit 4
"#,
                log = self.log.display(),
                eh = self.export_help,
                ih = self.import_help,
                export_body = self.export_body,
                import_body = self.import_body,
            )
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .map(|t| t.lines().map(str::to_owned).collect())
                .unwrap_or_default()
        }
    }

    async fn export_call(
        app: Router,
        nickname: &str,
        path: &Path,
        pw: &str,
    ) -> (StatusCode, Value) {
        call(
            app,
            "POST",
            "/identity/export",
            Some(json!({"nickname": nickname, "path": path.to_str().unwrap(), "backup_password": pw})),
        )
        .await
    }

    async fn import_call(
        app: Router,
        nickname: &str,
        path: &Path,
        pw: &str,
        default: bool,
    ) -> (StatusCode, Value) {
        call(
            app,
            "POST",
            "/identity/import",
            Some(json!({"nickname": nickname, "path": path.to_str().unwrap(), "backup_password": pw, "default": default})),
        )
        .await
    }

    fn write_backup(dir: &Path) -> PathBuf {
        let p = dir.join("in.ncryptsec");
        std::fs::write(&p, format!("{}\n", crate::export::testkit::SPEC_NCRYPTSEC)).unwrap();
        p
    }

    #[tokio::test]
    async fn export_drives_hyphae_without_out_verifies_the_backup_then_writes_it_itself() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path());
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let out = tmp.path().join("out");
        tokio::fs::create_dir_all(&out).await.unwrap();
        let dest = out.join("t1.ncryptsec");
        let (status, body) = export_call(router(state), "t1", &dest, "backup-pass-1").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        let text = body.to_string();
        assert!(
            !text.contains("ncryptsec1")
                && !text.contains("nsec1")
                && !text.contains("backup-pass-1"),
            "{text}"
        );
        assert_eq!(
            body["data"]["npub"],
            crate::export::testkit::npub().as_str()
        );
        assert_eq!(body["data"]["format"], "ncryptsec");

        // argv + stdin order: keystore password first, then the backup password.
        // No --out: the ciphertext comes back on stdout and the staging name
        // never reaches Hyphae.
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let parts: Vec<&str> = calls[0].split('|').collect();
        assert_eq!(parts[0], "export");
        assert_eq!(
            parts[1],
            "identity export --nickname=t1 --format ncryptsec --password-stdin"
        );
        assert_eq!((parts[2], parts[3]), ("test-password", "backup-pass-1"));

        // the daemon created it: 0600, one link, nothing else in the folder, and
        // it opens with the user's password to this identity's key
        let meta = std::fs::metadata(&dest).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.nlink(), 1);
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
        let file = std::fs::read_to_string(&dest).unwrap();
        assert!(file.starts_with("ncryptsec1") && file.ends_with('\n'));
        let key = crate::export::decrypt_nip49(file.trim(), "backup-pass-1").unwrap();
        assert_eq!(
            crate::export::npub_of_secret(&key).unwrap(),
            crate::export::testkit::npub()
        );
        assert!(crate::export::decrypt_nip49(file.trim(), "some-other-pass").is_err());
    }

    #[tokio::test]
    async fn export_and_restore_say_hyphae_too_old_when_the_flags_are_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fake = Fake::new(tmp.path());
        // what v0.27.2 prints: no --password-stdin / --format / --out on export,
        // and no `identity import` at all
        fake.export_help = "--nickname".to_owned();
        fake.import_help = "No help topic".to_owned();
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let app = router(state);
        let dest = tmp.path().join("t1.ncryptsec");
        let (status, body) = export_call(app.clone(), "t1", &dest, "backup-pass-1").await;
        assert_eq!(status, StatusCode::CONFLICT, "{body:?}");
        assert_eq!(body["error"], "hyphae_too_old");
        assert!(body["message"].as_str().unwrap().contains("v0.28.0"));
        let (status, body) =
            import_call(app, "r9", &write_backup(tmp.path()), "nostr", false).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body:?}");
        assert_eq!(body["error"], "hyphae_too_old");
        assert!(
            fake.calls().is_empty(),
            "nothing may be run on an old binary"
        );
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn export_refuses_an_existing_target_without_running_hyphae() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path());
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let dest = tmp.path().join("exists.ncryptsec");
        std::fs::write(&dest, "precious").unwrap();
        let (status, body) = export_call(router(state), "t1", &dest, "backup-pass-1").await;
        assert_eq!(status, StatusCode::CONFLICT, "{body:?}");
        assert_eq!(body["data"]["reason"], "target_exists");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "precious");
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn export_writes_nothing_unless_the_backup_opens_with_the_users_password_to_this_identity()
     {
        use crate::export::testkit::{SPEC_NCRYPTSEC, encrypt_nip49, npub, sk};
        let tmp = tempfile::tempdir().unwrap();
        let other = [9u8; 32];
        let mut flipped = encrypt_nip49(&sk(), "backup-pass-1", 4);
        flipped.pop();
        flipped.push(if flipped.ends_with('q') { 'p' } else { 'q' });
        let cases = [
            // encrypted with some OTHER password (e.g. an unencrypted keystore made
            // Hyphae take our first stdin line, the keystore password, as the
            // backup password)
            (
                "wrong password",
                Fake::export_json(&npub(), &encrypt_nip49(&sk(), "test-password", 4)),
            ),
            (
                "the spec vector (password nostr)",
                Fake::export_json(&npub(), SPEC_NCRYPTSEC),
            ),
            // opens, but it is a different identity's key
            (
                "someone else's key",
                Fake::export_json(&npub(), &encrypt_nip49(&other, "backup-pass-1", 4)),
            ),
            // Hyphae reports another identity
            (
                "reports another npub",
                Fake::export_json("npub1other", &encrypt_nip49(&sk(), "backup-pass-1", 4)),
            ),
            ("bad checksum", Fake::export_json(&npub(), &flipped)),
            ("not an ncryptsec", Fake::export_json(&npub(), "hello")),
            (
                "no ncryptsec field",
                "echo '{\"ok\":true,\"data\":{\"nickname\":\"t1\"}}'\n".to_owned(),
            ),
        ];
        for (name, body) in cases {
            let mut fake = Fake::new(tmp.path());
            fake.export_body = body;
            let sub = tempfile::tempdir_in(tmp.path()).unwrap();
            let state = ready_state_with_identity(sub.path(), &fake.script()).await;
            let dest = sub.path().join("t1.ncryptsec");
            let (status, resp) = export_call(router(state), "t1", &dest, "backup-pass-1").await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{name}: {resp:?}");
            assert!(!dest.exists(), "{name}");
            let leftovers: Vec<_> = std::fs::read_dir(sub.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(".agent24-backup-"))
                .collect();
            assert!(leftovers.is_empty(), "{name}: {leftovers:?}");
        }
    }

    #[tokio::test]
    async fn export_maps_hyphae_failures_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        for (body, expected, needle) in [
            (
                "echo '{\"ok\":false,\"error\":\"auth_error\",\"message\":\"failed to unlock keystore\"}' >&2; exit 3\n",
                StatusCode::LOCKED,
                "locked",
            ),
            (
                "echo '{\"ok\":false,\"error\":\"user_error\",\"message\":\"unsupported --format\"}' >&2; exit 1\n",
                StatusCode::BAD_REQUEST,
                "invalid",
            ),
        ] {
            let mut fake = Fake::new(tmp.path());
            fake.export_body = body.to_owned();
            let sub = tempfile::tempdir_in(tmp.path()).unwrap();
            let state = ready_state_with_identity(sub.path(), &fake.script()).await;
            let (status, resp) = export_call(
                router(state),
                "t1",
                &sub.path().join("t1.ncryptsec"),
                "backup-pass-1",
            )
            .await;
            assert_eq!(status, expected, "{resp:?}");
            assert_eq!(resp["error"], needle);
            assert_eq!(
                std::fs::read_dir(sub.path())
                    .unwrap()
                    .filter(|e| e
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".agent24"))
                    .count(),
                0
            );
        }
    }

    #[tokio::test]
    async fn export_user_error_from_hyphae_is_explained_as_an_unencrypted_keystore() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fake = Fake::new(tmp.path());
        fake.export_body = "echo '{\"ok\":false,\"error\":\"user_error\",\"message\":\"keystore is not encrypted\"}' >&2; exit 1\n".to_owned();
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let (status, resp) = export_call(
            router(state),
            "t1",
            &tmp.path().join("t1.ncryptsec"),
            "backup-pass-1",
        )
        .await;
        assert_eq!(
            (status, resp["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid")),
            "{resp:?}"
        );
        assert!(
            resp["message"]
                .as_str()
                .unwrap()
                .contains("keystore must be encrypted"),
            "{resp:?}"
        );
        assert!(!tmp.path().join("t1.ncryptsec").exists());
    }

    #[tokio::test]
    async fn export_validates_the_request_before_running_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path());
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let app = router(state);
        let ok = tmp.path().join("x.ncryptsec");
        let (s, _) = export_call(app.clone(), "t1", Path::new("rel.txt"), "backup-pass-1").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        for bad in [
            "short",
            "bad\nline-pass",
            "bad\rline-pass",
            "bad\u{0}nul-pass",
            "          ",
        ] {
            let (s, b) = export_call(app.clone(), "t1", &ok, bad).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{bad:?} {b:?}");
        }
        let (s, _) = export_call(app.clone(), "t1\nNsec: x", &ok, "backup-pass-1").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = export_call(app.clone(), "nobody", &ok, "backup-pass-1").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = call(app.clone(), "POST", "/identity/export",
            Some(json!({"nickname": "t1", "path": "/tmp/x", "backup_password": "backup-pass-1", "extra": 1}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = call(
            app,
            "POST",
            "/identity/export",
            Some(json!({"nickname": "t1", "path": "/tmp/x"})),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(fake.calls().is_empty());
        assert!(!ok.exists());
    }

    #[tokio::test]
    async fn control_characters_are_rejected_in_names_everywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path());
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let app = router(state);
        for (uri, body) in [
            ("/identity", json!({"nickname": "a\nb"})),
            ("/identity/default", json!({"nickname": "a\nb"})),
            (
                "/contact",
                json!({"nickname": "a\u{7}b", "npub": crate::export::testkit::npub()}),
            ),
            (
                "/identity/import",
                json!({"nickname": "a\nb", "path": "/tmp/x", "backup_password": "x"}),
            ),
        ] {
            let (s, b) = call(app.clone(), "POST", uri, Some(body)).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{uri} {b:?}");
        }
    }

    #[tokio::test]
    async fn nicknames_follow_one_length_contract_but_old_long_names_keep_working() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path());
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let app = router(state);
        let file = write_backup(tmp.path());
        let npub = crate::export::testkit::npub();
        let long = "x".repeat(129);
        let max = "y".repeat(128);
        let wide = "𝔘".repeat(129); // 129 characters, 516 bytes
        for name in [&long, &wide] {
            for (uri, body) in [
                ("/identity", json!({"nickname": name})),
                ("/contact", json!({"nickname": name, "npub": npub})),
                (
                    "/identity/import",
                    json!({"nickname": name, "path": file, "backup_password": "x"}),
                ),
                (
                    "/identity/export",
                    json!({"nickname": name, "path": "/tmp/x", "backup_password": "backup-pass-1"}),
                ),
            ] {
                let (s, b) = call(app.clone(), "POST", uri, Some(body)).await;
                assert_eq!(
                    (s, b["error"].as_str()),
                    (StatusCode::BAD_REQUEST, Some("nickname_too_long")),
                    "{uri} {b:?}"
                );
            }
        }
        // exactly at the limit passes this check (create then reaches the fake hyphae)
        let (_, b) = call(
            app.clone(),
            "POST",
            "/identity",
            Some(json!({"nickname": max})),
        )
        .await;
        assert_ne!(b["error"].as_str(), Some("nickname_too_long"), "{b:?}");
        // an existing long name can still be made the default
        let (_, b) = call(
            app,
            "POST",
            "/identity/default",
            Some(json!({"nickname": long})),
        )
        .await;
        assert_ne!(b["error"].as_str(), Some("nickname_too_long"), "{b:?}");
    }

    #[tokio::test]
    async fn a_second_concurrent_export_gets_export_in_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fake = Fake::new(tmp.path());
        fake.export_body = format!("sleep 2\n{}", fake.export_body);
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let app = router(state);
        let first = tmp.path().join("one.ncryptsec");
        let second = tmp.path().join("two.ncryptsec");
        let running = tokio::spawn({
            let (app, first) = (app.clone(), first.clone());
            async move { export_call(app, "t1", &first, "backup-pass-1").await }
        });
        tokio::time::sleep(Duration::from_millis(700)).await;
        let (status, body) = export_call(app.clone(), "t1", &second, "backup-pass-1").await;
        assert_eq!(status, StatusCode::CONFLICT, "{body:?}");
        assert_eq!(body["data"]["reason"], "export_in_progress");
        let (status, body) = running.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body:?}");
        let (status, _) = export_call(app, "t1", &second, "backup-pass-1").await;
        assert_eq!(status, StatusCode::OK, "the gate is released afterwards");
    }

    #[tokio::test]
    async fn restore_into_an_existing_keystore_uses_the_documented_argv_and_stdin_order() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path());
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let file = write_backup(tmp.path());
        let (status, body) = import_call(router(state), "r2", &file, "backup-pass-1", true).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["data"]["status"], "imported");
        assert_eq!(body["data"]["nickname"], "r2");
        assert_eq!(body["data"]["default"], true);
        let text = body.to_string();
        assert!(!text.contains("backup-pass-1") && !text.contains("ncryptsec1"));
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let parts: Vec<&str> = calls[0].split('|').collect();
        assert_eq!(parts[0], "import");
        assert_eq!(
            parts[1],
            "identity import --nickname=r2 --ncryptsec-stdin --default --password-stdin"
        );
        // stdin: keystore password (from our keychain), backup password, ncryptsec LAST
        assert_eq!(parts[2], "test-password");
        assert_eq!(parts[3], "backup-pass-1");
        assert_eq!(parts[4], crate::export::testkit::SPEC_NCRYPTSEC);
    }

    #[tokio::test]
    async fn restore_as_the_first_identity_generates_and_promotes_a_keychain_password() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path());
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", &fake.script()).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let store = Arc::new(MemoryPasswordStore::new());
        let app = router(CommState::ready(runner, store.clone(), home));
        let file = write_backup(tmp.path());
        let (status, body) = import_call(app, "first", &file, "backup-pass-1", false).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(
            body["data"]["default"], true,
            "the first identity is the default"
        );
        let parts: Vec<String> = fake.calls()[0].split('|').map(str::to_owned).collect();
        assert_eq!(parts[2].len(), 43, "a generated keystore password was used");
        // ... and it was promoted onto the keystore's salt account
        let promoted = store
            .get(&Account::from_salt("c2FsdC1mb3ItdGVzdA=="))
            .await
            .unwrap();
        assert_eq!(promoted.as_bytes(), parts[2].as_bytes());
    }

    #[tokio::test]
    async fn restore_first_identity_failure_leaves_no_keychain_entry_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fake = Fake::new(tmp.path());
        // fails before writing a keystore: the fake's keystore.json write is skipped
        fake.import_body = "rm -f \"$HOME/.hyphae/keystore.json\"\necho '{\"ok\":false,\"error\":\"bad_backup_password\",\"message\":\"x\"}' >&2\nexit 3\n".to_owned();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", &fake.script()).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let store = Arc::new(MemoryPasswordStore::new());
        let app = router(CommState::ready(runner, store.clone(), home.clone()));
        let (status, body) =
            import_call(app, "first", &write_backup(tmp.path()), "wrong", false).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(body["error"], "bad_backup_password");
        assert!(!home.join(".hyphae").join("keystore.json").exists());
        assert!(matches!(
            store.get(&Account::from_salt("c2FsdC1mb3ItdGVzdA==")).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn restore_maps_each_hyphae_error_code_by_its_string() {
        let tmp = tempfile::tempdir().unwrap();
        let file = write_backup(tmp.path());
        // exit codes are shared (1, 3) on purpose: the string decides
        for (code, exit, status) in [
            ("bad_backup_password", 3, StatusCode::BAD_REQUEST),
            ("invalid_key", 1, StatusCode::BAD_REQUEST),
            ("nickname_exists", 1, StatusCode::CONFLICT),
            ("keystore_locked", 3, StatusCode::LOCKED),
        ] {
            let fake = Fake::new(tmp.path()).fail_import(code, "hyphae says no", exit);
            let sub = tempfile::tempdir_in(tmp.path()).unwrap();
            let state = ready_state_with_identity(sub.path(), &fake.script()).await;
            let (s, body) = import_call(router(state), "r3", &file, "backup-pass-1", false).await;
            assert_eq!(s, status, "{code} {body:?}");
            assert_eq!(body["error"], code);
        }
        // Hyphae's release notes: a keystore password changed by another process
        // answers a plain auth_error on import; it means the same thing to us
        let fake = Fake::new(tmp.path()).fail_import("auth_error", "failed to unlock keystore", 3);
        let sub = tempfile::tempdir_in(tmp.path()).unwrap();
        let state = ready_state_with_identity(sub.path(), &fake.script()).await;
        let (s, body) = import_call(router(state), "r3", &file, "backup-pass-1", false).await;
        assert_eq!(
            (s, body["error"].as_str()),
            (StatusCode::LOCKED, Some("keystore_locked")),
            "{body:?}"
        );
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("another program")
        );
        // a code we do not know is NOT mistaken for one we do
        let fake = Fake::new(tmp.path()).fail_import("user_error", "bad flag", 1);
        let sub = tempfile::tempdir_in(tmp.path()).unwrap();
        let state = ready_state_with_identity(sub.path(), &fake.script()).await;
        let (s, body) = import_call(router(state), "r3", &file, "backup-pass-1", false).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid");
    }

    #[tokio::test]
    async fn the_reported_default_flag_comes_from_identity_list_not_from_the_request() {
        let tmp = tempfile::tempdir().unwrap();
        let file = write_backup(tmp.path());
        let fake = Fake::new(tmp.path());
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let app = router(state);
        // Hyphae made "auto" the default although the request did not ask for it
        let (s, body) = import_call(app.clone(), "auto", &file, "backup-pass-1", false).await;
        assert_eq!(s, StatusCode::OK, "{body:?}");
        assert_eq!(body["data"]["default"], true);
        // the request asked for the default but Hyphae did not set it
        let (s, body) = import_call(app, "r1", &file, "backup-pass-1", true).await;
        assert_eq!(s, StatusCode::OK, "{body:?}");
        assert_eq!(body["data"]["default"], false);
    }

    #[tokio::test]
    async fn invisible_formatting_and_bidi_characters_are_rejected_in_names() {
        assert!(
            !is_format_control('a')
                && !is_format_control('张')
                && !is_format_control('\u{200C}')
                && !is_format_control('\u{200D}')
        );
        for c in [
            '\u{202E}', '\u{202A}', '\u{2066}', '\u{2069}', '\u{200B}', '\u{200F}', '\u{FEFF}',
            '\u{00AD}', '\u{2060}',
        ] {
            assert!(is_format_control(c), "{c:?}");
        }
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path());
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let app = router(state);
        let file = write_backup(tmp.path());
        let npub = crate::export::testkit::npub();
        for name in [
            "al\u{202E}ice",
            "al\u{2066}ice",
            "al\u{200B}ice",
            "\u{FEFF}alice",
        ] {
            for (uri, body) in [
                ("/identity", json!({"nickname": name})),
                ("/identity/default", json!({"nickname": name})),
                ("/contact", json!({"nickname": name, "npub": npub})),
                (
                    "/identity/import",
                    json!({"nickname": name, "path": file, "backup_password": "x"}),
                ),
                (
                    "/identity/export",
                    json!({"nickname": name, "path": "/tmp/x", "backup_password": "backup-pass-1"}),
                ),
            ] {
                let (s, b) = call(app.clone(), "POST", uri, Some(body)).await;
                assert_eq!(s, StatusCode::BAD_REQUEST, "{uri} {name:?} {b:?}");
            }
        }
        // joiners used by real scripts are fine (this one reaches the fake, which refuses it)
        let (s, _) = call(
            app,
            "POST",
            "/identity/default",
            Some(json!({"nickname": "می\u{200C}خواهم"})),
        )
        .await;
        assert_ne!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn restoring_a_key_that_is_already_stored_is_already_present_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let file = write_backup(tmp.path());
        let npub = crate::export::testkit::npub();
        let fake = Fake::new(tmp.path()).fail_import(
            "npub_exists",
            "this key is already stored as identity 'r1'",
            1,
        );
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let (s, body) = import_call(router(state), "again", &file, "backup-pass-1", false).await;
        assert_eq!(s, StatusCode::OK, "{body:?}");
        assert_eq!(body["data"]["status"], "already_present");
        assert_eq!(body["data"]["nickname"], "r1");
        assert_eq!(body["data"]["npub"], npub.as_str());
        // a named identity that `identity list` does not confirm is not echoed
        let sub = tempfile::tempdir_in(tmp.path()).unwrap();
        let fake = Fake::new(sub.path()).fail_import(
            "npub_exists",
            "this key is already stored as identity 'ghost'",
            1,
        );
        let state = ready_state_with_identity(sub.path(), &fake.script()).await;
        let (s, body) = import_call(router(state), "again", &file, "backup-pass-1", false).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["data"]["status"], "already_present");
        assert!(
            body["data"]["nickname"].is_null() && body["data"]["npub"].is_null(),
            "{body:?}"
        );
    }

    #[tokio::test]
    async fn restore_refuses_bad_files_and_passwords_before_running_hyphae() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(tmp.path());
        let state = ready_state_with_identity(tmp.path(), &fake.script()).await;
        let app = router(state);
        let good = write_backup(tmp.path());
        let plain = tmp.path().join("plain.txt");
        std::fs::write(&plain, "hello\n").unwrap();
        let link = tmp.path().join("link.ncryptsec");
        std::os::unix::fs::symlink(&good, &link).unwrap();
        let big = tmp.path().join("big.ncryptsec");
        std::fs::write(&big, format!("ncryptsec1{}", "q".repeat(5000))).unwrap();
        for (path, expect) in [
            (&plain, "invalid_key"),
            (&link, "invalid"),
            (&big, "invalid"),
            (&tmp.path().join("missing.ncryptsec"), "not_found"),
        ] {
            let (s, b) = import_call(app.clone(), "r4", path, "backup-pass-1", false).await;
            assert!(s.is_client_error(), "{path:?} {b:?}");
            assert_eq!(b["error"], expect, "{path:?}");
        }
        for bad in ["", "two\nlines", "cr\rhere", "nul\u{0}here"] {
            let (s, b) = import_call(app.clone(), "r4", &good, bad, false).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{bad:?} {b:?}");
        }
        let (s, _) = import_call(app, "r4", Path::new("relative"), "x", false).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(fake.calls().is_empty());
    }

    // ---- unconfigured / binary_rejected: every route, no binary touched --

    #[tokio::test]
    async fn unconfigured_state_reports_not_configured_on_every_route() {
        let app = router(CommState::unconfigured("no binary set"));
        let (status, body) = call(app, "GET", "/identity", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "not_configured");
        assert_eq!(body["ok"], false);
    }

    #[tokio::test]
    async fn binary_rejected_state_reports_binary_rejected() {
        let app = router(CommState::binary_rejected("hash mismatch"));
        let (status, body) = call(app, "GET", "/relay", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "binary_rejected");
    }

    // ---- relay: configured flag derived from `source` ---------------------

    #[tokio::test]
    async fn relay_list_exposes_unconfigured_source_without_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        let script =
            "#!/bin/sh\necho '{\"ok\":true,\"data\":{\"relays\":[],\"source\":\"default\"}}'\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(router(state), "GET", "/relay", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["configured"], false);
    }

    #[tokio::test]
    async fn relay_list_reports_configured_true_when_source_is_config() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho '{\"ok\":true,\"data\":{\"relays\":[\"wss://r\"],\"source\":\"config\"}}'\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(router(state), "GET", "/relay", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["configured"], true);
    }

    #[tokio::test]
    async fn relay_set_rejects_more_than_eight_relays() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho '{\"ok\":true,\"data\":{}}'\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let relays: Vec<String> = (0..9).map(|i| format!("wss://r{i}")).collect();
        let (status, body) = call(
            router(state),
            "PUT",
            "/relay",
            Some(json!({"relays": relays})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid");
    }

    #[tokio::test]
    async fn relay_set_rejects_a_non_ws_url() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho '{\"ok\":true,\"data\":{}}'\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "PUT",
            "/relay",
            Some(json!({"relays": ["https://not-ws"]})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid");
    }

    // ---- contact: npub validated before Hyphae ever runs -------------------

    #[tokio::test]
    async fn contact_add_rejects_a_malformed_npub_without_invoking_hyphae() {
        let tmp = tempfile::tempdir().unwrap();
        // A script that would fail the test if it were ever invoked.
        let script = "#!/bin/sh\necho '{\"ok\":false,\"error\":\"other_error\",\"message\":\"should \
                       never run\"}' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/contact",
            Some(json!({"nickname": "bob", "npub": "not-a-valid-npub"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid");
    }

    // ---- closed error set: auth_error -> locked ----------------------------

    #[tokio::test]
    async fn identity_use_maps_auth_error_to_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho '{\"ok\":false,\"error\":\"auth_error\",\"message\":\"incorrect \
                       password\"}' >&2\nexit 3\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/identity/default",
            Some(json!({"nickname": "alice"})),
        )
        .await;
        assert_eq!(status, StatusCode::LOCKED);
        assert_eq!(body["error"], "locked");
    }

    async fn write_unlock_keystore(home: &Path, salt: &str) {
        let hyphae = home.join(".hyphae");
        tokio::fs::create_dir_all(&hyphae).await.unwrap();
        tokio::fs::write(
            hyphae.join("keystore.json"),
            serde_json::to_vec(&json!({"salt": salt})).unwrap(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unlock_checks_real_command_registers_memory_session_and_needs_no_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        write_unlock_keystore(&home, "salt-before").await;
        let script = "#!/bin/sh\n[ \"$1 $2 $3\" = \"identity check-password --password-stdin\" ] || exit 90\ncat >/dev/null\nprintf '%s\\n' '{\"ok\":true,\"data\":{\"encrypted\":true,\"valid\":true}}'\n";
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let store = Arc::new(MemoryPasswordStore::new());
        let account = Account::from_salt("salt-before");
        let state = CommState::ready(runner, store.clone(), home);

        let (status, body) = call(
            router(state),
            "POST",
            "/unlock",
            Some(json!({"password":"verified-pass"})),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["data"]["unlocked"], true);
        assert_eq!(body["data"]["remembered"], false);
        assert_eq!(
            store.get(&account).await.unwrap().as_bytes(),
            b"verified-pass"
        );
    }

    #[tokio::test]
    async fn unlock_maps_auth_error_to_locked_without_registering_password() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        write_unlock_keystore(&home, "salt-auth").await;
        let script = r#"#!/bin/sh
printf '' > "$HOME/fixture-started"
cat >/dev/null
printf '' > "$HOME/fixture-after-stdin"
printf '' > "$HOME/fixture-emitting"
printf '%s\n' '{"ok":false,"error":"auth_error","message":"password verification failed"}' >&2
printf '' > "$HOME/fixture-emitted"
exit 3
"#;
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let store = Arc::new(MemoryPasswordStore::new());
        let account = Account::from_salt("salt-auth");
        let state = CommState::ready(runner, store.clone(), home.clone());

        let (status, body) = call(
            router(state),
            "POST",
            "/unlock",
            Some(json!({"password":"wrong"})),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::LOCKED,
            "fixture stages: {:?}",
            fixture_stage_flags(&home)
        );
        assert_eq!(fixture_stage_flags(&home), [true; 4]);
        assert_eq!(body["error"], "locked");
        assert!(matches!(
            store.get(&account).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn unlock_rechecks_salt_before_registering() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        write_unlock_keystore(&home, "salt-before").await;
        let script = format!(
            r#"#!/bin/sh
printf '' > "$HOME/fixture-started"
cat >/dev/null
printf '' > "$HOME/fixture-after-stdin"
printf '%s\n' '{{"salt":"salt-after"}}' > '{}'
printf '' > "$HOME/fixture-emitting"
printf '%s\n' '{{"ok":true,"data":{{"encrypted":true,"valid":true}}}}'
printf '' > "$HOME/fixture-emitted"
"#,
            home.join(".hyphae").join("keystore.json").display()
        );
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", &script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let store = Arc::new(MemoryPasswordStore::new());
        let previous = Account::from_salt("salt-before");
        let current = Account::from_salt("salt-after");
        let state = CommState::ready(runner, store.clone(), home.clone());

        let (status, _body) = call(
            router(state),
            "POST",
            "/unlock",
            Some(json!({"password":"verified-pass"})),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "fixture stages: {:?}",
            fixture_stage_flags(&home)
        );
        assert_eq!(fixture_stage_flags(&home), [true; 4]);
        assert!(matches!(
            store.get(&previous).await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.get(&current).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn unlock_rejects_invalid_password_length_before_invoking_hyphae() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        write_unlock_keystore(&home, "salt-invalid").await;
        let marker = tmp.path().join("invoked");
        let script = format!(
            "#!/bin/sh\ntouch '{}'\nprintf '%s\\n' '{{\"ok\":true,\"data\":{{\"encrypted\":true,\"valid\":true}}}}'\n",
            marker.display()
        );
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", &script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let state = CommState::ready(runner, Arc::new(MemoryPasswordStore::new()), home);

        let (status, body) = call(
            router(state),
            "POST",
            "/unlock",
            Some(json!({"password":""})),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(body["error"], "invalid");
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn unlock_rejects_success_envelope_without_true_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        write_unlock_keystore(&home, "salt-flags").await;
        let script = "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"ok\":true,\"data\":{\"encrypted\":true,\"valid\":false}}'\n";
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let store = Arc::new(MemoryPasswordStore::new());
        let account = Account::from_salt("salt-flags");
        let state = CommState::ready(runner, store.clone(), home);

        let (status, body) = call(
            router(state),
            "POST",
            "/unlock",
            Some(json!({"password":"candidate"})),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
        assert_eq!(body["error"], "upstream");
        assert!(matches!(
            store.get(&account).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn unlock_json_rejections_are_fixed_invalid_responses() {
        let tmp = tempfile::tempdir().unwrap();
        let state =
            ready_state_with_script(tmp.path(), "#!/bin/sh\necho should-not-run >&2\nexit 4\n")
                .await;
        for body in [
            r#"{"password":"candidate","extra":true}"#,
            r#"{"password":42}"#,
            r#"{"remember":true}"#,
            r#"{"password":"candidate","remember":"yes"}"#,
            r#"{"password":"candidate""#,
        ] {
            let request = Request::builder()
                .method("POST")
                .uri("/unlock")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap();
            let response = router(state.clone()).oneshot(request).await.unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
            assert_eq!(body["error"], "invalid");
            assert_eq!(body["message"], "invalid unlock request");
            assert!(!body.to_string().contains("candidate"));
        }

        let request = Request::builder()
            .method("POST")
            .uri("/unlock")
            .header("content-type", "application/json")
            .body(Body::from(vec![b'x'; 2 * 1024 * 1024 + 1]))
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"], "invalid");
        assert_eq!(body["message"], "invalid unlock request");
    }

    #[tokio::test]
    async fn blank_nickname_is_invalid_before_any_invocation() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 'should never run' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/identity/default",
            Some(json!({"nickname": "   "})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid");
    }

    // ---- identity create: first-identity Pending->Salt promotion ----------

    #[tokio::test]
    async fn first_identity_create_promotes_pending_password_to_salt_account() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::create_dir_all(home.join(".hyphae"))
            .await
            .unwrap();
        // The fake binary writes keystore.json itself, the same way a real
        // `identity create` on a fresh HOME would, so `read_keystore_salt`
        // has something to find afterwards.
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\necho '{{\"salt\":\"c2FsdA==\"}}' > \"{}\"\n\
             echo '{{\"ok\":true,\"data\":{{\"nickname\":\"alice\"}}}}'\n",
            home.join(".hyphae").join("keystore.json").display()
        );
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", &script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let store = Arc::new(MemoryPasswordStore::new());
        let state = CommState::ready(runner, store.clone(), home);

        let (status, body) = call(
            router(state),
            "POST",
            "/identity",
            Some(json!({"nickname": "alice", "default": true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");

        let salt_account = Account::from_salt("c2FsdA==");
        assert!(
            store.get(&salt_account).await.is_ok(),
            "the generated password must have been renamed onto the Salt account"
        );
    }

    // ---- identity create: a failure AFTER the keystore write must never --
    // ---- delete the only copy of its password (PR #622 High) -------------

    #[tokio::test]
    async fn first_identity_create_timeout_after_keystore_write_preserves_the_password() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::create_dir_all(home.join(".hyphae"))
            .await
            .unwrap();
        // Writes keystore.json before reading stdin, then sleeps. A test-only
        // runner barrier below pauses *after spawn but before the timeout is
        // armed*, so the observer can prove this write has actually landed
        // before allowing the real timeout path to proceed. That ordering is
        // the fact under test; relying on the shell child merely being
        // scheduled within a few seconds flakes under a parallel full suite.
        let script = format!(
            "#!/bin/sh\necho '{{\"salt\":\"c2FsdA==\"}}' > \"{}\"\ncat > /dev/null\nsleep 30\n",
            home.join(".hyphae").join("keystore.json").display()
        );
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", &script).await;
        let runner = Arc::new(HyphaeRunner::new(
            bin,
            home.clone(),
            Duration::from_millis(100),
        ));
        let (spawned_before_timeout, arm_timeout) =
            crate::runner::timeout_test_hook::install(runner.bin_path());
        let store = Arc::new(MemoryPasswordStore::new());
        let state = CommState::ready(runner, store.clone(), home.clone());
        let keystore = home.join(".hyphae").join("keystore.json");

        let request = call(
            router(state),
            "POST",
            "/identity",
            Some(json!({"nickname": "alice"})),
        );
        let prove_write_then_arm_timeout = async {
            spawned_before_timeout.notified().await;
            tokio::time::timeout(Duration::from_secs(30), async {
                while !keystore.exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("fixture child never wrote keystore.json");
            arm_timeout.notify_one();
        };
        let ((status, body), ()) = tokio::join!(request, prove_write_then_arm_timeout);
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
        assert_eq!(body["data"]["password_preserved"], true, "{body:?}");

        let salt_account = Account::from_salt("c2FsdA==");
        assert!(
            store.get(&salt_account).await.is_ok(),
            "keystore.json was already written before the timeout; its password must have \
             been promoted onto the Salt account, not deleted"
        );
    }

    #[tokio::test]
    async fn first_identity_create_failure_after_keystore_write_preserves_the_password() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::create_dir_all(home.join(".hyphae"))
            .await
            .unwrap();
        // Writes keystore.json, THEN reports a failure envelope on a
        // non-zero exit — reproducing Hyphae itself failing a LATER step
        // (e.g. `--default`'s SetDefault) after its own keystore write has
        // already landed.
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\necho '{{\"salt\":\"c2FsdA==\"}}' > \"{}\"\n\
             echo '{{\"ok\":false,\"error\":\"other_error\",\"message\":\"set default failed \
             after write\"}}' >&2\nexit 4\n",
            home.join(".hyphae").join("keystore.json").display()
        );
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", &script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let store = Arc::new(MemoryPasswordStore::new());
        let state = CommState::ready(runner, store.clone(), home);

        let (status, body) = call(
            router(state),
            "POST",
            "/identity",
            Some(json!({"nickname": "alice", "default": true})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
        assert_eq!(body["data"]["password_preserved"], true, "{body:?}");

        let salt_account = Account::from_salt("c2FsdA==");
        assert!(
            store.get(&salt_account).await.is_ok(),
            "keystore.json was already written before the reported failure; its password \
             must have been promoted onto the Salt account, not deleted"
        );
    }

    // ---- identity create: KeystoreWriteLock must cover the Pending->Salt --
    // ---- rename, not just the Hyphae spawn (PR #622 Medium) ---------------

    /// Stands in for `KeyringPasswordStore`'s real `spawn_blocking` keychain
    /// I/O (every op there runs on the blocking pool, `password_store.rs`'s
    /// `run_blocking`) with an artificial, deterministic delay — long
    /// enough that, without the Medium fix (holding `KeystoreWriteLock`
    /// through the Pending->Salt `rename`, not just the Hyphae spawn), a
    /// second identity-create request that was parked on the lock wakes up
    /// right after the first drops its guard, reads the just-written salt,
    /// and looks its password up on the `Salt` account before the first
    /// request's own `rename` has finished writing it there.
    struct DelayedPasswordStore {
        inner: MemoryPasswordStore,
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl PasswordStore for DelayedPasswordStore {
        async fn get(&self, account: &Account) -> Result<Password, StoreError> {
            tokio::time::sleep(self.delay).await;
            self.inner.get(account).await
        }

        async fn put(&self, account: &Account, password: &Password) -> Result<(), StoreError> {
            tokio::time::sleep(self.delay).await;
            self.inner.put(account, password).await
        }

        async fn delete(&self, account: &Account) -> Result<(), StoreError> {
            tokio::time::sleep(self.delay).await;
            self.inner.delete(account).await
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn keystore_write_lock_covers_the_pending_to_salt_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hyphae-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::create_dir_all(home.join(".hyphae"))
            .await
            .unwrap();
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\necho '{{\"salt\":\"c2FsdA==\"}}' > \"{}\"\n\
             echo '{{\"ok\":true,\"data\":{{\"nickname\":\"ignored\"}}}}'\n",
            home.join(".hyphae").join("keystore.json").display()
        );
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", &script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let delayed = Arc::new(DelayedPasswordStore {
            inner: MemoryPasswordStore::new(),
            delay: Duration::from_millis(150),
        });
        let state = CommState::ready(runner, delayed.clone(), home);
        let app = router(state);

        // Both requests are issued at once: whichever loses the race to
        // `KeystoreWriteLock` is parked on it until the winner's guard
        // drops. Without the fix, the winner drops its guard BEFORE its own
        // `rename` finishes, so the loser wakes up, sees the keystore
        // (already written), and looks its password up on the `Salt`
        // account before `rename`'s `put` has landed there.
        let first = {
            let app = app.clone();
            tokio::spawn(async move {
                call(app, "POST", "/identity", Some(json!({"nickname": "alice"}))).await
            })
        };
        let second = {
            let app = app.clone();
            tokio::spawn(async move {
                call(app, "POST", "/identity", Some(json!({"nickname": "bob"}))).await
            })
        };
        let (status_a, body_a) = first.await.unwrap();
        let (status_b, body_b) = second.await.unwrap();
        assert_eq!(status_a, StatusCode::OK, "{body_a:?}");
        assert_eq!(status_b, StatusCode::OK, "{body_b:?}");

        let keys = delayed.inner.snapshot_keys().await;
        assert_eq!(
            keys.len(),
            1,
            "expected exactly one surviving account (the promoted Salt account), found {keys:?}"
        );
        assert!(
            !keys[0].starts_with("pending-"),
            "no Pending account should survive: {keys:?}"
        );
    }

    // ---- import (COMM-2b): HTTP-level status/error mapping ---------------

    #[tokio::test]
    async fn import_without_confirm_is_bad_request_before_any_invocation() {
        let tmp = tempfile::tempdir().unwrap();
        // Would fail the test if ever invoked.
        let script = "#!/bin/sh\necho 'should never run' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/import",
            Some(json!({"from": "/nonexistent", "confirm": false})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(body["error"], "confirm_required");
    }

    #[tokio::test]
    async fn import_rejects_a_missing_source_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 'should never run' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let missing = tmp.path().join("does-not-exist");
        let (status, body) = call(
            router(state),
            "POST",
            "/import",
            Some(json!({
                "from": missing.to_string_lossy(),
                "confirm": true,
                "dry_run": true,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(body["error"], "invalid");
    }

    // ---- COMM-3: send / history / inbox / outbox --------------------------

    /// A [`CommState::ready`] whose `home` already has an encrypted
    /// keystore (just the `salt` field — enough for `resolve_account`) and
    /// whose password store already has that account's password filed —
    /// standing in for "an identity was already created", without actually
    /// running `identity create` through the fake binary.
    async fn ready_state_with_identity(dir: &Path, script: &str) -> CommState {
        const SALT: &str = "dGVzdC1zYWx0";
        let home = dir.join("hyphae-home");
        tokio::fs::create_dir_all(home.join(".hyphae"))
            .await
            .unwrap();
        tokio::fs::write(
            home.join(".hyphae").join("keystore.json"),
            format!("{{\"salt\":\"{SALT}\"}}"),
        )
        .await
        .unwrap();
        let bin = install_fixture(dir, "hyphae-fake.sh", script).await;
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
        let store = Arc::new(MemoryPasswordStore::new());
        store
            .put(
                &Account::from_salt(SALT),
                &Password::new(b"test-password".to_vec()).unwrap(),
            )
            .await
            .unwrap();
        CommState::ready(runner, store, home)
    }

    /// §8's COMM-3 row: "relay 不可达时 send 返回 ok，层级为 L1，
    /// `published_to==0`". `from` is given explicitly so this test does not
    /// also depend on the default-identity-resolution path.
    #[tokio::test]
    async fn send_with_relay_unreachable_is_ok_layer_l1() {
        let tmp = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
cat > /dev/null
case "$*" in
  "relay list") echo '{"ok":true,"data":{"relays":["wss://r"],"source":"config"}}' ;;
  "agent msg --from alice --to npub1bob --content hi --password-stdin") echo '{"ok":true,"data":{"published_to":0,"queued_for_retry":true,"event_id":"e1","history_stored":true}}' ;;
  *) echo "{\"ok\":false,\"error\":\"other_error\",\"message\":\"unexpected: $*\"}" >&2; exit 4 ;;
esac
"#;
        let state = ready_state_with_identity(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/send",
            Some(json!({"to": "npub1bob", "content": "hi", "from": "alice"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["published_to"], 0);
        assert_eq!(body["data"]["layer"], "L1");
    }

    /// §8's COMM-3 row: send returns `not_configured` when no relay is
    /// configured, rather than silently using Hyphae's own built-in default
    /// relay (§9 R1).
    #[tokio::test]
    async fn send_without_configured_relay_is_not_configured() {
        let tmp = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
cat > /dev/null
echo '{"ok":true,"data":{"relays":[],"source":"default"}}'
"#;
        let state = ready_state_with_identity(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/send",
            Some(json!({"to": "npub1bob", "content": "hi", "from": "alice"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body:?}");
        assert_eq!(body["error"], "not_configured");
    }

    /// §5.3 / §8: `published_to>0` with `history_stored:false` must surface
    /// as `partial` (with `data` kept intact, event_id included) — not as a
    /// plain success.
    #[tokio::test]
    async fn send_with_published_but_unstored_history_is_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
cat > /dev/null
case "$*" in
  "relay list") echo '{"ok":true,"data":{"relays":["wss://r"],"source":"config"}}' ;;
  *) echo '{"ok":true,"data":{"published_to":1,"history_stored":false,"event_id":"e9"}}' ;;
esac
"#;
        let state = ready_state_with_identity(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/send",
            Some(json!({"to": "npub1bob", "content": "hi", "from": "alice"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
        assert_eq!(body["error"], "partial");
        assert_eq!(body["data"]["event_id"], "e9");
    }

    /// §8: "retry 返回的 event_id 与原 event_id 相等" — even if the fake
    /// binary's own retry response omitted `event_id` entirely (as the real
    /// Hyphae binary does in practice, per JOINT-ROUND1 §2 step 4), the
    /// router must still hand it back.
    #[tokio::test]
    async fn outbox_retry_echoes_the_original_event_id() {
        let tmp = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
cat > /dev/null
case "$*" in
  "storage outbox list") echo '{"ok":true,"data":[{"id":"e2","status":"pending"}]}' ;;
  "relay list") echo '{"ok":true,"data":{"relays":["wss://r"],"source":"config"}}' ;;
  "storage outbox retry --id e2") echo '{"ok":true,"data":{"attempted":true,"sent":true,"queued":false,"marked_failed":false}}' ;;
  *) echo "{\"ok\":false,\"error\":\"other_error\",\"message\":\"unexpected: $*\"}" >&2; exit 4 ;;
esac
"#;
        let state = ready_state_with_identity(tmp.path(), script).await;
        let (status, body) = call(router(state), "POST", "/outbox/e2/retry", None).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["data"]["event_id"], "e2");
        assert_eq!(body["data"]["sent"], true);
    }

    /// Codex 挑战（B 端 gpt-6-astra）Medium：并发 `outbox clear`/daemon 把条目
    /// 移除时，Hyphae 的 `storage outbox retry` 返回 `write_conflict` 且带
    /// `data`（`{"sent":true,"superseded":true}` 这种「已经拿到 ACK，但条目
    /// 被并发移除」的情形，见 `internal/messaging/outbox_commands.go`）。路由
    /// 必须把这份 data 原样带回去，而不是只剩一句 message——`with_event_id`
    /// 补写的 event_id 也要留在里面。
    #[tokio::test]
    async fn outbox_retry_write_conflict_keeps_data_and_injected_event_id() {
        let tmp = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
cat > /dev/null
case "$*" in
  "storage outbox list") echo '{"ok":true,"data":[{"id":"e2","relays":["wss://r"]}]}' ;;
  "storage outbox retry --id e2") echo '{"ok":false,"error":"write_conflict","message":"queue entry superseded","data":{"sent":true,"superseded":true}}' >&2; exit 1 ;;
  *) echo "{\"ok\":false,\"error\":\"other_error\",\"message\":\"unexpected: $*\"}" >&2; exit 4 ;;
esac
"#;
        let state = ready_state_with_identity(tmp.path(), script).await;
        let (status, body) = call(router(state), "POST", "/outbox/e2/retry", None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body:?}");
        assert_eq!(body["error"], "conflict");
        assert_eq!(body["data"]["sent"], true, "{body:?}");
        assert_eq!(body["data"]["superseded"], true, "{body:?}");
        assert_eq!(
            body["data"]["event_id"], "e2",
            "with_event_id's injected id must survive into conflict's data: {body:?}"
        );
    }

    /// §4/§8: retrying an id absent from `storage outbox list` is
    /// `not_found`, checked before ever invoking `outbox retry`.
    #[tokio::test]
    async fn outbox_retry_of_an_unknown_id_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
cat > /dev/null
case "$*" in
  "storage outbox list") echo '{"ok":true,"data":[]}' ;;
  *) echo "{\"ok\":false,\"error\":\"other_error\",\"message\":\"should never run: $*\"}" >&2; exit 4 ;;
esac
"#;
        let state = ready_state_with_identity(tmp.path(), script).await;
        let (status, body) = call(router(state), "POST", "/outbox/nope/retry", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body:?}");
        assert_eq!(body["error"], "not_found");
    }

    /// §4/§8: `outbox clear` without `confirm:true` is rejected before the
    /// fake binary is ever invoked (a script that would fail the test if run).
    #[tokio::test]
    async fn outbox_clear_without_confirm_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 'should never run' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(router(state), "POST", "/outbox/clear", Some(json!({}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(body["error"], "confirm_required");
    }

    #[tokio::test]
    async fn outbox_clear_with_confirm_invokes_hyphae() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\ncat > /dev/null\necho '{\"ok\":true,\"data\":{\"cleared\":2}}'\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/outbox/clear",
            Some(json!({"confirm": true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["data"]["cleared"], 2);
    }

    /// §5.3: "任何情形都不提供「重发」" — there is no route anywhere in this
    /// router that would mint a brand-new event_id for an already-sent
    /// message. `POST /comm/send` itself is the only message-creating route
    /// and takes no event_id, so "no resend" means no OTHER route accepts
    /// one for that purpose either.
    #[tokio::test]
    async fn there_is_no_resend_route() {
        let app = router(CommState::unconfigured("x"));
        for (method, path) in [
            ("POST", "/send/e1/resend"),
            ("POST", "/outbox/e1/resend"),
            ("POST", "/resend"),
            ("PUT", "/send"),
        ] {
            let (status, _) = call(app.clone(), method, path, None).await;
            assert!(
                status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path} unexpectedly matched a route: {status}"
            );
        }
    }

    /// Codex 挑战（B 端 gpt-6-astra）High：Hyphae 真实返回的 `audit_error` 是
    /// 非空字符串（`internal/messaging/agent.go` 的 `AuditError string
    /// \`json:"audit_error,omitempty"\`` ），不是布尔值；审计失败时 Hyphae 的
    /// 退出码仍可能是 0（只在非 json 模式打一行 stderr warning，`sendErr`
    /// 不受影响）。字符串形态的 `audit_error` 必须被 `classify_send_result`
    /// 识别为「本地记账异常」，走 §5.3 的 `partial`，而不是被静默当成
    /// `Some(true)`（布尔读取下恒为 `None`）比较失败从而放行成功。
    #[tokio::test]
    async fn send_with_string_audit_error_is_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
cat > /dev/null
case "$*" in
  "relay list") echo '{"ok":true,"data":{"relays":["wss://r"],"source":"config"}}' ;;
  *) echo '{"ok":true,"data":{"published_to":1,"history_stored":true,"audit_error":"permission denied","event_id":"e7"}}' ;;
esac
"#;
        let state = ready_state_with_identity(tmp.path(), script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/send",
            Some(json!({"to": "npub1bob", "content": "hi", "from": "alice"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
        assert_eq!(body["error"], "partial");
        assert_eq!(body["data"]["audit_error"], "permission denied");
        assert_eq!(body["data"]["event_id"], "e7");
    }

    /// Codex 挑战（B 端 gpt-6-astra）Medium：retry 的超时要按「这次实际会用
    /// 哪些 relay」算，而真实 Hyphae 的 `storage outbox retry`
    /// （`internal/messaging/outbox_commands.go`）优先用 outbox 条目自己入队
    /// 时保存的 `relays`，只在条目没有自己的 relay 时才落回当前 `relay
    /// list`。配置变更之后重试（条目保存的 relay 数与当前配置不同）必须按
    /// 条目自己的数量算超时，不能看当前配置。
    #[tokio::test]
    async fn retry_relay_count_prefers_the_entrys_own_relays_over_current_config() {
        let tmp = tempfile::tempdir().unwrap();
        // Current config has drifted to a single relay — if the fix didn't
        // land, this is what gets used for every retry regardless of what
        // the entry itself queued against.
        let script = r#"#!/bin/sh
cat > /dev/null
echo '{"ok":true,"data":{"relays":["wss://only-now"],"source":"config"}}'
"#;
        let state = ready_state_with_script(tmp.path(), script).await;
        let (runner, ..) = state.require_ready().unwrap();
        let entry = json!({"id": "e1", "relays": ["wss://a", "wss://b", "wss://c"]});
        assert_eq!(
            relay_count_for_retry(runner, &entry).await,
            3,
            "must use the entry's own 3 queued relays, not the current config's 1"
        );
    }

    /// Same helper, the fallback half: an entry with no relays of its own
    /// (e.g. a legacy entry, or `ResolveRelays` having filled nothing in)
    /// falls back to whatever is currently configured, same as before this
    /// fix for every other route.
    #[tokio::test]
    async fn retry_relay_count_falls_back_to_current_config_when_entry_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
cat > /dev/null
echo '{"ok":true,"data":{"relays":["wss://a","wss://b"],"source":"config"}}'
"#;
        let state = ready_state_with_script(tmp.path(), script).await;
        let (runner, ..) = state.require_ready().unwrap();
        let entry = json!({"id": "e1", "relays": []});
        assert_eq!(relay_count_for_retry(runner, &entry).await, 2);
    }

    #[tokio::test]
    async fn history_rejects_an_out_of_range_limit() {
        let tmp = tempfile::tempdir().unwrap();
        // `require_ready` succeeds (so the handler reaches the limit check)
        // but the script would fail the test if the limit check didn't
        // short-circuit before any invocation.
        let script = "#!/bin/sh\necho 'should never run' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(router(state), "GET", "/history?limit=0&as=alice", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(body["error"], "invalid");
    }

    /// Codex 挑战（B 端 gpt-6-astra）Medium：`?limit=abc` 这种连类型都不对的
    /// 查询参数，在 Axum 的 `Query<T>` extractor 阶段就失败了，比
    /// `get_history` 函数体里 `1..=200` 的范围检查还早——这个失败必须也走
    /// §4 统一的 `{ok:false,error:"invalid",...}` JSON envelope，不能是
    /// Axum 默认的纯文本 400。
    #[tokio::test]
    async fn history_with_non_numeric_limit_is_invalid_json_not_plain_text() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 'should never run' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(router(state), "GET", "/history?limit=abc", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(body["error"], "invalid", "{body:?}");
        assert_eq!(body["ok"], false, "{body:?}");
    }

    /// Same bug, `limit=-1`: still a `QueryRejection` (`u32` cannot hold a
    /// negative number) before `get_history`'s own `1..=200` range check
    /// ever runs.
    #[tokio::test]
    async fn history_with_negative_limit_is_invalid_json_not_plain_text() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 'should never run' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(router(state), "GET", "/history?limit=-1", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(body["error"], "invalid", "{body:?}");
    }

    /// Same bug, `list_outbox`'s `?failed_only=abc` (`bool` extractor
    /// failure).
    #[tokio::test]
    async fn outbox_list_with_non_bool_failed_only_is_invalid_json_not_plain_text() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 'should never run' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(router(state), "GET", "/outbox?failed_only=abc", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(body["error"], "invalid", "{body:?}");
        assert_eq!(body["ok"], false, "{body:?}");
    }

    // ---- COMM-4b: relay probe only folds `network_error` into ------------
    // ---- `connected:false`; every other failure still errors (T2) -------

    /// pre-pr-check T2: without the third match arm in `probe_relay`
    /// (`Envelope::Failed { error, message, data, .. } => return
    /// Err(...)`), this test goes red — a non-network failure (here
    /// `other_error`, Hyphae's own exit 4) would otherwise be silently
    /// folded into `{connected:false}` the same as a real network-down
    /// probe, hiding a different failure mode (bad `--timeout`, a crashed
    /// Hyphae, …) behind the "relay is down" result. S3: this is checked at
    /// the one and only place the invocation's result is interpreted — the
    /// match in `probe_relay` itself — not at an earlier entry/validation
    /// step that a later code path could bypass.
    #[tokio::test]
    async fn relay_probe_of_a_non_network_failure_is_still_an_http_error() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho '{\"ok\":false,\"error\":\"other_error\",\"message\":\"boom\"}' >&2\nexit 4\n";
        let state = ready_state_with_script(tmp.path(), script).await;
        let (status, body) = call(router(state), "POST", "/relay/probe", Some(json!({}))).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
        assert_eq!(body["error"], "upstream", "{body:?}");
        assert!(
            body.get("data").is_none() || body["data"].get("connected").is_none(),
            "a non-network failure must never report connected:false: {body:?}"
        );
    }

    // ---- COMM-4b review round 2, Low: `{"url": ""}` == no `url` at all ---

    /// Codex COMM-4b review, round 2, Low: an explicit-but-empty `url`
    /// (`{"url": ""}`) must resolve the configured default, exactly like
    /// omitting `url` entirely — matching Hyphae's own `relayInfo`, which
    /// treats an empty CLI arg as "no override" (`internal/nostr/relay.go:70`,
    /// locked source). Before the fix, `effective_probe_target` matched
    /// `Some("")` as if it were a real target, so this never called `relay
    /// list` at all and sent `relay info ""` straight through — proven here
    /// by asserting on the exact argv each invocation of the fake binary
    /// received, not just the final JSON response.
    #[tokio::test]
    async fn relay_probe_with_an_explicit_empty_url_resolves_the_default_like_hyphae_does() {
        let tmp = tempfile::tempdir().unwrap();
        let argv_log = tmp.path().join("argv.log");
        let script = format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\" >> \"{log}\"; done\n\
             printf -- '---\\n' >> \"{log}\"\n\
             case \"$1 $2\" in\n\
             'relay list')\n\
             echo '{{\"ok\":true,\"data\":{{\"source\":\"config\",\"relays\":[\"wss://relay.example\"]}}}}'\n\
             ;;\n\
             'relay info')\n\
             echo '{{\"ok\":true,\"data\":{{\"url\":\"wss://relay.example\",\"connected\":true}}}}'\n\
             ;;\n\
             esac\n",
            log = argv_log.display(),
        );
        let state = ready_state_with_script(tmp.path(), &script).await;
        let (status, body) = call(
            router(state),
            "POST",
            "/relay/probe",
            Some(json!({"url": ""})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["data"]["url"], "wss://relay.example", "{body:?}");

        let log = tokio::fs::read_to_string(&argv_log).await.unwrap();
        let invocations: Vec<Vec<&str>> = log
            .split("---\n")
            .map(|chunk| chunk.lines().collect::<Vec<_>>())
            .filter(|args| !args.is_empty())
            .collect();
        assert!(
            invocations
                .iter()
                .any(|args| args.as_slice() == ["relay", "list"]),
            "an empty `url` must still trigger the `relay list` default-resolution \
             fallback (the pre-fix short-circuit skipped this entirely): {invocations:?}"
        );
        assert!(
            invocations.iter().any(|args| args.as_slice()
                == ["relay", "info", "wss://relay.example", "--timeout", "5"]),
            "the resolved default address must be passed explicitly to `relay info`: \
             {invocations:?}"
        );
        assert!(
            !invocations.iter().any(|args| {
                args.first() == Some(&"relay")
                    && args.get(1) == Some(&"info")
                    && args.get(2) == Some(&"")
            }),
            "`relay info` must never receive a literal empty-string url argument: \
             {invocations:?}"
        );
    }
}
