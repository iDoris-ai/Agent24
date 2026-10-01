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
use axum::extract::State;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{CommError, map_envelope_failure, map_runner_error, map_store_error};
use crate::npub::is_valid_npub;
use crate::password::Password;
use crate::password_store::{Account, PasswordStore};
use crate::runner::{Envelope, HyphaeRunner, Invocation};

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
        }
    }

    /// Every route answers `not_configured`, message `reason` — no binary
    /// was ever named (`A24_HYPHAE_BIN` unset, no sibling `hyphae` found).
    #[must_use]
    pub fn unconfigured(reason: impl Into<String>) -> Self {
        Self {
            backend: Backend::Unconfigured {
                reason: Arc::new(reason.into()),
            },
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
        }
    }

    fn require_ready(&self) -> Result<(&HyphaeRunner, &dyn PasswordStore, &Path), CommError> {
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
        .route("/contact", get(list_contact).post(add_contact))
        .route("/relay", get(list_relay).put(set_relay))
        .route("/relay/probe", post(probe_relay))
        .route("/import", post(import))
        .with_state(state)
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

async fn resolve_account(home: &Path) -> Result<Option<Account>, CommError> {
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
    invalid_if_blank("nickname", &req.nickname)?;

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
    invalid_if_blank("nickname", &req.nickname)?;
    let envelope = runner
        .run_keystore_write(read_invocation(&[
            "identity",
            "use",
            "--nickname",
            &req.nickname,
        ]))
        .await
        .map_err(map_runner_error)?;
    envelope_response(envelope)
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
    invalid_if_blank("nickname", &req.nickname)?;
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
    relay_envelope_response(envelope)
}

#[derive(Deserialize, Default)]
struct RelayProbeReq {
    url: Option<String>,
}

/// `POST /comm/relay/probe` → `relay info [U] --timeout 5`, run only when a
/// caller explicitly asks (§4, §5.2: comm does not probe periodically —
/// that was cut, M12). Always expects a JSON body (`{}` when no `url`).
async fn probe_relay(State(state): State<CommState>, Json(req): Json<RelayProbeReq>) -> CommResult {
    let (runner, ..) = state.require_ready()?;
    let mut argv: Vec<OsString> = vec!["relay".into(), "info".into()];
    if let Some(url) = &req.url {
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
    envelope_response(envelope)
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
        // Writes keystore.json immediately (Hyphae's own atomic rename has
        // already landed), THEN sleeps well past the runner's timeout —
        // reproducing `RunnerError::Timeout` AFTER the write, exactly the
        // ordinary operational event (a slow child, `kill_process_group`
        // landing late) the High finding describes, not an exotic attack.
        // The keystore write happens BEFORE the process ever reads stdin, so
        // it lands as early as possible after spawn — the test's own
        // timeout budget must only cover process startup, not stdin
        // round-trip latency.
        let script = format!(
            "#!/bin/sh\necho '{{\"salt\":\"c2FsdA==\"}}' > \"{}\"\ncat > /dev/null\nsleep 30\n",
            home.join(".hyphae").join("keystore.json").display()
        );
        let bin = install_fixture(tmp.path(), "hyphae-fake.sh", &script).await;
        // A generous timeout (this test only waits for the KILL, not the
        // sleep): under a loaded `cargo test --workspace` run, several
        // seconds of scheduler contention for the fork/exec itself is not
        // unusual, and the point under test is "a timeout AFTER the write
        // lands", not how fast that happens.
        let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(3)));
        let store = Arc::new(MemoryPasswordStore::new());
        let state = CommState::ready(runner, store.clone(), home);

        let (status, body) = call(
            router(state),
            "POST",
            "/identity",
            Some(json!({"nickname": "alice"})),
        )
        .await;
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
}
