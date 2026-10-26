//! TUI-2a / TUI-2a2：daemon 的只读 GET。
//! session / run / memory / schedule / usage，以及 timings、consent、
//! approval、tools、decide profile。
//! 写操作留给 TUI-2b（CORE-2 提升级在场证明）。
//!
//! 路径字面量必须写在 `Method::GET` 元组里，`scripts/tui-coverage.mjs` 才认。

use std::time::Duration;

use serde_json::Value;

const JSON_HELP: &str = "原样输出 JSON 响应";

#[derive(clap::Subcommand)]
pub enum SessionAction {
    /// 列出会话
    List {
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    /// 读取一条会话
    Get {
        id: String,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum RunAction {
    /// 读取一条 run
    Get {
        id: String,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum MemoryAction {
    /// 列出或检索个人记忆断言
    List {
        /// 全文检索；省略则按记录时间倒序
        #[arg(long)]
        q: Option<String>,
        /// 条数。daemon 默认 20，最大 200
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    /// 读取个人记忆暂停开关
    Settings {
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum ScheduleAction {
    /// 列出定时任务
    List {
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    /// 读取一条定时任务
    Get {
        id: String,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum ConsentAction {
    /// 列出模块工具授权
    List {
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    /// 导出模块工具授权
    Export {
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum ApprovalAction {
    /// 模块审批（gate / advise），与 run 审批队列分开
    Module {
        #[command(subcommand)]
        action: ModuleApprovalAction,
    },
}

#[derive(clap::Subcommand)]
pub enum ModuleApprovalAction {
    /// 列出模块审批
    List {
        /// pending、approved、denied 或 timed_out
        #[arg(long)]
        decision: Option<String>,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    /// 读取一条模块审批
    Get {
        id: String,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum ToolsAction {
    /// 列出已注册工具
    List {
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    /// 列出目标级长期授权
    Grants {
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    /// 列出本地风险覆盖
    Overrides {
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
}

#[derive(Debug)]
enum Read {
    Sessions,
    Session {
        id: String,
    },
    Run {
        id: String,
    },
    Usage,
    Memory {
        q: Option<String>,
        limit: Option<u32>,
    },
    MemorySettings,
    Schedules,
    Schedule {
        id: String,
    },
    Timings {
        summary: bool,
        source: Option<String>,
        since: Option<String>,
        limit: Option<u32>,
    },
    Consents,
    ConsentExport,
    ModuleApprovals {
        decision: Option<String>,
    },
    ModuleApproval {
        id: String,
    },
    Tools,
    StandingGrants,
    ToolOverrides,
    DecideProfile,
}

#[derive(Clone, Copy, Debug)]
struct View {
    list_key: Option<&'static str>,
    fields: &'static [&'static str],
}

impl View {
    const fn rows(key: &'static str, fields: &'static [&'static str]) -> Self {
        Self {
            list_key: Some(key),
            fields,
        }
    }
    const fn one(fields: &'static [&'static str]) -> Self {
        Self {
            list_key: None,
            fields,
        }
    }
}

pub async fn session(action: SessionAction) -> Result<(), String> {
    let (read, json) = match action {
        SessionAction::List { json } => (Read::Sessions, json),
        SessionAction::Get { id, json } => (Read::Session { id }, json),
    };
    run(read, json).await
}

pub async fn run_cmd(action: RunAction) -> Result<(), String> {
    let RunAction::Get { id, json } = action;
    run(Read::Run { id }, json).await
}

pub async fn memory(action: MemoryAction) -> Result<(), String> {
    let (read, json) = match action {
        MemoryAction::List { q, limit, json } => (Read::Memory { q, limit }, json),
        MemoryAction::Settings { json } => (Read::MemorySettings, json),
    };
    run(read, json).await
}

pub async fn schedule(action: ScheduleAction) -> Result<(), String> {
    let (read, json) = match action {
        ScheduleAction::List { json } => (Read::Schedules, json),
        ScheduleAction::Get { id, json } => (Read::Schedule { id }, json),
    };
    run(read, json).await
}

pub async fn usage(json: bool) -> Result<(), String> {
    run(Read::Usage, json).await
}

pub async fn timings(
    summary: bool,
    source: Option<String>,
    since: Option<String>,
    limit: Option<u32>,
    json: bool,
) -> Result<(), String> {
    run(
        Read::Timings {
            summary,
            source,
            since,
            limit,
        },
        json,
    )
    .await
}

pub async fn consent(action: ConsentAction) -> Result<(), String> {
    let (read, json) = match action {
        ConsentAction::List { json } => (Read::Consents, json),
        ConsentAction::Export { json } => (Read::ConsentExport, json),
    };
    run(read, json).await
}

pub async fn approval(action: ApprovalAction) -> Result<(), String> {
    let ApprovalAction::Module { action } = action;
    let (read, json) = match action {
        ModuleApprovalAction::List { decision, json } => (Read::ModuleApprovals { decision }, json),
        ModuleApprovalAction::Get { id, json } => (Read::ModuleApproval { id }, json),
    };
    run(read, json).await
}

pub async fn tools(action: ToolsAction) -> Result<(), String> {
    let (read, json) = match action {
        ToolsAction::List { json } => (Read::Tools, json),
        ToolsAction::Grants { json } => (Read::StandingGrants, json),
        ToolsAction::Overrides { json } => (Read::ToolOverrides, json),
    };
    run(read, json).await
}

pub async fn decide_profile(json: bool) -> Result<(), String> {
    run(Read::DecideProfile, json).await
}

async fn run(read: Read, json: bool) -> Result<(), String> {
    let ep = super::connect().await?;
    let result = execute(&ep, &read, json).await;
    super::finish(ep).await;
    println!("{}", result?);
    Ok(())
}

fn reject_blank(id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        Err("id must not be blank".to_owned())
    } else {
        Ok(())
    }
}

fn with_query(path: &str, pairs: &[(&str, Option<&str>)]) -> String {
    let mut out = String::from(path);
    let mut first = true;
    for (key, value) in pairs {
        let Some(value) = value else { continue };
        out.push(if first { '?' } else { '&' });
        first = false;
        out.push_str(key);
        out.push('=');
        out.push_str(&super::qs_encode(value));
    }
    out
}

fn request(read: &Read) -> Result<(reqwest::Method, String, View), String> {
    if let Read::Session { id }
    | Read::Run { id }
    | Read::Schedule { id }
    | Read::ModuleApproval { id } = read
    {
        reject_blank(id)?;
    }
    Ok(match read {
        Read::Sessions => (
            reqwest::Method::GET,
            "/api/v1/sessions".to_owned(),
            View::rows("sessions", &["id", "title", "channel", "updated_at"]),
        ),
        Read::Session { id } => (
            reqwest::Method::GET,
            format!("/api/v1/sessions/{}", super::qs_encode(id)),
            View::one(&[
                "id",
                "title",
                "channel",
                "workspace_id",
                "created_at",
                "updated_at",
            ]),
        ),
        Read::Run { id } => (
            reqwest::Method::GET,
            format!("/api/v1/runs/{}", super::qs_encode(id)),
            View::one(&["id", "status", "session_id", "schedule_id", "created_at"]),
        ),
        Read::Usage => (
            reqwest::Method::GET,
            "/api/v1/usage".to_owned(),
            View::one(&[
                "prompt_tokens",
                "completion_tokens",
                "total_tokens",
                "cost_usd",
            ]),
        ),
        Read::Memory { q, limit } => {
            let limit = limit.map(|n| n.to_string());
            (
                reqwest::Method::GET,
                with_query(
                    "/api/v1/memory/assertions",
                    &[("q", q.as_deref()), ("limit", limit.as_deref())],
                ),
                View::rows("assertions", &["id", "text", "status", "recorded_at"]),
            )
        }
        Read::MemorySettings => (
            reqwest::Method::GET,
            "/api/v1/memory/settings".to_owned(),
            View::one(&["enabled"]),
        ),
        Read::Schedules => (
            reqwest::Method::GET,
            "/api/v1/schedules".to_owned(),
            View::rows(
                "schedules",
                &["id", "name", "enabled", "effective_enabled", "next_run_at"],
            ),
        ),
        Read::Schedule { id } => (
            reqwest::Method::GET,
            format!("/api/v1/schedules/{}", super::qs_encode(id)),
            View::one(&[
                "id",
                "name",
                "enabled",
                "effective_enabled",
                "next_run_at",
                "last_run_at",
            ]),
        ),
        Read::Timings {
            summary: true,
            since,
            source,
            limit,
        } => {
            if source.is_some() || limit.is_some() {
                return Err("--source and --limit apply to the list, not --summary".to_owned());
            }
            (
                reqwest::Method::GET,
                with_query("/api/v1/timings/summary", &[("since", since.as_deref())]),
                View::rows(
                    "summary",
                    &[
                        "source", "model_id", "step", "count", "p50_ms", "p95_ms", "max_ms",
                    ],
                ),
            )
        }
        Read::Timings {
            summary: false,
            source,
            since,
            limit,
        } => {
            let limit = limit.map(|n| n.to_string());
            (
                reqwest::Method::GET,
                with_query(
                    "/api/v1/timings",
                    &[
                        ("source", source.as_deref()),
                        ("since", since.as_deref()),
                        ("limit", limit.as_deref()),
                    ],
                ),
                View::rows(
                    "timings",
                    &["id", "source", "model_id", "step", "total_ms", "ok"],
                ),
            )
        }
        Read::Consents => (
            reqwest::Method::GET,
            "/api/v1/module-consents".to_owned(),
            View::rows("authorizations", &["module", "tool", "host_risk", "status"]),
        ),
        Read::ConsentExport => (
            reqwest::Method::GET,
            "/api/v1/module-consents/export".to_owned(),
            View::rows("authorizations", &["module", "tool", "host_risk", "status"]),
        ),
        Read::ModuleApprovals { decision } => (
            reqwest::Method::GET,
            with_query(
                "/api/v1/module-approvals",
                &[("decision", decision.as_deref())],
            ),
            View::rows("module_approvals", &["id", "module", "action", "decision"]),
        ),
        Read::ModuleApproval { id } => (
            reqwest::Method::GET,
            format!("/api/v1/module-approvals/{}", super::qs_encode(id)),
            View::one(&["id", "module", "kind", "action", "decision", "target"]),
        ),
        Read::Tools => (
            reqwest::Method::GET,
            "/api/v1/tools".to_owned(),
            View::rows("tools", &["name", "source", "risk_class"]),
        ),
        Read::StandingGrants => (
            reqwest::Method::GET,
            "/api/v1/standing-grants".to_owned(),
            View::rows("standing_grants", &["id", "scope_kind", "tool", "target"]),
        ),
        Read::ToolOverrides => (
            reqwest::Method::GET,
            "/api/v1/tool-overrides".to_owned(),
            View::rows("overrides", &["pattern", "risk_class", "source"]),
        ),
        Read::DecideProfile => (
            reqwest::Method::GET,
            "/api/v1/decide/profile".to_owned(),
            View::one(&["hardware_tier", "effective_tier", "reasons"]),
        ),
    })
}

async fn execute(ep: &crate::Endpoint, read: &Read, json: bool) -> Result<String, String> {
    let (method, path, view) = request(read)?;
    let response = super::bearer(
        ep,
        super::client().request(method, format!("{}{path}", ep.base)),
    )
    .timeout(Duration::from_secs(15))
    .send()
    .await
    .map_err(|e| e.to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("daemon returned {status}: {text}"));
    }
    let body: Value = serde_json::from_str(&text)
        .map_err(|e| format!("daemon returned {status} but the body is not JSON: {e}"))?;
    if json { Ok(text) } else { render(&body, view) }
}

fn render(body: &Value, view: View) -> Result<String, String> {
    let text = if let Some(key) = view.list_key {
        let items = body
            .get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("response has no `{key}` array"))?;
        if items.is_empty() {
            "(none)".to_owned()
        } else {
            items
                .iter()
                .map(|item| row(item, view.fields))
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        }
    } else {
        view.fields
            .iter()
            .filter_map(|key| field(body, key))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if text.is_empty() {
        serde_json::to_string_pretty(body).map_err(|e| e.to_string())
    } else {
        Ok(text)
    }
}

fn field(body: &Value, key: &str) -> Option<String> {
    scalar(body.get(key)?).map(|value| format!("{key}: {value}"))
}

fn row(item: &Value, fields: &[&str]) -> String {
    fields
        .iter()
        .filter_map(|key| scalar(item.get(key)?))
        .collect::<Vec<_>>()
        .join("  ")
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn endpoint(port: u16) -> crate::Endpoint {
        crate::Endpoint {
            base: format!("http://127.0.0.1:{port}"),
            token: "tok".to_owned(),
            child: None,
        }
    }

    fn stub(status: &str, body: &str) -> (u16, std::sync::Arc<std::sync::Mutex<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hit = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let hit2 = hit.clone();
        let status = status.to_owned();
        let body = body.to_owned();
        std::thread::spawn(move || {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                match sock.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            *hit2.lock().unwrap() = String::from_utf8_lossy(&buf).into_owned();
            let resp = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes());
        });
        (port, hit)
    }

    async fn assert_roundtrip(read: Read, request_line: &str, body: &str, needle: &str) {
        let (port, hit) = stub("200 OK", body);
        let out = execute(&endpoint(port), &read, true).await.unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, serde_json::from_str::<Value>(body).unwrap());
        let (_, _, view) = request(&read).unwrap();
        assert!(render(&parsed, view).unwrap().contains(needle), "{needle}");
        let raw = hit.lock().unwrap().clone();
        assert_eq!(
            raw.lines().next().unwrap_or("").trim_end_matches('\r'),
            request_line,
            "{raw}"
        );
        assert!(
            raw.to_ascii_lowercase()
                .contains("authorization: bearer tok"),
            "{raw}"
        );
    }

    macro_rules! case {
        ($name:ident, $read:expr, $line:expr, $body:expr, $needle:expr) => {
            #[tokio::test]
            async fn $name() {
                assert_roundtrip($read, $line, $body, $needle).await;
            }
        };
    }

    case!(
        session_list_gets_sessions,
        Read::Sessions,
        "GET /api/v1/sessions HTTP/1.1",
        r#"{"sessions":[{"id":"s1","title":"hello title","channel":"cli","workspace_id":null,"created_at":"2026-10-26T01:00:00Z","updated_at":"2026-10-26T02:00:00Z"}]}"#,
        "hello title"
    );
    case!(
        session_get_encodes_the_path_segment,
        Read::Session { id: "a/b c".into() },
        "GET /api/v1/sessions/a%2Fb%20c HTTP/1.1",
        r#"{"id":"a/b c","title":"hello title","channel":"cli","workspace_id":null,"created_at":"2026-10-26T01:00:00Z","updated_at":"2026-10-26T02:00:00Z"}"#,
        "hello title"
    );
    case!(
        run_get_gets_one_run,
        Read::Run { id: "run_1".into() },
        "GET /api/v1/runs/run_1 HTTP/1.1",
        r#"{"id":"run_1","session_id":"s1","workspace_id":null,"status":"completed","input":{"text":"hi"},"output":{"text":"done"},"error":null,"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3,"cost_usd":0.0},"schedule_id":"sch_9","created_at":"2026-10-26T01:00:00Z","started_at":null,"ended_at":null}"#,
        "sch_9"
    );
    case!(
        memory_list_sends_q_and_limit,
        Read::Memory {
            q: Some("hello world".into()),
            limit: Some(3)
        },
        "GET /api/v1/memory/assertions?q=hello%20world&limit=3 HTTP/1.1",
        r#"{"assertions":[{"id":"m1","text":"我对花生过敏","recorded_at":"2026-10-26T01:00:00Z","status":"active","evidence":["ev1"],"session":null}]}"#,
        "我对花生过敏"
    );
    case!(
        memory_settings_gets_the_pause_switch,
        Read::MemorySettings,
        "GET /api/v1/memory/settings HTTP/1.1",
        r#"{"enabled":true}"#,
        "enabled: true"
    );
    case!(
        schedule_list_gets_schedules,
        Read::Schedules,
        "GET /api/v1/schedules HTTP/1.1",
        r#"{"schedules":[{"id":"sch_1","name":"daily digest","enabled":true,"spec":{"kind":"interval","every_seconds":60},"action":null,"delivery":[],"last_run_at":null,"next_run_at":"2026-10-27T00:00:00Z","consecutive_failures":0,"owner":null,"user_suspended":false,"system_disabled_reason":null,"effective_enabled":true,"disabled_by":null}]}"#,
        "daily digest"
    );
    case!(
        schedule_get_gets_one_schedule,
        Read::Schedule { id: "sch_1".into() },
        "GET /api/v1/schedules/sch_1 HTTP/1.1",
        r#"{"id":"sch_1","name":"daily digest","enabled":false,"spec":{"kind":"interval","every_seconds":60},"action":null,"delivery":[],"last_run_at":"2026-10-26T00:00:00Z","next_run_at":null,"consecutive_failures":0,"owner":null,"user_suspended":false,"system_disabled_reason":null,"effective_enabled":false,"disabled_by":"user"}"#,
        "daily digest"
    );
    case!(
        usage_gets_token_totals,
        Read::Usage,
        "GET /api/v1/usage HTTP/1.1",
        r#"{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7,"cost_usd":0.1}"#,
        "total_tokens: 7"
    );
    case!(
        timings_list_sends_source_since_and_limit,
        Read::Timings {
            summary: false,
            source: Some("chat".into()),
            since: Some("2026-01-01T00:00:00Z".into()),
            limit: Some(2),
        },
        "GET /api/v1/timings?source=chat&since=2026-01-01T00%3A00%3A00Z&limit=2 HTTP/1.1",
        r#"{"timings":[{"id":1,"source":"chat","model_id":"m","step":"llm","total_ms":12,"ok":true}]}"#,
        "chat"
    );
    case!(
        timings_summary_sends_since,
        Read::Timings {
            summary: true,
            source: None,
            since: Some("t0".into()),
            limit: None,
        },
        "GET /api/v1/timings/summary?since=t0 HTTP/1.1",
        r#"{"summary":[{"source":"chat","model_id":"m","step":"llm","count":2,"p50_ms":10,"p95_ms":20,"max_ms":30}]}"#,
        "chat"
    );
    case!(
        consent_list_gets_authorizations,
        Read::Consents,
        "GET /api/v1/module-consents HTTP/1.1",
        r#"{"authorizations":[{"module":"sin90","tool":"fs","host_risk":"low","status":"granted"}]}"#,
        "sin90"
    );
    case!(
        consent_export_gets_the_export,
        Read::ConsentExport,
        "GET /api/v1/module-consents/export HTTP/1.1",
        r#"{"authorizations":[{"module":"sin90","tool":"fs","host_risk":"high","status":"revoked"}]}"#,
        "revoked"
    );
    case!(
        module_approval_list_sends_decision,
        Read::ModuleApprovals {
            decision: Some("pending".into())
        },
        "GET /api/v1/module-approvals?decision=pending HTTP/1.1",
        r#"{"module_approvals":[{"id":"a1","module":"sin90","action":"write","decision":"pending"}]}"#,
        "sin90"
    );
    case!(
        module_approval_get_encodes_the_path_segment,
        Read::ModuleApproval { id: "a/b".into() },
        "GET /api/v1/module-approvals/a%2Fb HTTP/1.1",
        r#"{"id":"a/b","module":"sin90","kind":"advise","action":"write","decision":"pending","target":null}"#,
        "advise"
    );
    case!(
        tools_list_gets_tools,
        Read::Tools,
        "GET /api/v1/tools HTTP/1.1",
        r#"{"tools":[{"name":"fs_read","source":"builtin","risk_class":"read"}]}"#,
        "fs_read"
    );
    case!(
        standing_grants_lists_grants,
        Read::StandingGrants,
        "GET /api/v1/standing-grants HTTP/1.1",
        r#"{"standing_grants":[{"id":"g1","scope_kind":"session","tool":"http","target":"ops-alerts"}]}"#,
        "ops-alerts"
    );
    case!(
        tool_overrides_lists_overrides,
        Read::ToolOverrides,
        "GET /api/v1/tool-overrides HTTP/1.1",
        r#"{"overrides":[{"pattern":"mcp_fs_*","risk_class":"read","source":"cli"}]}"#,
        "mcp_fs_*"
    );
    case!(
        decide_profile_gets_tiers,
        Read::DecideProfile,
        "GET /api/v1/decide/profile HTTP/1.1",
        r#"{"hardware_tier":"t2","effective_tier":"t1","reasons":["ram"]}"#,
        "effective_tier: t1"
    );

    /// Minimal text extraction from `protocol/openapi.yaml` (no YAML
    /// dependency): the `required:` list and the property names (8-space
    /// indent) of one `components.schemas` entry.
    fn schema_fields(name: &str) -> (Vec<String>, Vec<String>) {
        const YAML: &str = include_str!("../../../../protocol/openapi.yaml");
        let head = format!("    {name}:");
        let mut lines = YAML.lines().skip_while(|l| *l != head).skip(1);
        let mut block = Vec::new();
        for l in lines.by_ref() {
            if !l.trim().is_empty() && !l.starts_with("     ") {
                break;
            }
            block.push(l);
        }
        assert!(!block.is_empty(), "schema {name} not found");
        let props = block
            .iter()
            .filter(|l| l.starts_with("        ") && !l.starts_with("         "))
            .filter_map(|l| l.trim().strip_suffix(':').map(str::to_owned))
            .collect();
        let at = block
            .iter()
            .position(|l| l.trim_start().starts_with("required:"));
        let mut required = Vec::new();
        if let Some(at) = at {
            let mut text = String::new();
            for l in &block[at..] {
                text.push_str(l);
                text.push(' ');
                if l.contains(']') {
                    break;
                }
            }
            let inner = text.split_once('[').map(|x| x.1).unwrap_or("");
            let inner = inner.split(']').next().unwrap_or("");
            required = inner
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
        (required, props)
    }

    /// The 200 response of `operation_id` must wrap its list in `key`
    /// (`required: [key]` between this operationId and the next).
    fn wrapper_requires(operation_id: &str, key: &str) -> bool {
        const YAML: &str = include_str!("../../../../protocol/openapi.yaml");
        let tag = format!("operationId: {operation_id}");
        let Some(start) = YAML.find(&tag) else {
            return false;
        };
        let rest = &YAML[start + tag.len()..];
        let end = rest.find("operationId:").unwrap_or(rest.len());
        rest[..end].contains(&format!("required: [{key}]"))
    }

    /// Mechanical contract check: every field a View reads is a documented
    /// property of the schema its endpoint returns, and (except the one
    /// documented optional) is `required` so it is always on the wire.
    #[test]
    fn views_only_read_fields_the_contract_defines() {
        // (read, response schema, list operationId when wrapped)
        let table: Vec<(Read, &str, Option<&str>)> = vec![
            (Read::Sessions, "Session", Some("listSessions")),
            (Read::Session { id: "x".into() }, "Session", None),
            (Read::Run { id: "x".into() }, "Run", None),
            (Read::Usage, "Usage", None),
            (
                Read::Memory {
                    q: None,
                    limit: None,
                },
                "MemoryAssertion",
                Some("listMemoryAssertions"),
            ),
            (Read::MemorySettings, "MemorySettings", None),
            (Read::Schedules, "Schedule", Some("listSchedules")),
            (Read::Schedule { id: "x".into() }, "Schedule", None),
        ];
        // Documented-but-optional properties (Usage.cost_usd has a default).
        let optional = [("Usage", "cost_usd")];
        for (read, schema, op) in table {
            let (_, _, view) = request(&read).unwrap();
            let (required, props) = schema_fields(schema);
            assert!(!props.is_empty(), "{schema}: no properties parsed");
            assert!(!view.fields.is_empty());
            for f in view.fields {
                assert!(
                    props.iter().any(|p| p == f),
                    "{schema} has no property `{f}`"
                );
                assert!(
                    required.iter().any(|r| r == f) || optional.contains(&(schema, *f)),
                    "{schema}.{f} is not required"
                );
            }
            match (view.list_key, op) {
                (Some(key), Some(op)) => {
                    assert!(wrapper_requires(op, key), "{op} does not wrap in `{key}`")
                }
                (None, None) => {}
                other => panic!("list/wrapper mismatch: {other:?}"),
            }
        }
    }

    #[test]
    fn schema_parser_has_a_positive_control() {
        let (required, props) = schema_fields("MemoryAssertion");
        assert!(required.iter().any(|r| r == "text"));
        assert!(props.iter().any(|p| p == "recorded_at"));
        assert!(!props.iter().any(|p| p == "subject"));
        assert!(wrapper_requires("listMemoryAssertions", "assertions"));
        assert!(!wrapper_requires("listMemoryAssertions", "items"));
    }

    #[test]
    fn blank_id_is_rejected() {
        for read in [
            Read::Session { id: "  ".into() },
            Read::ModuleApproval { id: " ".into() },
        ] {
            let err = request(&read).unwrap_err();
            assert!(err.contains("blank"), "{err}");
        }
    }

    #[test]
    fn timings_summary_rejects_list_only_filters() {
        let err = request(&Read::Timings {
            summary: true,
            source: Some("chat".into()),
            since: None,
            limit: None,
        })
        .unwrap_err();
        assert!(err.contains("--summary"), "{err}");
        let err = request(&Read::Timings {
            summary: true,
            source: None,
            since: None,
            limit: Some(1),
        })
        .unwrap_err();
        assert!(err.contains("--summary"), "{err}");
    }

    #[tokio::test]
    async fn daemon_error_status_is_returned_with_the_body() {
        let (port, _) = stub("404 Not Found", r#"{"error":"not_found"}"#);
        let err = execute(&endpoint(port), &Read::Usage, true)
            .await
            .unwrap_err();
        assert!(err.contains("404"), "{err}");
        assert!(err.contains("not_found"), "{err}");
    }
}
