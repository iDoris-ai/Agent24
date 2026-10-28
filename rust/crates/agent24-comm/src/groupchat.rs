//! COMM-G1: `/comm/groupchat/*` routes over Hyphae's `groupchat` commands
//! (contract: Hyphae `docs/agent/group-chat-agent24-interface-20261025.md`,
//! pinned by its `TestCommandJSONContract`).
//!
//! Mapping (nothing here is CLI- or UI-specific):
//!
//! | route                                          | hyphae                       |
//! | ---------------------------------------------- | ---------------------------- |
//! | `GET  /groupchat`                              | `groupchat list`             |
//! | `POST /groupchat`                              | `groupchat create`           |
//! | `GET  /groupchat/{g}/members`                  | `groupchat members`          |
//! | `GET  /groupchat/{g}/invites`                  | `groupchat invite list`      |
//! | `GET  /groupchat/{g}/messages`                 | `groupchat history`          |
//! | `POST /groupchat/{g}/messages`                 | `groupchat send`             |
//! | `GET  /groupchat/{g}/messages/{l}`             | `groupchat status`           |
//! | `POST /groupchat/{g}/messages/{l}/retry`       | `groupchat retry`            |
//! | `POST /groupchat/invites/{i}/accept`           | `groupchat accept`           |
//!
//! There is no `activate` route: Hyphae has no such command (activation is
//! automatic once the last invitee accepts, and surfaces as `accept`'s
//! `state:"active"` or a later `list`). Likewise `invite` is read-only
//! (`invite list`); invitations are made by `create`.
//!
//! Every command needs the keystore password (`--password-stdin`), fetched
//! from the same [`PasswordStore`](crate::PasswordStore) as `send`. Failure
//! mapping is the shared closed set (`crate::error`): exit 1 `user_error`
//! (bad history cursor, both cursors; since Hyphae v0.27.2 also group not
//! active, unknown logical id, invite/identity not found) → 400 `invalid`;
//! exit 4 with a send
//! report in `data` (partial fan-out) → 502 `partial` carrying that report
//! verbatim, so a UI can render per-recipient state; other exit 4 → 502
//! `upstream`. Mapping keys on the envelope error name, not the exit number,
//! so 0.27.1 (`other_error`/4 → 502) and 0.27.2 (`user_error`/1 → 400) both
//! classify correctly. Malformed ids and npubs are still rejected locally
//! first as 400 `invalid`.

use std::ffi::OsString;

use axum::Json;
use axum::Router;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{CommError, map_envelope_failure, map_runner_error, map_store_error};
use crate::npub::is_valid_npub;
use crate::router::{CommState, resolve_account};
use crate::runner::{Envelope, Invocation};

type CommResult = Result<Json<Value>, CommError>;

/// Largest `--body` accepted. Hyphae passes it through argv, which the OS
/// caps well below the runner's output cap.
const MAX_BODY_BYTES: usize = 32 * 1024;
const MAX_INVITEES: usize = 100;

pub(crate) fn routes() -> Router<CommState> {
    Router::new()
        .route("/groupchat", get(list_groups).post(create_group))
        .route("/groupchat/{group_id}/members", get(members))
        .route("/groupchat/{group_id}/invites", get(invites))
        .route(
            "/groupchat/{group_id}/messages",
            get(history).post(send_message),
        )
        .route("/groupchat/{group_id}/messages/{logical_id}", get(status))
        .route(
            "/groupchat/{group_id}/messages/{logical_id}/retry",
            post(retry),
        )
        .route("/groupchat/invites/{invite_id}/accept", post(accept))
}

// ---------------------------------------------------------------------
// validation + argv (pure, unit-tested)
// ---------------------------------------------------------------------

/// 32 lowercase hex chars: group ids, logical (message) ids, invite ids.
fn hex32(field: &'static str, value: &str) -> Result<(), CommError> {
    if value.len() == 32
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(CommError::Invalid(format!(
            "{field} must be 32 lowercase hex characters"
        )))
    }
}

fn text(field: &'static str, value: &str, max: usize) -> Result<(), CommError> {
    if value.trim().is_empty() {
        Err(CommError::Invalid(format!("{field} must not be empty")))
    } else if value.len() > max || value.contains('\0') {
        Err(CommError::Invalid(format!(
            "{field} is too long or contains a NUL byte"
        )))
    } else {
        Ok(())
    }
}

fn npub(field: &'static str, value: &str) -> Result<(), CommError> {
    if is_valid_npub(value) {
        Ok(())
    } else {
        Err(CommError::Invalid(format!("{field} is not a valid npub")))
    }
}

/// Builds `groupchat <sub…> --flag value…`; `--identity` is always optional
/// (Hyphae falls back to the default identity when it is absent).
struct Argv(Vec<OsString>);

impl Argv {
    fn new(sub: &[&str]) -> Self {
        let mut v: Vec<OsString> = vec!["groupchat".into()];
        v.extend(sub.iter().map(OsString::from));
        Self(v)
    }

    fn flag(mut self, name: &str, value: &str) -> Self {
        self.0.push(name.into());
        self.0.push(value.into());
        self
    }

    fn opt(self, name: &str, value: Option<&str>) -> Self {
        match value {
            Some(v) => self.flag(name, v),
            None => self,
        }
    }

    fn identity(self, identity: Option<&str>) -> Result<Self, CommError> {
        if let Some(i) = identity {
            text("identity", i, 128)?;
        }
        Ok(self.opt("--identity", identity))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    name: String,
    #[serde(default)]
    invitees: Vec<String>,
    identity: Option<String>,
}

fn create_argv(req: &CreateReq) -> Result<Vec<OsString>, CommError> {
    text("name", &req.name, 256)?;
    if req.invitees.is_empty() {
        return Err(CommError::Invalid(
            "invitees must name at least one npub".to_owned(),
        ));
    }
    if req.invitees.len() > MAX_INVITEES {
        return Err(CommError::Invalid(format!(
            "at most {MAX_INVITEES} invitees"
        )));
    }
    let mut argv = Argv::new(&["create"]).flag("--name", &req.name);
    for invitee in &req.invitees {
        npub("invitees[]", invitee)?;
        argv = argv.flag("--invitee", invitee);
    }
    Ok(argv.identity(req.identity.as_deref())?.0)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendReq {
    body: String,
    logical_id: Option<String>,
    max_retries: Option<u32>,
    identity: Option<String>,
}

fn send_argv(group_id: &str, req: &SendReq) -> Result<Vec<OsString>, CommError> {
    hex32("group_id", group_id)?;
    text("body", &req.body, MAX_BODY_BYTES)?;
    if let Some(l) = &req.logical_id {
        hex32("logical_id", l)?;
    }
    let retries = req.max_retries.map(|n| n.to_string());
    Ok(Argv::new(&["send"])
        .flag("--group-id", group_id)
        .flag("--body", &req.body)
        .opt("--logical-id", req.logical_id.as_deref())
        .opt("--max-retries", retries.as_deref())
        .identity(req.identity.as_deref())?
        .0)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryReq {
    recipient: Option<String>,
    identity: Option<String>,
}

fn retry_argv(
    group_id: &str,
    logical_id: &str,
    req: &RetryReq,
) -> Result<Vec<OsString>, CommError> {
    hex32("group_id", group_id)?;
    hex32("logical_id", logical_id)?;
    if let Some(r) = &req.recipient {
        npub("recipient", r)?;
    }
    Ok(Argv::new(&["retry"])
        .flag("--group-id", group_id)
        .flag("--logical-id", logical_id)
        .opt("--recipient", req.recipient.as_deref())
        .identity(req.identity.as_deref())?
        .0)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptReq {
    identity: Option<String>,
}

fn accept_argv(invite_id: &str, req: &AcceptReq) -> Result<Vec<OsString>, CommError> {
    hex32("invite_id", invite_id)?;
    Ok(Argv::new(&["accept"])
        .flag("--invite", invite_id)
        .identity(req.identity.as_deref())?
        .0)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct IdentityQuery {
    identity: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    identity: Option<String>,
    limit: Option<u32>,
    before_id: Option<String>,
    after_id: Option<String>,
}

fn history_argv(group_id: &str, q: &HistoryQuery) -> Result<Vec<OsString>, CommError> {
    hex32("group_id", group_id)?;
    if let Some(limit) = q.limit
        && !(1..=200).contains(&limit)
    {
        return Err(CommError::Invalid(format!(
            "limit must be in 1..=200, got {limit}"
        )));
    }
    if q.before_id.is_some() && q.after_id.is_some() {
        return Err(CommError::Invalid(
            "use only one of before_id and after_id".to_owned(),
        ));
    }
    // The cursor is deliberately NOT hex-checked here: whether it names a
    // message of this group is Hyphae's call, and it answers with a
    // `user_error` (400) — see `history_is_an_array_pages_by_cursor_and_rejects_bad_cursors`.
    for (field, v) in [("before_id", &q.before_id), ("after_id", &q.after_id)] {
        if let Some(v) = v {
            text(field, v, 128)?;
        }
    }
    let limit = q.limit.map(|n| n.to_string());
    Ok(Argv::new(&["history"])
        .flag("--group-id", group_id)
        .opt("--limit", limit.as_deref())
        .opt("--before-id", q.before_id.as_deref())
        .opt("--after-id", q.after_id.as_deref())
        .identity(q.identity.as_deref())?
        .0)
}

// ---------------------------------------------------------------------
// execution
// ---------------------------------------------------------------------

/// Hyphae's Go side marshals an empty slice as `null` (`"messages":null`
/// for a group with no history); the UI contract is "always an array".
fn arrayify(mut data: Value, key: &str) -> Value {
    if let Some(slot) = data.get_mut(key)
        && slot.is_null()
    {
        *slot = json!([]);
    }
    data
}

async fn run(state: &CommState, argv: Vec<OsString>, list_key: Option<&str>) -> CommResult {
    let (runner, password_store, home) = state.require_ready()?;
    let account = resolve_account(home).await?.ok_or_else(|| {
        CommError::NotConfigured("no keystore exists yet; create an identity first".to_owned())
    })?;
    let password = password_store
        .get(&account)
        .await
        .map_err(map_store_error)?;
    let envelope = runner
        .run(Invocation {
            args: argv,
            password: Some(password),
            timeout: None,
        })
        .await
        .map_err(map_runner_error)?;
    match envelope {
        Envelope::Ok { data } => {
            let data = match list_key {
                Some(key) => arrayify(data, key),
                None => data,
            };
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

fn body<T>(parsed: Result<Json<T>, JsonRejection>) -> Result<T, CommError> {
    parsed
        .map(|Json(v)| v)
        .map_err(|e| CommError::Invalid(e.body_text()))
}

fn query<T>(parsed: Result<Query<T>, QueryRejection>) -> Result<T, CommError> {
    parsed
        .map(|Query(v)| v)
        .map_err(|e| CommError::Invalid(e.body_text()))
}

async fn list_groups(
    State(state): State<CommState>,
    q: Result<Query<IdentityQuery>, QueryRejection>,
) -> CommResult {
    let q = query(q)?;
    let argv = Argv::new(&["list"]).identity(q.identity.as_deref())?.0;
    run(&state, argv, Some("groups")).await
}

async fn create_group(
    State(state): State<CommState>,
    req: Result<Json<CreateReq>, JsonRejection>,
) -> CommResult {
    run(&state, create_argv(&body(req)?)?, None).await
}

async fn members(
    State(state): State<CommState>,
    Path(group_id): Path<String>,
    q: Result<Query<IdentityQuery>, QueryRejection>,
) -> CommResult {
    hex32("group_id", &group_id)?;
    let q = query(q)?;
    let argv = Argv::new(&["members"])
        .flag("--group-id", &group_id)
        .identity(q.identity.as_deref())?
        .0;
    run(&state, argv, Some("members")).await
}

async fn invites(
    State(state): State<CommState>,
    Path(group_id): Path<String>,
    q: Result<Query<IdentityQuery>, QueryRejection>,
) -> CommResult {
    hex32("group_id", &group_id)?;
    let q = query(q)?;
    let argv = Argv::new(&["invite", "list"])
        .flag("--group", &group_id)
        .identity(q.identity.as_deref())?
        .0;
    run(&state, argv, Some("invites")).await
}

async fn history(
    State(state): State<CommState>,
    Path(group_id): Path<String>,
    q: Result<Query<HistoryQuery>, QueryRejection>,
) -> CommResult {
    run(
        &state,
        history_argv(&group_id, &query(q)?)?,
        Some("messages"),
    )
    .await
}

async fn send_message(
    State(state): State<CommState>,
    Path(group_id): Path<String>,
    req: Result<Json<SendReq>, JsonRejection>,
) -> CommResult {
    run(&state, send_argv(&group_id, &body(req)?)?, None).await
}

async fn status(
    State(state): State<CommState>,
    Path((group_id, logical_id)): Path<(String, String)>,
    q: Result<Query<IdentityQuery>, QueryRejection>,
) -> CommResult {
    hex32("group_id", &group_id)?;
    hex32("logical_id", &logical_id)?;
    let q = query(q)?;
    let argv = Argv::new(&["status"])
        .flag("--group-id", &group_id)
        .flag("--logical-id", &logical_id)
        .identity(q.identity.as_deref())?
        .0;
    run(&state, argv, None).await
}

async fn retry(
    State(state): State<CommState>,
    Path((group_id, logical_id)): Path<(String, String)>,
    req: Result<Json<RetryReq>, JsonRejection>,
) -> CommResult {
    run(
        &state,
        retry_argv(&group_id, &logical_id, &body(req)?)?,
        None,
    )
    .await
}

async fn accept(
    State(state): State<CommState>,
    Path(invite_id): Path<String>,
    req: Result<Json<AcceptReq>, JsonRejection>,
) -> CommResult {
    run(&state, accept_argv(&invite_id, &body(req)?)?, None).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const G: &str = "0123456789abcdef0123456789abcdef";

    fn strs(argv: &[OsString]) -> Vec<&str> {
        argv.iter().map(|a| a.to_str().unwrap()).collect()
    }

    #[test]
    fn hex32_accepts_only_32_lowercase_hex() {
        assert!(hex32("x", G).is_ok());
        for bad in [
            "",
            "abc",
            &G.to_uppercase(),
            &format!("{G}0"),
            "g123456789abcdef0123456789abcdef",
        ] {
            assert!(hex32("x", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn history_argv_passes_cursor_and_limit_and_rejects_both_cursors() {
        let q = HistoryQuery {
            limit: Some(5),
            before_id: Some("m1".into()),
            identity: Some("alice".into()),
            ..Default::default()
        };
        let argv = history_argv(G, &q).unwrap();
        assert_eq!(
            strs(&argv),
            [
                "groupchat",
                "history",
                "--group-id",
                G,
                "--limit",
                "5",
                "--before-id",
                "m1",
                "--identity",
                "alice"
            ]
        );
        let both = HistoryQuery {
            before_id: Some("a".into()),
            after_id: Some("b".into()),
            ..Default::default()
        };
        assert!(matches!(history_argv(G, &both), Err(CommError::Invalid(_))));
        for limit in [0, 201] {
            let q = HistoryQuery {
                limit: Some(limit),
                ..Default::default()
            };
            assert!(history_argv(G, &q).is_err(), "limit {limit}");
        }
    }

    #[test]
    fn send_argv_omits_unset_options_and_validates_ids() {
        let req = SendReq {
            body: "hi".into(),
            logical_id: None,
            max_retries: Some(3),
            identity: None,
        };
        assert_eq!(
            strs(&send_argv(G, &req).unwrap()),
            [
                "groupchat",
                "send",
                "--group-id",
                G,
                "--body",
                "hi",
                "--max-retries",
                "3"
            ]
        );
        assert!(send_argv("nope", &req).is_err());
        let bad_logical = SendReq {
            logical_id: Some("short".into()),
            ..req
        };
        assert!(send_argv(G, &bad_logical).is_err());
        let blank = SendReq {
            body: "  ".into(),
            logical_id: None,
            max_retries: None,
            identity: None,
        };
        assert!(send_argv(G, &blank).is_err());
    }

    #[test]
    fn create_and_retry_reject_malformed_npubs_before_spawning() {
        let c = CreateReq {
            name: "g".into(),
            invitees: vec!["npub1bogus".into()],
            identity: None,
        };
        assert!(matches!(create_argv(&c), Err(CommError::Invalid(_))));
        let r = RetryReq {
            recipient: Some("xx".into()),
            identity: None,
        };
        assert!(retry_argv(G, G, &r).is_err());
    }

    #[test]
    fn create_requires_at_least_one_invitee() {
        // Hyphae answers an empty invitee list with `Required flag "invitee"
        // not set`; reject it here with a message that names the JSON field.
        let c = CreateReq {
            name: "g".into(),
            invitees: vec![],
            identity: None,
        };
        let err = create_argv(&c).unwrap_err();
        assert!(
            matches!(&err, CommError::Invalid(m) if m.contains("invitees")),
            "{err}"
        );
    }

    #[test]
    fn arrayify_turns_go_nil_slices_into_arrays() {
        let out = arrayify(json!({"messages": null}), "messages");
        assert_eq!(out, json!({"messages": []}));
        let kept = arrayify(json!({"messages": [1]}), "messages");
        assert_eq!(kept, json!({"messages": [1]}));
    }
}
