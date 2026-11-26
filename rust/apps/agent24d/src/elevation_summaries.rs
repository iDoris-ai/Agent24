//! K-B3b (#1715): the presence text for an approval decision, a schedule
//! change and shutdown, derived by the daemon from its own records when the
//! request is first refused with `presence_required`.
//!
//! In service mode the desktop shows only the daemon's presence dialog for
//! these (#1738), so this text is everything the user sees before approving.
//! The rules (#1715, comments 6732795187 and 6732823397):
//!
//! - Every field comes from the daemon's records or from the request body
//!   parsed exactly as the handler parses it; nothing the caller names is
//!   trusted as a label.
//! - A tool approval shows the decision and its scope, the approval and run
//!   ids, the session title, the tool name and a bounded preview of the
//!   arguments (at least what the old IPC dialog showed: the user approving a
//!   `shell_exec` has to see the command).
//! - Never shown: the run's input text, the deny reason, a schedule's prompt,
//!   any value under a credential-like key.
//! - Anything that cannot be found or shown leaves no text, and the elevate
//!   is then refused (`not_elevated_operation`): fail closed. The handler
//!   would refuse the same request anyway (404, 409 or 400).

use agent24_protocol::{ApprovalStatus, ModuleApprovalDecision, ScheduleSpec, ScheduleUpdate};
use serde_json::{Map, Value};

use crate::capabilities::first_party::{SHOWN_MAX_CHARS, SUMMARY_MAX_UTF16, hidden, shown};
use crate::server::AppState;

/// The bytes of argument JSON a preview keeps, as `summarize_input` does.
pub const PREVIEW_MAX_BYTES: usize = 200;
/// Kept free for the "{identifier} (pid {pid}) wants to: " prefix `elevate`
/// adds before the whole dialog string is measured against
/// `SUMMARY_MAX_UTF16`.
const PREFIX_RESERVE_UTF16: usize = 64;
/// Shown for a run with no session, or a session with an empty title.
pub const UNTITLED: &str = "untitled";
/// The fixed shutdown text.
pub const SHUTDOWN_TEXT: &str = "stop the Agent24 background service (running work stops)";

/// Presence text for `POST /api/v1/os/{name}/delegation`.
///
/// Not a [`crate::capabilities::first_party::DERIVED_ROUTES`] row. That list
/// is exactly the Elevated rows of approval, schedule, shutdown and models
/// (`derived_routes_are_the_elevated_rows_they_summarise`). The surface is
/// read at elevate time, the same way kernel consent stays off that list.
/// `routes_prefix` is the first 12 hex digits of `routes_sha256`.
pub fn module_delegation_summary(
    name: &str,
    version: &str,
    namespace: &str,
    routes_prefix: &str,
) -> String {
    format!(
        "允许发起方写入 {name} 模块的数据; delegate module '{name}'; version: {version}; namespace: {namespace}; routes_sha256: {routes_prefix}"
    )
}

/// The text for `method` + `template` at `path` with `body`, or `None`.
pub async fn derive(
    state: &AppState,
    method: &str,
    template: &str,
    path: &str,
    body: &[u8],
) -> Option<String> {
    let text = match (method, template) {
        ("POST", "/api/v1/approvals/{id}") => approval(state, last_segment(path)?, body).await?,
        ("POST", "/api/v1/module-approvals/{id}") => {
            module_approval(state, last_segment(path)?, body).await?
        }
        ("POST", "/api/v1/module-consents/modules/{module}/tools/{tool}") => {
            module_consent(state, path, body)?
        }
        ("POST", "/api/v1/module-consents/modules/{module}/tools") => {
            module_consents(state, path, body)?
        }
        ("POST", "/api/v1/schedules") => schedule_create(body)?,
        ("PATCH", "/api/v1/schedules/{id}") => {
            let update: ScheduleUpdate = serde_json::from_slice(body).ok()?;
            if update.is_empty() {
                return None;
            }
            let name = schedule_name(state, last_segment(path)?).await?;
            format!("change schedule {name}: {}", changed_fields(&update))
        }
        ("DELETE", "/api/v1/schedules/{id}") => {
            empty(body)?;
            format!(
                "delete schedule {}",
                schedule_name(state, last_segment(path)?).await?
            )
        }
        ("POST", "/api/v1/schedules/{id}/run_now") => {
            empty(body)?;
            format!(
                "run schedule {} now",
                schedule_name(state, segment_before(path, "run_now")?).await?
            )
        }
        ("POST", "/api/v1/schedules/{id}/suspend") => {
            empty(body)?;
            format!(
                "suspend schedule {}",
                schedule_name(state, segment_before(path, "suspend")?).await?
            )
        }
        ("POST", "/api/v1/schedules/{id}/resume") => {
            empty(body)?;
            format!(
                "resume schedule {}",
                schedule_name(state, segment_before(path, "resume")?).await?
            )
        }
        ("POST", "/api/v1/shutdown") => SHUTDOWN_TEXT.to_owned(),
        ("PUT", "/api/v1/models/default") => {
            let parsed: crate::routes::PutDefaultModelBody = serde_json::from_slice(body).ok()?;
            let model = crate::default_model::validate_model_id(&parsed.model).ok()?;
            format!("把默认聊天模型改为 {}", shown(&model)?)
        }
        (_, t) if t.starts_with("/api/v1/comm/") => comm(method, template, path, body)?,
        _ => return None,
    };
    // The length is checked where it is authoritative: `remember` on the
    // details and `elevate` on the whole dialog string, prefix included.
    Some(text)
}

fn details_budget() -> usize {
    SUMMARY_MAX_UTF16 - PREFIX_RESERVE_UTF16
}

fn empty(body: &[u8]) -> Option<()> {
    body.is_empty().then_some(())
}

/// A path segment as the handler reads it. A percent-encoded one is refused:
/// some of these handlers read the raw segment and some the decoded one, so
/// only a segment that is the same both ways is shown.
fn plain(segment: &str) -> Option<&str> {
    (!segment.is_empty() && !segment.contains('%')).then_some(segment)
}

fn last_segment(path: &str) -> Option<&str> {
    plain(path.rsplit('/').next()?)
}

fn segment_before<'a>(path: &'a str, last: &str) -> Option<&'a str> {
    plain(
        path.strip_suffix(last)?
            .strip_suffix('/')?
            .rsplit('/')
            .next()?,
    )
}

/// `POST /api/v1/approvals/{id}`.
async fn approval(state: &AppState, id: &str, body: &[u8]) -> Option<String> {
    // The handler's own parse (`approvals::decide_approval`).
    let decision: agent24_protocol::Decision = serde_json::from_slice(body).ok()?;
    let record = state.store.get_approval(id).await.ok()??;
    if record.status != ApprovalStatus::Pending
        || !record.available_decisions.contains(&decision.kind)
    {
        return None;
    }
    let scope = match decision.kind.as_str() {
        "approve" => "approve once".to_owned(),
        "approve_for_session" => "approve for the rest of this session".to_owned(),
        "approve_for_target" => {
            format!(
                "approve always for target {}",
                shown(record.standing_target.as_deref()?)?
            )
        }
        // The reason travels with the request and into the audit; it is not
        // the user's to re-read here.
        "deny" => "deny (with your reason)".to_owned(),
        "abort" => "abort the whole run".to_owned(),
        _ => return None,
    };
    // The approval has no tool field: the tool call it gates does. The agent
    // inserts that row before it dispatches into the approval gate.
    let tool = state
        .store
        .list_tool_calls(&record.run_id)
        .await
        .ok()?
        .into_iter()
        .find(|call| call.id == record.tool_call_id)?
        .tool;
    let head = format!(
        "{scope}: approval {} of run {}",
        shown(&record.id)?,
        shown(&record.run_id)?
    );
    let tail = format!(
        "; tool {}; arguments: {}",
        shown(&tool)?,
        preview(&Value::Object(record.payload.clone()))
    );
    let title = session_title(state, &record.run_id).await?;
    Some(fit_title(&head, &title, &tail))
}

/// `{head}, conversation {title}{tail}` with the title as a JSON string
/// whose hidden characters are made visible. The title is cut first, to at
/// most `SHOWN_MAX_CHARS` characters and then to what the budget leaves;
/// the preview in `tail` is never cut further.
fn fit_title(head: &str, title: &str, tail: &str) -> String {
    let line = |t: &str| format!("{head}, conversation {}{tail}", visible(&quoted(t)));
    let fits = |t: &str| line(t).encode_utf16().count() <= details_budget();
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= SHOWN_MAX_CHARS && fits(title) {
        return line(title);
    }
    let mut keep = chars.len().min(SHOWN_MAX_CHARS - 1);
    loop {
        let cut: String = chars[..keep].iter().collect::<String>() + "…";
        if keep == 0 || fits(&cut) {
            return line(&cut);
        }
        keep -= 1;
    }
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// The run's session title, trimmed, or [`UNTITLED`]. `None` only when the
/// run itself cannot be read.
async fn session_title(state: &AppState, run_id: &str) -> Option<String> {
    let run = state.store.get_run(run_id).await.ok()??;
    let title = match run.session_id {
        Some(session) => state
            .store
            .get_session(&session)
            .await
            .ok()?
            .map(|s| s.title)
            .unwrap_or_default(),
        None => String::new(),
    };
    let title = title.trim();
    Some(if title.is_empty() {
        UNTITLED.to_owned()
    } else {
        title.to_owned()
    })
}

/// The argument preview: credential-like values replaced by `"***"` at any
/// depth, one-line JSON, every character [`shown`] would refuse written as
/// `\u{XXXX}`, then cut to `PREVIEW_MAX_BYTES` on a character (and escape)
/// boundary with the same marker as `agent24_tools::truncate`. With nothing
/// masked or escaped it is byte-for-byte `summarize_input` of the payload.
pub fn preview(payload: &Value) -> String {
    let json = mask(payload).to_string();
    let pieces: Vec<String> = json
        .chars()
        .map(|c| {
            if hidden(c) {
                format!("\\u{{{:04X}}}", u32::from(c))
            } else {
                c.to_string()
            }
        })
        .collect();
    let total: usize = pieces.iter().map(String::len).sum();
    if total <= PREVIEW_MAX_BYTES {
        return pieces.concat();
    }
    let mut out = String::new();
    for piece in &pieces {
        if out.len() + piece.len() > PREVIEW_MAX_BYTES {
            break;
        }
        out.push_str(piece);
    }
    let cut = total - out.len();
    format!("{out}… [truncated {cut} bytes]")
}

/// `value` with every credential-like key's value replaced by `"***"`.
fn mask(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if credential_key(k) {
                        Value::String("***".into())
                    } else {
                        mask(v)
                    };
                    (k.clone(), v)
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(mask).collect()),
        other => other.clone(),
    }
}

/// #1715 6732823397: token, secret, password, passwd, api_key, apikey,
/// authorization, credential, cookie, private_key, and any key ending in
/// `_token`, `_secret` or `_key`; case-insensitive.
pub fn credential_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    matches!(
        k.as_str(),
        "token"
            | "secret"
            | "password"
            | "passwd"
            | "api_key"
            | "apikey"
            | "authorization"
            | "credential"
            | "cookie"
            | "private_key"
    ) || k.ends_with("_token")
        || k.ends_with("_secret")
        || k.ends_with("_key")
}

/// `value` with every character [`shown`] would refuse written as `\u{XXXX}`.
fn visible(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if hidden(c) {
                format!("\\u{{{:04X}}}", u32::from(c))
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// `POST /api/v1/module-approvals/{id}`.
async fn module_approval(state: &AppState, id: &str, body: &[u8]) -> Option<String> {
    let decision = crate::module_approvals::parse_decide_body(body)?;
    let record = state.module_approval_broker.get(id).await.ok()??;
    if record.decision != ModuleApprovalDecision::Pending {
        return None;
    }
    let verb = match decision {
        ModuleApprovalDecision::Approved => "approve",
        ModuleApprovalDecision::Denied => "deny",
        _ => return None,
    };
    let target = match &record.target {
        Some(target) => format!("; target {}", shown(target)?),
        None => String::new(),
    };
    Some(format!(
        "{verb} module approval {}: module {}, action {}{target}; arguments: {}",
        shown(&record.id)?,
        shown(&record.module)?,
        shown(&record.action)?,
        preview(&record.payload)
    ))
}

/// `POST /api/v1/module-consents/modules/{module}/tools/{tool}`.
/// Like the other two, only for a record the daemon has: decisions open on
/// this host, and a card for this tool that takes grants (the handler's
/// `grants_closed`, `no_such_tool` and `protocol_too_old`).
fn module_consent(state: &AppState, path: &str, body: &[u8]) -> Option<String> {
    let rest = path.strip_prefix("/api/v1/module-consents/modules/")?;
    let (module, tool) = rest.split_once("/tools/")?;
    let (module, tool) = (plain(module)?, plain(tool)?);
    state.module_grants.as_ref()?;
    let cards = state.module_tool_cards.as_ref()?;
    if !cards
        .tools
        .get(&(module.to_owned(), tool.to_owned()))?
        .takes_grants
    {
        return None;
    }
    let request = crate::consent_routes::parse_decision_request(body)?;
    let kinds = request.resource_kinds()?;
    let resources = if kinds.is_empty() {
        "no resources".to_owned()
    } else {
        format!(
            "{} resource(s) of kind {}",
            request.resource_count(),
            shown(&kinds.join(", "))?
        )
    };
    Some(format!(
        "{} consent for module {} tool {} until {}; {resources}",
        if request.grants() { "grant" } else { "deny" },
        shown(module)?,
        shown(tool)?,
        shown(request.expires_at()?)?
    ))
}

/// A1: `POST /api/v1/module-consents/modules/{module}/tools`, every tool
/// named, so the user sees all that one confirmation grants. Only for tools
/// whose cards take grants; only grants (a batch never denies).
fn module_consents(state: &AppState, path: &str, body: &[u8]) -> Option<String> {
    let module = plain(
        path.strip_prefix("/api/v1/module-consents/modules/")?
            .strip_suffix("/tools")?,
    )?;
    state.module_grants.as_ref()?;
    let cards = state.module_tool_cards.as_ref()?;
    let request = crate::consent_routes::parse_batch_request(body)?;
    let mut tools = Vec::new();
    for tool in request.tool_names() {
        let tool = plain(tool)?;
        if !cards
            .tools
            .get(&(module.to_owned(), tool.to_owned()))?
            .takes_grants
        {
            return None;
        }
        tools.push(tool);
    }
    let kinds = request.resource_kinds()?;
    let resources = if kinds.is_empty() {
        "no resources".to_owned()
    } else {
        format!(
            "{} resource(s) of kind {}",
            request.resource_count(),
            shown(&kinds.join(", "))?
        )
    };
    Some(format!(
        "grant consent for module {} tools {} until {}; {resources}",
        shown(module)?,
        shown(&tools.join(", "))?,
        shown(request.expires_at()?)?
    ))
}

/// `POST /api/v1/schedules`: the name and spec from the body, as the handler
/// parses it; the action (the prompt) and the delivery are not shown.
fn schedule_create(body: &[u8]) -> Option<String> {
    let create: agent24_protocol::ScheduleCreate = serde_json::from_slice(body).ok()?;
    Some(format!(
        "create schedule {} ({}; {})",
        shown(&create.name)?,
        spec_text(&create.spec)?,
        if create.enabled {
            "enabled"
        } else {
            "disabled"
        }
    ))
}

fn spec_text(spec: &ScheduleSpec) -> Option<String> {
    Some(match spec {
        ScheduleSpec::Cron { expr, tz } => match tz {
            Some(tz) => format!("cron {} in {}", shown(expr)?, shown(tz)?),
            None => format!("cron {} in UTC", shown(expr)?),
        },
        ScheduleSpec::Every { secs } => format!("every {secs} s"),
        ScheduleSpec::At { ts } => format!("once at {}", shown(ts)?),
    })
}

/// Field names only: a new prompt or delivery is not shown.
fn changed_fields(update: &ScheduleUpdate) -> String {
    [
        ("name", update.name.is_some()),
        ("enabled", update.enabled.is_some()),
        ("spec", update.spec.is_some()),
        ("action", update.action.is_some()),
        ("delivery", update.delivery.is_some()),
    ]
    .iter()
    .filter(|(_, set)| *set)
    .map(|(name, _)| *name)
    .collect::<Vec<_>>()
    .join(", ")
}

/// The schedule's name from the scheduler, visible; `None` when it is gone.
async fn schedule_name(state: &AppState, id: &str) -> Option<String> {
    let schedule = state.scheduler.get(id).await.ok()?;
    Some(format!(
        "{} ({})",
        shown(&schedule.name)?,
        shown(&schedule.id)?
    ))
}

/// A Hyphae write from `COMM_DERIVED_ROUTES`. Names, npubs, ids and relay
/// URLs are shown as `shown` quotes them; a message body never is, only its
/// length in characters.
fn comm(method: &str, template: &str, path: &str, body: &[u8]) -> Option<String> {
    let object = || -> Option<Map<String, Value>> {
        if body.is_empty() {
            return Some(Map::new());
        }
        match serde_json::from_slice(body).ok()? {
            Value::Object(map) => Some(map),
            _ => None,
        }
    };
    let field =
        |map: &Map<String, Value>, key: &str| -> Option<String> { shown(map.get(key)?.as_str()?) };
    let text = match (method, template) {
        ("POST", "/api/v1/comm/identity") => {
            format!("创建通信身份 {}", field(&object()?, "nickname")?)
        }
        ("POST", "/api/v1/comm/identity/default") => {
            format!("把默认通信身份设为 {}", field(&object()?, "nickname")?)
        }
        ("POST", "/api/v1/comm/contact") => {
            let map = object()?;
            format!(
                "添加联系人 {}（{}）",
                field(&map, "nickname")?,
                field(&map, "npub")?
            )
        }
        ("PATCH", "/api/v1/comm/contact/{name}") => format!(
            "把联系人 {} 改名为 {}",
            decoded_segment(path)?,
            field(&object()?, "nickname")?
        ),
        ("DELETE", "/api/v1/comm/contact/{name}") => {
            format!("删除联系人 {}（保留聊天记录）", decoded_segment(path)?)
        }
        ("PUT", "/api/v1/comm/relay") => {
            let relays = object()?.get("relays")?.as_array()?.clone();
            if relays.is_empty() {
                return None;
            }
            let shown_relays = relays
                .iter()
                .map(|r| shown(r.as_str()?))
                .collect::<Option<Vec<_>>>()?;
            format!("把通信中继改为 {}", shown_relays.join(", "))
        }
        ("POST", "/api/v1/comm/relay/probe") => match object()?.get("url") {
            None | Some(Value::Null) => "检测当前通信中继是否可连".to_owned(),
            Some(url) => format!("检测通信中继 {} 是否可连", shown(url.as_str()?)?),
        },
        ("POST", "/api/v1/comm/daemon/start") => "启动 Hyphae 通信后台".to_owned(),
        ("POST", "/api/v1/comm/daemon/stop") => "停止 Hyphae 通信后台".to_owned(),
        ("POST", "/api/v1/comm/send") => {
            let map = object()?;
            let content = map.get("content")?.as_str()?;
            format!(
                "给 {} 发送一条 {} 字的消息",
                field(&map, "to")?,
                content.chars().count()
            )
        }
        ("POST", "/api/v1/comm/outbox/{event_id}/retry") => {
            format!("重发一条未送达的消息 {}", shown(last_segment(path)?)?)
        }
        ("POST", "/api/v1/comm/groupchat") => {
            let map = object()?;
            let invitees = map.get("invitees")?.as_array()?;
            format!(
                "创建群聊 {} 并邀请 {} 人",
                field(&map, "name")?,
                invitees.len()
            )
        }
        ("POST", "/api/v1/comm/groupchat/{group_id}/messages") => {
            let map = object()?;
            let content = map.get("body")?.as_str()?;
            format!(
                "在群 {} 发送一条 {} 字的消息",
                shown(segment_before(path, "messages")?)?,
                content.chars().count()
            )
        }
        ("POST", "/api/v1/comm/groupchat/{group_id}/messages/{logical_id}/retry") => {
            object()?;
            format!(
                "重试群消息 {} 的失败收件人",
                shown(segment_before(path, "retry")?)?
            )
        }
        ("POST", "/api/v1/comm/groupchat/invites/{invite_id}/accept") => {
            object()?;
            format!("接受群邀请 {}", shown(segment_before(path, "accept")?)?)
        }
        _ => return None,
    };
    Some(text)
}

/// The last path segment percent-decoded as axum's `Path` decodes it, so a
/// contact nickname in any script reads as the handler will see it.
fn decoded_segment(path: &str) -> Option<String> {
    let raw = path.rsplit('/').next()?;
    let name = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()?;
    if name.is_empty() {
        return None;
    }
    shown(&name)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn comm_writes_have_summaries_that_hide_message_bodies() {
        let send = comm(
            "POST",
            "/api/v1/comm/send",
            "/api/v1/comm/send",
            br#"{"to":"bob","content":"secret plan"}"#,
        )
        .unwrap();
        assert!(send.contains("\"bob\"") && send.contains("11"), "{send}");
        assert!(!send.contains("secret"), "{send}");
        let rename = comm(
            "PATCH",
            "/api/v1/comm/contact/{name}",
            "/api/v1/comm/contact/%E5%B0%8F%E6%98%8E",
            br#"{"nickname":"Ming"}"#,
        )
        .unwrap();
        assert!(
            rename.contains("\"小明\"") && rename.contains("\"Ming\""),
            "{rename}"
        );
        let group = comm(
            "POST",
            "/api/v1/comm/groupchat/{group_id}/messages",
            "/api/v1/comm/groupchat/00112233445566778899aabbccddeeff/messages",
            br#"{"body":"hi\nall","logical_id":"00112233445566778899aabbccddeeff"}"#,
        )
        .unwrap();
        assert!(!group.contains("hi"), "{group}");
        assert!(
            comm(
                "POST",
                "/api/v1/comm/daemon/start",
                "/api/v1/comm/daemon/start",
                b""
            )
            .is_some()
        );
        // A body the handler would refuse, or a hidden character, leaves no text.
        assert!(comm("POST", "/api/v1/comm/send", "/api/v1/comm/send", b"[]").is_none());
        assert!(
            comm(
                "POST",
                "/api/v1/comm/contact",
                "/api/v1/comm/contact",
                br#"{"nickname":"a\u202eb","npub":"npub1x"}"#
            )
            .is_none()
        );
    }

    #[test]
    fn credential_keys_by_name_and_suffix_any_case() {
        for key in [
            "token",
            "Secret",
            "PASSWORD",
            "passwd",
            "api_key",
            "ApiKey",
            "Authorization",
            "credential",
            "cookie",
            "private_key",
            "github_token",
            "client_secret",
            "ssh_key",
        ] {
            assert!(credential_key(key), "{key}");
        }
        for key in [
            "argv",
            "cwd",
            "url",
            "monkey",
            "tokens",
            "keyboard",
            "key",
            "secretary",
        ] {
            assert!(!credential_key(key), "{key}");
        }
    }

    #[test]
    fn with_nothing_masked_or_escaped_the_preview_is_summarize_input() {
        for payload in [
            serde_json::json!({}),
            serde_json::json!({ "argv": ["ls"], "cwd": "/w" }),
            serde_json::json!({ "text": "中文".repeat(80) }),
            serde_json::json!({ "text": "x".repeat(199) }),
            serde_json::json!({ "text": "x".repeat(400), "n": 1 }),
            serde_json::json!({ "line": "a\nb\u{0001}c" }),
        ] {
            let map = payload.as_object().unwrap();
            assert_eq!(
                preview(&payload),
                agent24_tools::summarize_input(map),
                "{payload}"
            );
        }
    }

    #[test]
    fn masks_at_any_depth_and_keeps_the_rest() {
        let payload = serde_json::json!({
            "a": [{ "token": "s1" }, { "b": { "Cookie": "s2", "c": "keep" } }],
            "password": { "nested": "s3" },
        });
        let out = preview(&payload);
        assert!(
            !out.contains("s1") && !out.contains("s2") && !out.contains("s3"),
            "{out}"
        );
        assert_eq!(out.matches("\"***\"").count(), 3, "{out}");
        assert!(out.contains("\"c\":\"keep\""), "{out}");
    }

    #[test]
    fn escapes_are_never_cut_in_half() {
        let payload = serde_json::json!({ "z": "\u{200B}".repeat(100) });
        let out = preview(&payload);
        let (kept, marker) = out.split_once('…').unwrap();
        assert!(kept.len() <= PREVIEW_MAX_BYTES, "{kept}");
        assert!(kept.ends_with("\\u{200B}"), "{kept}");
        assert!(!kept.contains('\u{200B}'));
        assert!(marker.starts_with(" [truncated "), "{marker}");
    }

    #[test]
    fn a_percent_encoded_or_empty_segment_is_not_shown() {
        assert_eq!(last_segment("/api/v1/approvals/apr_1"), Some("apr_1"));
        assert_eq!(last_segment("/api/v1/approvals/apr%5F1"), None);
        assert_eq!(last_segment("/api/v1/approvals/"), None);
        assert_eq!(
            segment_before("/api/v1/schedules/s1/run_now", "run_now"),
            Some("s1")
        );
        assert_eq!(
            segment_before("/api/v1/schedules/s%31/run_now", "run_now"),
            None
        );
    }

    #[test]
    fn schedule_specs_and_changes_are_named_not_quoted() {
        assert_eq!(
            spec_text(&ScheduleSpec::Every { secs: 60 }).unwrap(),
            "every 60 s"
        );
        assert_eq!(
            spec_text(&ScheduleSpec::At {
                ts: "2026-11-10T09:00:00Z".into()
            })
            .unwrap(),
            "once at \"2026-11-10T09:00:00Z\""
        );
        assert_eq!(
            spec_text(&ScheduleSpec::Cron {
                expr: "0 3 * * *".into(),
                tz: None
            })
            .unwrap(),
            "cron \"0 3 * * *\" in UTC"
        );
        assert!(
            spec_text(&ScheduleSpec::Cron {
                expr: "0\u{200B} 3 * * *".into(),
                tz: None
            })
            .is_none()
        );
        let update = ScheduleUpdate {
            enabled: Some(false),
            delivery: Some(vec![]),
            ..Default::default()
        };
        assert_eq!(changed_fields(&update), "enabled, delivery");
    }
}
