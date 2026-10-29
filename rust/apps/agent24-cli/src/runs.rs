//! `agent24 run …` (alias `runs`), list/show/cancel: the run operations the desktop app has
//! (`apps/desktop/src/renderer/pages/agent/api.ts`: list, get, cancel), as REST
//! calls to the same daemon routes, so a CLI use case exercises what the
//! desktop does. Like `agent24 os`, it only uses a resident daemon: an
//! ephemeral one has an in-memory store, so a list would be empty.

use std::time::Duration;

use agent24_protocol::state_file;
use agent24_protocol::{Run, RunStatus};
use clap::Subcommand;

use super::{Endpoint, attach_only, bearer, client, daemon_error_line, finish};

#[derive(Subcommand)]
pub enum RunsAction {
    /// List runs (`GET /api/v1/runs`)
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one run, with its output or error
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Cancel a queued, running or waiting run
    Cancel { id: String },
}

fn wire_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "?".to_owned())
}

/// The first line of `text`, cut to `max` characters.
pub(super) fn short(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() > max || text.lines().nth(1).is_some() {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_owned()
    }
}

fn run_line(run: &Run) -> String {
    let mut line = format!("{}  {}  {}", run.id, wire_name(&run.status), run.created_at);
    if let Some(schedule) = &run.schedule_id {
        line.push_str(&format!("  schedule {schedule}"));
    }
    line.push_str(&format!("  {:?}", short(&run.input.prompt, 60)));
    line
}

fn run_detail(run: &Run) -> Vec<String> {
    let mut lines = vec![
        format!("{}  {}", run.id, wire_name(&run.status)),
        format!("  prompt: {}", run.input.prompt),
        format!("  created {}", run.created_at),
    ];
    if let Some(session) = &run.session_id {
        lines.push(format!("  session {session}"));
    }
    if let Some(schedule) = &run.schedule_id {
        lines.push(format!("  schedule {schedule}"));
    }
    if let Some(ended) = &run.ended_at {
        lines.push(format!("  ended {ended}"));
    }
    if let Some(output) = &run.output {
        lines.push(format!("  output: {}", output.text));
    }
    if let Some(error) = &run.error {
        lines.push(format!("  error {}: {}", error.code, error.message));
    }
    lines
}

/// The daemon answers a cancel with the run as it is now: a run that had
/// already ended stays as it was.
fn cancel_line(run: &Run) -> String {
    match run.status {
        RunStatus::Cancelled => format!("cancelled {}", run.id),
        RunStatus::Completed | RunStatus::Failed => format!(
            "{} had already ended ({}); nothing to cancel",
            run.id,
            wire_name(&run.status)
        ),
        _ => format!(
            "cancel requested for {}; it is {} now",
            run.id,
            wire_name(&run.status)
        ),
    }
}

pub(super) async fn resident() -> Result<Endpoint, String> {
    if let Some(ep) = attach_only().await? {
        return Ok(ep);
    }
    // `attach_only` also says no to a daemon that is running but keeps host
    // authority off daemon.json; telling that user to start one would mislead.
    match state_file::read_live() {
        Some(state) if state.auth_mode.is_capabilities() => {
            Err(super::HOST_AUTHORITY_UNAVAILABLE.to_owned())
        }
        _ => Err("daemon not running; start it with `agent24 daemon start`".to_owned()),
    }
}

/// `id` as one path segment. The daemon's ids are a prefix and a ULID
/// (`run_…`, `sch_…`); anything else could step out of the route (`..`, `/`,
/// `?`, `#`) and is refused before a request is sent.
pub(super) fn segment(id: &str) -> Result<&str, String> {
    let ok = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then_some(id).ok_or_else(|| format!("not an id: {id:?}"))
}

/// Sends `req`; the body as JSON on success, the daemon's own message otherwise.
pub(super) async fn send(req: reqwest::RequestBuilder) -> Result<serde_json::Value, String> {
    let res = req
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    if status == reqwest::StatusCode::NO_CONTENT {
        return Ok(serde_json::Value::Null);
    }
    if status.is_success() {
        return res
            .json()
            .await
            .map_err(|e| format!("unreadable answer from the daemon: {e}"));
    }
    let body: serde_json::Value = res.json().await.unwrap_or_default();
    if body["error"]["code"] == "not_found" {
        // The daemon's message is just the kind and the id ("run run_…").
        Err(format!("not found: {}", daemon_error_line(&body)))
    } else if body["error"]["message"].as_str().is_some() {
        Err(daemon_error_line(&body))
    } else {
        Err(format!("daemon returned {status}"))
    }
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("unexpected answer from the daemon: {e}"))
}

pub(super) fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

pub(crate) async fn cmd_runs(action: RunsAction) -> Result<(), String> {
    let ep = resident().await?;
    let out = runs(&ep, action).await;
    finish(ep).await;
    println!("{}", out?);
    Ok(())
}

/// What to print for `action`.
async fn runs(ep: &Endpoint, action: RunsAction) -> Result<String, String> {
    let base = &ep.base;
    Ok(match action {
        RunsAction::List { json } => {
            let body = send(bearer(ep, client().get(format!("{base}/api/v1/runs")))).await?;
            let list: Vec<Run> = decode(body["runs"].clone())?;
            if json {
                pretty(&body)
            } else if list.is_empty() {
                "(no runs)".to_owned()
            } else {
                list.iter().map(run_line).collect::<Vec<_>>().join("\n")
            }
        }
        RunsAction::Show { id, json } => {
            let url = format!("{base}/api/v1/runs/{}", segment(&id)?);
            let body = send(bearer(ep, client().get(url))).await?;
            let run: Run = decode(body.clone())?;
            if json {
                pretty(&body)
            } else {
                run_detail(&run).join("\n")
            }
        }
        RunsAction::Cancel { id } => {
            let url = format!("{base}/api/v1/runs/{}/cancel", segment(&id)?);
            let run: Run = decode(send(bearer(ep, client().post(url))).await?)?;
            cancel_line(&run)
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::io::{Read, Write};

    use super::*;

    fn run_json(status: &str, prompt: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "run_1", "session_id": null, "workspace_id": null, "status": status,
            "input": {"prompt": prompt, "workspace_id": null, "model_override": null, "mode": "normal"},
            "output": null,
            "error": {"code": "provider_unavailable", "message": "no provider"},
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0, "cost_usd": 0.0},
            "schedule_id": "sch_1", "created_at": "2026-10-26T00:00:00Z",
            "started_at": null, "ended_at": "2026-10-26T00:00:01Z"
        })
    }

    /// A daemon that answers each request in turn with the next canned
    /// answer; the requests it got (head and body) come back through the handle.
    pub(crate) fn answer_each(
        replies: Vec<(&'static str, String)>,
    ) -> (Endpoint, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in replies {
                let Some(mut sock) = accept(&listener) else {
                    break;
                };
                seen.push(read_request(&mut sock));
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        let ep = Endpoint {
            base,
            token: "tok".to_owned(),
            child: None,
            session: None,
        };
        (ep, seen)
    }

    /// The next connection, or none after 5 s: a client that sends fewer
    /// requests than there are replies fails its test instead of hanging it.
    fn accept(listener: &std::net::TcpListener) -> Option<std::net::TcpStream> {
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((sock, _)) => {
                    sock.set_nonblocking(false).unwrap();
                    return Some(sock);
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => return None,
            }
        }
    }

    /// The head, then as many body bytes as its content-length says.
    fn read_request(sock: &mut std::net::TcpStream) -> String {
        let mut got = Vec::new();
        let mut buf = [0u8; 1024];
        let head_end = loop {
            if let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            let n = sock.read(&mut buf).unwrap();
            assert!(n > 0, "the client closed before its request ended");
            got.extend_from_slice(&buf[..n]);
        };
        let head = String::from_utf8_lossy(&got[..head_end]).to_ascii_lowercase();
        let length: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .map_or(0, |v| v.trim().parse().unwrap());
        while got.len() < head_end + length {
            let n = sock.read(&mut buf).unwrap();
            assert!(n > 0, "the client closed before its body ended");
            got.extend_from_slice(&buf[..n]);
        }
        String::from_utf8_lossy(&got).into_owned()
    }

    fn answer_once(
        status: &'static str,
        body: String,
    ) -> (Endpoint, std::thread::JoinHandle<String>) {
        let (ep, seen) = answer_each(vec![(status, body)]);
        let one = std::thread::spawn(move || seen.join().unwrap().remove(0));
        (ep, one)
    }

    #[tokio::test]
    async fn list_reads_the_runs_envelope_with_the_bearer() {
        let body = serde_json::json!({ "runs": [run_json("failed", "hi")] }).to_string();
        let (ep, seen) = answer_once("200 OK", body);
        let out = runs(&ep, RunsAction::List { json: false }).await.unwrap();
        assert!(out.starts_with("run_1  failed"), "{out}");
        let seen = seen.join().unwrap().to_ascii_lowercase();
        assert!(seen.starts_with("get /api/v1/runs http/1.1"), "{seen}");
        assert!(seen.contains("authorization: bearer tok"), "{seen}");
    }

    #[tokio::test]
    async fn an_answer_of_the_wrong_shape_fails_even_as_json() {
        for json in [false, true] {
            let (ep, _seen) = answer_once("200 OK", r#"{"items":[]}"#.to_owned());
            assert!(
                runs(&ep, RunsAction::List { json }).await.is_err(),
                "json {json}"
            );
        }
        let (ep, _seen) = answer_once("200 OK", "not json".to_owned());
        let err = runs(&ep, RunsAction::List { json: true })
            .await
            .unwrap_err();
        assert!(err.contains("unreadable"), "{err}");
    }

    #[tokio::test]
    async fn cancel_takes_the_202_and_says_what_the_run_is_now() {
        let (ep, seen) = answer_once("202 Accepted", run_json("cancelled", "hi").to_string());
        let id = "run_1".to_owned();
        assert_eq!(
            runs(&ep, RunsAction::Cancel { id }).await.unwrap(),
            "cancelled run_1"
        );
        let seen = seen.join().unwrap();
        assert!(
            seen.starts_with("POST /api/v1/runs/run_1/cancel HTTP/1.1"),
            "{seen}"
        );
    }

    #[tokio::test]
    async fn the_daemon_s_refusal_is_the_error() {
        let body = r#"{"error":{"code":"not_found","message":"run run_x"}}"#.to_owned();
        let (ep, _seen) = answer_once("404 Not Found", body);
        let err = runs(
            &ep,
            RunsAction::Show {
                id: "run_x".to_owned(),
                json: true,
            },
        )
        .await;
        assert_eq!(err.unwrap_err(), "not found: run run_x");
        let body = r#"{"error":{"code":"conflict","message":"m","hint":"h"}}"#.to_owned();
        let (ep, _seen) = answer_once("409 Conflict", body);
        let err = runs(
            &ep,
            RunsAction::Cancel {
                id: "run_x".to_owned(),
            },
        )
        .await;
        assert_eq!(err.unwrap_err(), "m\n  h");
        let (ep, _seen) = answer_once("500 Internal Server Error", String::new());
        let err = runs(&ep, RunsAction::List { json: false }).await;
        assert!(err.unwrap_err().contains("500"));
    }

    #[tokio::test]
    async fn an_id_that_could_leave_the_route_is_refused_before_any_request() {
        // Nothing listens here: a request that went out would fail differently.
        let ep = Endpoint {
            base: "http://127.0.0.1:9".to_owned(),
            token: String::new(),
            child: None,
            session: None,
        };
        for id in ["../shutdown#", "a/b", "a?b", "run_1#x", "", "run 1"] {
            let show = runs(
                &ep,
                RunsAction::Show {
                    id: id.to_owned(),
                    json: false,
                },
            )
            .await;
            assert!(show.unwrap_err().starts_with("not an id"), "{id:?}");
            let cancel = runs(&ep, RunsAction::Cancel { id: id.to_owned() }).await;
            assert!(cancel.unwrap_err().starts_with("not an id"), "{id:?}");
        }
    }

    #[test]
    fn a_run_line_names_its_schedule_and_cuts_the_prompt() {
        let prompt = format!("{}\nsecond line", "p".repeat(70));
        let run: Run = serde_json::from_value(run_json("failed", &prompt)).unwrap();
        let line = run_line(&run);
        assert!(
            line.starts_with("run_1  failed  2026-10-26T00:00:00Z  schedule sch_1"),
            "{line}"
        );
        assert!(line.contains(&format!("{}…", "p".repeat(60))), "{line}");
        assert!(!line.contains("second line"), "{line}");
        let detail = run_detail(&run).join("\n");
        assert!(
            detail.contains("error provider_unavailable: no provider"),
            "{detail}"
        );
        assert!(
            detail.contains("second line"),
            "show prints the whole prompt: {detail}"
        );
        assert_eq!(short(&"p".repeat(60), 60), "p".repeat(60));
        // A short first line still says there is more after it.
        assert_eq!(short("hi\nmore", 60), "hi…");
    }

    #[test]
    fn a_cancel_says_whether_it_stopped_anything() {
        let line = |status| cancel_line(&serde_json::from_value(run_json(status, "hi")).unwrap());
        assert_eq!(line("cancelled"), "cancelled run_1");
        assert_eq!(
            line("failed"),
            "run_1 had already ended (failed); nothing to cancel"
        );
        assert_eq!(
            line("completed"),
            "run_1 had already ended (completed); nothing to cancel"
        );
        assert_eq!(
            line("running"),
            "cancel requested for run_1; it is running now"
        );
    }
}
