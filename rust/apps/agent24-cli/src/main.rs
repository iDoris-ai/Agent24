//! agent24 — CLI for the Agent24 daemon (B6 skeleton).
//!
//! Two connection modes:
//! - Attached: a running agent24d is discovered via ~/.agent24/daemon.json
//! - Standalone: no daemon found → spawn an ephemeral agent24d for this
//!   invocation and terminate it afterwards

use std::process::Stdio;
use std::time::Duration;

use agent24_protocol::state_file::{self, DaemonState};
use agent24_protocol::{ChatMessage, ChatRequest, ChatResponse, Health};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(
    name = "agent24",
    version,
    about = "Agent24 CLI — 24/7 personal agent daemon"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// One-shot chat with the agent
    Chat {
        /// The message to send
        message: String,
        /// Model id override
        #[arg(long)]
        model: Option<String>,
    },
    /// List models known to the daemon
    Models,
    /// Manage the daemon process
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start agent24d in the background (no-op if already running)
    Start,
    /// Show daemon status
    Status,
    /// Stop the running daemon
    Stop,
}

struct Endpoint {
    base: String,
    token: String,
    /// Ephemeral child to terminate when the CLI exits (standalone mode)
    child: Option<tokio::process::Child>,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_default()
}

async fn health_ok(base: &str, token: &str) -> bool {
    let req = client().get(format!("{base}/api/v1/health"));
    let req = if token.is_empty() {
        req
    } else {
        req.bearer_auth(token)
    };
    matches!(
        req.timeout(Duration::from_secs(3)).send().await,
        Ok(r) if r.status().is_success()
    )
}

fn agent24d_binary() -> String {
    if let Some(bin) = std::env::var_os("AGENT24D_BIN") {
        return bin.to_string_lossy().into_owned();
    }
    // Default: agent24d next to this binary (release layout); dev fallback PATH
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("agent24d")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "agent24d".to_owned())
}

async fn spawn_daemon(ephemeral: bool) -> Result<(DaemonState, tokio::process::Child), String> {
    let bin = agent24d_binary();
    let mut cmd = tokio::process::Command::new(&bin);
    let mut args = vec!["serve", "--port", "0"];
    if ephemeral {
        // Private instance: no singleton lock, no discovery file
        args.push("--ephemeral");
    }
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Ephemeral children die with the CLI no matter which return path runs
        // (early ? returns and panics included; SIGKILL of the CLI is the one
        // exception)
        .kill_on_drop(ephemeral);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn {bin}: {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout from agent24d")?;
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let next = tokio::time::timeout_at(deadline, lines.next_line()).await;
        let line = match next {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => return Err("agent24d exited before ready line".to_owned()),
            Ok(Err(e)) => return Err(format!("reading agent24d stdout: {e}")),
            Err(_) => return Err("agent24d did not become ready within 15s".to_owned()),
        };
        if let Ok(state) = serde_json::from_str::<serde_json::Value>(&line)
            && state["type"] == "ready"
        {
            let port = state["port"].as_u64().unwrap_or(0) as u16;
            let token = state["token"].as_str().unwrap_or("").to_owned();
            let pid = child.id().unwrap_or(0);
            return Ok((
                DaemonState {
                    port,
                    token,
                    pid,
                    version: state["version"].as_str().unwrap_or("").to_owned(),
                },
                child,
            ));
        }
    }
}

/// Attached if a live daemon is discoverable and healthy; standalone otherwise.
async fn connect() -> Result<Endpoint, String> {
    if let Some(state) = state_file::read_live() {
        let base = format!("http://127.0.0.1:{}", state.port);
        if health_ok(&base, &state.token).await {
            return Ok(Endpoint {
                base,
                token: state.token,
                child: None,
            });
        }
    }
    let (state, child) = spawn_daemon(true).await?;
    let base = format!("http://127.0.0.1:{}", state.port);
    Ok(Endpoint {
        base,
        token: state.token,
        child: Some(child),
    })
}

async fn finish(mut ep: Endpoint) {
    if let Some(child) = ep.child.as_mut() {
        let _ = child.kill().await;
    }
}

fn bearer(ep: &Endpoint, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if ep.token.is_empty() {
        rb
    } else {
        rb.bearer_auth(&ep.token)
    }
}

async fn cmd_chat(message: String, model: Option<String>) -> Result<(), String> {
    let ep = connect().await?;
    let req = ChatRequest {
        messages: vec![ChatMessage {
            role: "user".to_owned(),
            content: message,
        }],
        model,
    };
    let result = bearer(&ep, client().post(format!("{}/api/v1/chat", ep.base)))
        .timeout(Duration::from_secs(180))
        .json(&req)
        .send()
        .await;
    let out = match result {
        Ok(res) if res.status().is_success() => {
            let body: ChatResponse = res.json().await.map_err(|e| e.to_string())?;
            println!("{}", body.message.content);
            println!("· {} tokens", body.usage.total_tokens);
            Ok(())
        }
        Ok(res) => {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            Err(format!("daemon returned {status}: {body}"))
        }
        Err(e) => Err(e.to_string()),
    };
    finish(ep).await;
    out
}

async fn cmd_models() -> Result<(), String> {
    let ep = connect().await?;
    let result = bearer(&ep, client().get(format!("{}/api/v1/models", ep.base)))
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    let out = match result {
        Ok(res) if res.status().is_success() => {
            let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
            let models = body["models"].as_array().cloned().unwrap_or_default();
            if models.is_empty() {
                println!("(no models — is a local LLM runtime running?)");
            }
            for m in models {
                println!(
                    "{}  [{} · {}{}]",
                    m["id"].as_str().unwrap_or("?"),
                    m["provider"].as_str().unwrap_or("?"),
                    m["tier"].as_str().unwrap_or("?"),
                    if m["loaded"].as_bool().unwrap_or(false) {
                        " · loaded"
                    } else {
                        ""
                    },
                );
            }
            Ok(())
        }
        Ok(res) => Err(format!("daemon returned {}", res.status())),
        Err(e) => Err(e.to_string()),
    };
    finish(ep).await;
    out
}

async fn cmd_daemon(action: DaemonAction) -> Result<(), String> {
    match action {
        DaemonAction::Start => {
            if let Some(state) = state_file::read_live() {
                let base = format!("http://127.0.0.1:{}", state.port);
                if health_ok(&base, &state.token).await {
                    println!(
                        "daemon already running (pid {}, port {})",
                        state.pid, state.port
                    );
                    return Ok(());
                }
            }
            let (state, child) = match spawn_daemon(false).await {
                Ok(v) => v,
                Err(err) => {
                    // Lost a concurrent-start race? The winner holds the
                    // singleton lock and our child exited before ready. The
                    // winner may still be booting — poll briefly for its
                    // state file before giving up.
                    for _ in 0..30 {
                        if let Some(state) = state_file::read_live() {
                            let base = format!("http://127.0.0.1:{}", state.port);
                            if health_ok(&base, &state.token).await {
                                println!(
                                    "daemon already running (pid {}, port {})",
                                    state.pid, state.port
                                );
                                return Ok(());
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    return Err(err);
                }
            };
            // Detach: without kill_on_drop, dropping the handle leaves the
            // daemon running (same session — production autostart is F1's
            // launchd/systemd job; this is the dev/manual path).
            drop(child);
            println!("daemon started (pid {}, port {})", state.pid, state.port);
            Ok(())
        }
        DaemonAction::Status => match state_file::read_live() {
            Some(state) => {
                let base = format!("http://127.0.0.1:{}", state.port);
                if health_ok(&base, &state.token).await {
                    let res = client()
                        .get(format!("{base}/api/v1/health"))
                        .bearer_auth(&state.token)
                        .send()
                        .await
                        .map_err(|e| e.to_string())?;
                    let health: Health = res.json().await.map_err(|e| e.to_string())?;
                    println!(
                        "running · pid {} · port {} · backend {} · v{}",
                        state.pid, state.port, health.backend, health.version
                    );
                } else {
                    println!(
                        "state file present (pid {}) but daemon not responding",
                        state.pid
                    );
                }
                Ok(())
            }
            None => {
                println!("not running");
                Ok(())
            }
        },
        DaemonAction::Stop => match state_file::read_live() {
            Some(state) => {
                // Authenticated shutdown: the bearer token proves this is OUR
                // daemon — a reused pid of an unrelated process can never be
                // hit (review B6)
                let base = format!("http://127.0.0.1:{}", state.port);
                let res = client()
                    .post(format!("{base}/api/v1/shutdown"))
                    .bearer_auth(&state.token)
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await;
                match res {
                    Ok(r) if r.status().is_success() => {
                        println!(
                            "shutdown requested (pid {}, port {})",
                            state.pid, state.port
                        );
                        Ok(())
                    }
                    Ok(r) => Err(format!("daemon refused shutdown: {}", r.status())),
                    Err(_) => Err(format!(
                        "daemon not responding on port {} — if it is truly gone, remove ~/.agent24/daemon.json",
                        state.port
                    )),
                }
            }
            None => {
                println!("not running");
                Ok(())
            }
        },
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Chat { message, model } => cmd_chat(message, model).await,
        Command::Models => cmd_models().await,
        Command::Daemon { action } => cmd_daemon(action).await,
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
