//! TUI-2a：daemon 的只读 GET。这一批是 session / run / memory / schedule / usage。
//! timings、consent、approval、tools、decide profile 留到 TUI-2a2。
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
    if let Read::Session { id } | Read::Run { id } | Read::Schedule { id } = read {
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
                View::rows("assertions", &["id", "subject", "predicate", "object"]),
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
        r#"{"sessions":[{"id":"s1","title":"hello","channel":"cli","updated_at":"t"}]}"#,
        "s1"
    );
    case!(
        session_get_encodes_the_path_segment,
        Read::Session { id: "a/b c".into() },
        "GET /api/v1/sessions/a%2Fb%20c HTTP/1.1",
        r#"{"id":"a/b c","title":"hello","channel":"cli"}"#,
        "hello"
    );
    case!(
        run_get_gets_one_run,
        Read::Run { id: "run_1".into() },
        "GET /api/v1/runs/run_1 HTTP/1.1",
        r#"{"id":"run_1","status":"completed","session_id":"s1","created_at":"t"}"#,
        "run_1"
    );
    case!(
        memory_list_sends_q_and_limit,
        Read::Memory {
            q: Some("hello world".into()),
            limit: Some(3)
        },
        "GET /api/v1/memory/assertions?q=hello%20world&limit=3 HTTP/1.1",
        r#"{"assertions":[{"id":"m1","subject":"sky","predicate":"is","object":"blue"}]}"#,
        "sky"
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
        r#"{"schedules":[{"id":"sch_1","name":"digest","enabled":true,"effective_enabled":true}]}"#,
        "digest"
    );
    case!(
        schedule_get_gets_one_schedule,
        Read::Schedule { id: "sch_1".into() },
        "GET /api/v1/schedules/sch_1 HTTP/1.1",
        r#"{"id":"sch_1","name":"digest","enabled":false,"next_run_at":null}"#,
        "digest"
    );
    case!(
        usage_gets_token_totals,
        Read::Usage,
        "GET /api/v1/usage HTTP/1.1",
        r#"{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7,"cost_usd":0.1}"#,
        "total_tokens: 7"
    );

    #[test]
    fn blank_id_is_rejected() {
        let err = request(&Read::Session { id: "  ".into() }).unwrap_err();
        assert!(err.contains("blank"), "{err}");
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
