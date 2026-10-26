//! `agent24 runs …`: the run operations the desktop app has
//! (`apps/desktop/src/renderer/pages/agent/api.ts`: list, get, cancel), as REST
//! calls to the same daemon routes, so a CLI use case exercises what the
//! desktop does. Like `agent24 os`, it only uses a resident daemon: an
//! ephemeral one has an in-memory store, so a list would be empty.

use std::time::Duration;

use agent24_protocol::{Run, RunStatus};
use clap::Subcommand;

use super::{Endpoint, attach_only, bearer, client, daemon_error_line, finish};

#[derive(Subcommand)]
pub(crate) enum RunsAction {
    /// List runs, newest first (`GET /api/v1/runs`)
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
fn short(text: &str, max: usize) -> String {
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

async fn resident() -> Result<Endpoint, String> {
    attach_only()
        .await
        .ok_or_else(|| "daemon not running; start it with `agent24 daemon start`".to_owned())
}

/// Sends `req`; the body as JSON on success, the daemon's own message otherwise.
async fn send(req: reqwest::RequestBuilder) -> Result<serde_json::Value, String> {
    let res = req
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let body: serde_json::Value = res.json().await.unwrap_or_default();
    if status.is_success() {
        Ok(body)
    } else if body["error"]["code"] == "not_found" {
        // The daemon's message is just the kind and the id ("run run_…").
        Err(format!("not found: {}", daemon_error_line(&body)))
    } else if body["error"]["message"].as_str().is_some() {
        Err(daemon_error_line(&body))
    } else {
        Err(format!("daemon returned {status}"))
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("unexpected answer from the daemon: {e}"))
}

fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

pub(crate) async fn cmd_runs(action: RunsAction) -> Result<(), String> {
    let ep = resident().await?;
    let out = runs(&ep, action).await;
    finish(ep).await;
    out
}

async fn runs(ep: &Endpoint, action: RunsAction) -> Result<(), String> {
    let base = &ep.base;
    match action {
        RunsAction::List { json } => {
            let body = send(bearer(ep, client().get(format!("{base}/api/v1/runs")))).await?;
            if json {
                print_json(&body);
                return Ok(());
            }
            let list: Vec<Run> = decode(body["runs"].clone())?;
            if list.is_empty() {
                println!("(no runs)");
            }
            for run in &list {
                println!("{}", run_line(run));
            }
        }
        RunsAction::Show { id, json } => {
            let body = send(bearer(ep, client().get(format!("{base}/api/v1/runs/{id}")))).await?;
            if json {
                print_json(&body);
                return Ok(());
            }
            for line in run_detail(&decode(body)?) {
                println!("{line}");
            }
        }
        RunsAction::Cancel { id } => {
            let req = client().post(format!("{base}/api/v1/runs/{id}/cancel"));
            let run: Run = decode(send(bearer(ep, req)).await?)?;
            println!("{}", cancel_line(&run));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_run_line_names_its_schedule_and_cuts_the_prompt() {
        let run: Run = serde_json::from_value(serde_json::json!({
            "id": "run_1", "session_id": null, "workspace_id": null, "status": "failed",
            "input": {"prompt": format!("{}\nsecond line", "p".repeat(70)), "workspace_id": null,
                      "model_override": null, "mode": "normal"},
            "output": null,
            "error": {"code": "provider_unavailable", "message": "no provider"},
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0, "cost_usd": 0.0},
            "schedule_id": "sch_1", "created_at": "2026-10-26T00:00:00Z",
            "started_at": null, "ended_at": "2026-10-26T00:00:01Z"
        }))
        .unwrap();
        let line = run_line(&run);
        assert!(
            line.starts_with("run_1  failed  2026-10-26T00:00:00Z  schedule sch_1"),
            "{line}"
        );
        assert!(line.contains(&format!("{}…", "p".repeat(60))), "{line}");
        assert!(!line.contains("second line"), "{line}");
        assert_eq!(
            cancel_line(&run),
            "run_1 had already ended (failed); nothing to cancel"
        );
        let detail = run_detail(&run).join("\n");
        assert!(
            detail.contains("error provider_unavailable: no provider"),
            "{detail}"
        );
        assert!(
            detail.contains("second line"),
            "show prints the whole prompt: {detail}"
        );
    }
}
