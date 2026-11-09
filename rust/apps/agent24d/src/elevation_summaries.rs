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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

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
