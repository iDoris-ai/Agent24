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
///   A failed create deletes the `Pending` entry.
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
            if is_first_identity {
                match read_keystore_salt(home).await {
                    Ok(Some(salt)) => {
                        let salt_account = Account::from_salt(&salt);
                        drop(guard);
                        if let Err(e) = password_store.rename(&account, &salt_account).await {
                            tracing::warn!(
                                "identity create succeeded but promoting the pending comm \
                                 keystore password failed ({e}); it stays under the pending \
                                 account until the next successful create or restart"
                            );
                        }
                    }
                    Ok(None) => {
                        drop(guard);
                        tracing::warn!(
                            "identity create succeeded but keystore.json has no salt field; \
                             leaving the pending password entry in place"
                        );
                    }
                    Err(e) => {
                        drop(guard);
                        tracing::warn!("could not read the new keystore's salt: {e}");
                    }
                }
            } else {
                drop(guard);
            }
            Ok(Json(json!({"ok": true, "data": data})))
        }
        Ok(Envelope::Failed {
            error,
            message,
            data,
            ..
        }) => {
            drop(guard);
            if is_first_identity {
                let _ = password_store.delete(&account).await;
            }
            Err(map_envelope_failure(&error, &message, data))
        }
        Err(e) => {
            drop(guard);
            if is_first_identity {
                let _ = password_store.delete(&account).await;
            }
            Err(map_runner_error(e))
        }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::binary::{VerifiedBinary, sha256_of};
    use crate::password_store::MemoryPasswordStore;

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
}
