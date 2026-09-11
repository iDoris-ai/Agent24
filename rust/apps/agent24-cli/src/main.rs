//! agent24 — CLI for the Agent24 daemon (B6 skeleton).
//!
//! Two connection modes:
//! - Attached: a running agent24d is discovered via ~/.agent24/daemon.json
//! - Standalone: no daemon found → spawn an ephemeral agent24d for this
//!   invocation and terminate it afterwards

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use agent24_protocol::state_file::{self, DaemonState};
use agent24_protocol::{ChatMessage, ChatRequest, ChatResponse, Health};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, BufReader};

mod service;
mod tui;

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
    /// Install/remove 24/7 unattended operation (macOS LaunchAgent)
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Manage the daemon process
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Launch the terminal UI (runs · events · approval queue)
    Tui,
    /// Inspect and toggle the domain OSes this daemon provides
    Os {
        #[command(subcommand)]
        action: OsAction,
    },
    /// Serve agent24d as an MCP server over stdio, so an external MCP client
    /// (Claude Desktop, another agent) can run tasks on it and introspect it.
    /// Risky actions are still approved on THIS host, never by the caller (E4).
    Mcp,
}

#[derive(Subcommand)]
enum OsAction {
    /// Show every domain OS the daemon knows about, and what it did with each
    List,
    // EXPIRES WHEN `os disable` ACTS ON A RUNNING MODULE — which is NOT the day
    // ME-3b-5 landed. Both "applies at the next daemon start" lines below are true
    // for one reason only: a toggle is written to the config and nothing acts on a
    // running module, so a restart is the only path by which it takes effect.
    //
    // This note used to say it expired with ME-3b's last slice. ME-3b-5 delivered
    // the two-phase stop as a library (`agent24_os_proto::drain::Generation`), but
    // the daemon still refuses to mount an out-of-process package at all
    // (`domain.rs`, `is_mountable_in_process`), so there is no running module for
    // a disable to act on and these lines are still true. Rewording them then
    // would have made a true sentence false.
    //
    // The event that DOES make them false: the daemon's disable path calling
    // `Generation::begin_drain`. The test
    // `an_out_of_process_manifest_is_refused_not_half_mounted` has to change the
    // same day — that is the tripwire, since no test can assert what a help
    // string promises. The note lives here rather than in the follow-ups ledger
    // so that it is read by whoever changes the thing that makes it false.
    /// Turn one on (applies at the next daemon start)
    Enable {
        /// Module name, e.g. sin90
        name: String,
    },
    /// Turn one off (applies at the next daemon start)
    Disable { name: String },
    /// Install a domain-OS package directory (takes effect at the next daemon start)
    ///
    /// Unlike list/enable/disable this does NOT go through the daemon, and does not
    /// need one running. Installing writes files; the daemon reads them when it
    /// starts. Routing it through the daemon would make writing depend on someone
    /// reading — and would mean you cannot install a module while the daemon is
    /// down, which is exactly when you are most likely to be fixing one.
    Install {
        /// Directory containing `domain-os.yml`
        path: PathBuf,
    },
    /// Remove an installed domain-OS package (takes effect at the next daemon start)
    Uninstall { name: String },
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

#[derive(Subcommand)]
enum ServiceAction {
    /// Install the LaunchAgent so the daemon starts at login and self-heals
    Install,
    /// Stop and remove the LaunchAgent
    Uninstall,
    /// Show whether 24/7 operation is installed and loaded
    Status,
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

/// Serve agent24d as an MCP server over stdio (E4). Attaches to the running
/// daemon (or a private ephemeral one) and proxies a curated, host-gated surface
/// to it. Runs until the MCP client closes stdin.
async fn cmd_mcp() -> Result<(), String> {
    let ep = connect().await?;
    let result = agent24_mcp::server::Agent24Server::new(ep.base.clone(), ep.token.clone())
        .serve_stdio()
        .await
        .map_err(|e| e.to_string());
    finish(ep).await;
    result
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

/// `agent24 os install` / `os uninstall` — the two that do NOT go through the
/// daemon.
///
/// Installing writes files into the packages root; the daemon reads them when it
/// next starts. Making this an RPC would make writing depend on someone reading,
/// and would mean a module cannot be installed or removed while the daemon is
/// down — which is exactly when an operator is most likely to be fixing one.
///
/// Every decision lives in `agent24-os-packages`: which directory to write to,
/// what the installed name is (the manifest's, not the source directory's), and
/// whether one is already there. This function maps arguments onto that and
/// prints the result. If it ever needs to compute a path or check for a duplicate
/// itself, the seam is in the wrong place and that logic belongs in the library.
fn os_local(action: &OsAction) -> Option<Result<(), String>> {
    // The state dir is OPTIONAL here, and passing it as an option rather than
    // resolving it first is the whole difference: `A24_OS_PACKAGES` is consulted
    // before it, so a container or CI runner with the override set and no `HOME`
    // installs into the directory it asked for. Resolving `state_dir()` first
    // reimposed the `HOME` requirement that the override exists to lift, and
    // reported it with a message identical to the one for "neither is set" —
    // indistinguishable outputs for two situations, one of which was wrong.
    let root = match agent24_os_packages::resolve_packages_root(
        agent24_os_packages::env_override().as_deref(),
        state_file::state_dir().as_deref(),
        false,
    ) {
        Ok(root) => root,
        Err(e) => {
            return Some(Err(format!(
                "{e} (set HOME, or set {})",
                agent24_os_packages::PACKAGES_ROOT_ENV
            )));
        }
    };
    match action {
        OsAction::Install { path } => Some(
            agent24_os_packages::install::install(path, &root)
                .map(|dest| {
                    println!("installed {}", dest.display());
                    println!("  it takes effect at the next daemon start: agent24 daemon stop && agent24 daemon start");
                    // Said out loud because the isolation is deliberate and
                    // therefore permanent: an ephemeral daemon (`agent24 chat`
                    // with nothing running) resolves a different packages root
                    // and will never see this package. Without this line the two
                    // lines above are, for that user, a promise that never comes
                    // true.
                    println!("  (a daemon started implicitly by `agent24 chat` does NOT read installed packages)");
                })
                .map_err(|e| e.to_string()),
        ),
        OsAction::Uninstall { name } => Some(
            agent24_os_packages::install::uninstall(name, &root)
                .map(|removed| {
                    if removed {
                        println!("removed {name}");
                        println!("  it takes effect at the next daemon start");
                    } else {
                        // Not an error: the end state the operator asked for is the
                        // one they have. Saying so beats a failure they must decide
                        // to ignore.
                        println!("{name} was not installed; nothing to remove");
                    }
                })
                .map_err(|e| e.to_string()),
        ),
        _ => None,
    }
}

/// `agent24 os` — read and toggle the domain-OS registry.
///
/// `list` / `enable` / `disable` go through the daemon; this never touches
/// `os.json`. That is what makes `agent24 os disable sin09` fail HERE, naming the
/// modules that do exist, instead of writing a file that breaks the registry at
/// the next start. `install` / `uninstall` are different in kind and are handled
/// by [`os_local`] before any of that.
async fn cmd_os(action: OsAction) -> Result<(), String> {
    if let Some(done) = os_local(&action) {
        return done;
    }
    let ep = match connect().await {
        Ok(ep) => ep,
        // The bootstrapping case, and it is the one that matters most: if a domain
        // OS is what keeps the daemon from starting, "ask the daemon to disable it"
        // is exactly the advice that cannot work. `os.json` is plain JSON and
        // nothing stops the user editing it — so say that, with the edit spelled
        // out, rather than leaving them stuck behind a tool that requires the very
        // thing that is broken.
        Err(e) => {
            let path = agent24_protocol::state_file::state_dir()
                .map(|d| d.join("os.json").display().to_string())
                .unwrap_or_else(|| "~/.agent24/os.json".to_owned());
            return Err(format!(
                "{e}\n  this command goes through the daemon, which owns os.json. \
                 If a domain OS is what stops the daemon starting, {}",
                offline_hint(&path, &action)
            ));
        }
    };
    let req = match &action {
        // Handled before the daemon lookup above; see `os_local`.
        OsAction::Install { .. } | OsAction::Uninstall { .. } => unreachable!(),
        OsAction::List => bearer(&ep, client().get(format!("{}/api/v1/os", ep.base))),
        OsAction::Enable { name } | OsAction::Disable { name } => {
            let enabled = matches!(action, OsAction::Enable { .. });
            bearer(&ep, client().patch(format!("{}/api/v1/os/{name}", ep.base)))
                .json(&agent24_protocol::DomainOsUpdate { enabled })
        }
    };
    let out = match req.timeout(Duration::from_secs(10)).send().await {
        Ok(res) if res.status().is_success() => {
            let body: agent24_protocol::DomainOsList =
                res.json().await.map_err(|e| e.to_string())?;
            print_os(&body);
            Ok(())
        }
        // Surface the daemon's own message: for a bad name it names the modules
        // that DO exist, which is the whole point of asking the daemon.
        Ok(res) => {
            let status = res.status();
            let body: serde_json::Value = res.json().await.unwrap_or_default();
            Err(match body["error"]["message"].as_str() {
                Some(m) => m.to_owned(),
                None => format!("daemon returned {status}"),
            })
        }
        Err(e) => Err(e.to_string()),
    };
    finish(ep).await;
    out
}

/// What to tell a user who cannot reach the daemon.
///
/// **It prints ONE ENTRY TO ADD, never a whole document.** The first version
/// printed a complete, valid `os.json` after the words "edit this file
/// directly" — and a user with `{"default": "disabled", ...}` who followed that
/// literally would have wiped their allow-list and silently switched ON every
/// module in the build. That is precisely the failure this whole feature treats
/// as fatal ("a config mistake that silently keeps something on"), arrived at by
/// obeying the tool instead of by mistyping. It is also printed at the WORST
/// possible moment — only when the daemon will not start, when a user is most
/// likely to copy something verbatim.
///
/// The name is serialised as JSON rather than interpolated, because it reaches
/// here without ever passing the daemon's name check: `agent24 os disable 'a"b'`
/// would otherwise print a broken document.
fn offline_hint(path: &str, action: &OsAction) -> String {
    match action {
        // Unreachable: `os_local` handles these before `cmd_os` ever looks for a
        // daemon, so an offline hint is never needed for them. Spelled out rather
        // than caught by a `_` arm — a `_` here would silently swallow a FUTURE
        // subcommand that really does need a hint, and the compiler is the only
        // thing that would otherwise have noticed. (It noticed this one.)
        OsAction::Install { .. } | OsAction::Uninstall { .. } => {
            "this command does not need the daemon".to_owned()
        }
        OsAction::List => format!("read {path} to see what is configured"),
        OsAction::Enable { name } | OsAction::Disable { name } => {
            let key = serde_json::to_string(name).unwrap_or_else(|_| "\"?\"".to_owned());
            let enabled = matches!(action, OsAction::Enable { .. });
            format!(
                "add this ONE entry inside the \"domainOs\" object in {path} \
                 (keep everything else that is already there): \
                 {key}: {{\"enabled\": {enabled}}}"
            )
        }
    }
}

fn print_os(list: &agent24_protocol::DomainOsList) {
    // The registry problem FIRST, because until it is fixed nothing else the user
    // does here takes effect — including the toggle they probably just tried.
    if let Some(err) = &list.registry_error {
        eprintln!("registry: {err}\n");
    }
    if list.modules.is_empty() {
        println!("(no domain OS installed)");
        return;
    }
    for m in &list.modules {
        // The RUNNING state leads, because that is what a request will hit. The
        // config only gets its own line when the two disagree.
        // Both states, always: the running one leads because that is what a
        // request will hit, and the config follows in parentheses when it differs
        // from what is running. An earlier version printed the config only when
        // `restart_required` was set, which hid it entirely for a REFUSED module.
        let mut line = format!("{}  {}  [{}]", m.name, m.version, m.state);
        if m.state != if m.enabled { "mounted" } else { "disabled" } {
            line.push_str(if m.enabled {
                "  (config: enabled)"
            } else {
                "  (config: disabled)"
            });
        }
        if !m.granted.is_empty() {
            line.push_str(&format!("  grants: {}", m.granted.join(",")));
        }
        println!("{line}");
        println!("    {}", m.namespace);
        if let Some(detail) = &m.detail {
            println!("    {detail}");
        }
        if m.resources == "missing" {
            println!(
                "    missing models: {}  (mounted anyway; features needing them \
                 will fail)",
                m.missing_models.join(", ")
            );
        } else if m.resources == "unknown" {
            println!("    declared models could not be checked");
        }
        if m.restart_required {
            println!(
                "    config says {} — restart the daemon to apply (agent24 daemon stop && agent24 daemon start)",
                if m.enabled { "enabled" } else { "disabled" }
            );
        }
    }
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

async fn cmd_tui() -> Result<(), String> {
    // Attach to a running daemon when present; otherwise spawn an ephemeral
    // one that lives for this TUI session (killed on exit via finish()).
    let ep = connect().await?;
    let conn = tui::Conn {
        base: ep.base.clone(),
        token: ep.token.clone(),
    };
    let result = tui::run(conn).await;
    finish(ep).await;
    result
}

fn cmd_service(action: ServiceAction) -> Result<(), String> {
    match action {
        ServiceAction::Install => {
            let exec = std::path::PathBuf::from(agent24d_binary());
            // Resolve to an absolute path: launchd has no working directory of
            // ours, so a relative or PATH-only name would never start.
            let exec = exec.canonicalize().map_err(|e| {
                format!(
                    "resolving {}: {e} — set AGENT24D_BIN to the built binary",
                    exec.display()
                )
            })?;
            let (plist, captured) = service::install(&exec)?;
            println!("24/7 enabled.");
            println!("  agent:  {}", plist.display());
            println!("  daemon: {}", exec.display());
            if let Some(logs) = service::log_dir() {
                println!("  logs:   {}", logs.display());
            }
            if !captured.is_empty() {
                println!(
                    "  env:    captured {} (snapshot — re-run install to refresh)",
                    captured.join(", ")
                );
            }
            println!("It now starts at login and restarts if it crashes.");
            println!("A clean `agent24 daemon stop` is respected (not resurrected).");
            Ok(())
        }
        ServiceAction::Uninstall => {
            service::uninstall()?;
            println!("24/7 disabled; the LaunchAgent is stopped and removed.");
            Ok(())
        }
        ServiceAction::Status => {
            let (installed, plist, loaded) = service::status();
            println!("installed: {}", if installed { "yes" } else { "no" });
            println!("loaded:    {}", if loaded { "yes" } else { "no" });
            if let Some(p) = plist {
                println!("plist:     {}", p.display());
            }
            match state_file::read_live() {
                Some(st) => println!("daemon:    running (pid {}, port {})", st.pid, st.port),
                None => println!("daemon:    not running"),
            }
            Ok(())
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Chat { message, model } => cmd_chat(message, model).await,
        Command::Models => cmd_models().await,
        Command::Daemon { action } => cmd_daemon(action).await,
        Command::Service { action } => cmd_service(action),
        Command::Tui => cmd_tui().await,
        Command::Os { action } => cmd_os(action).await,
        Command::Mcp => cmd_mcp().await,
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_offline_hint_never_tells_you_to_replace_the_whole_file() {
        // The regression that matters: a user with an allow-list who follows this
        // literally must not end up with every module enabled. The old text was a
        // complete `os.json` after the words "edit this file directly".
        let h = offline_hint(
            "/home/u/.agent24/os.json",
            &OsAction::Disable {
                name: "sin90".to_owned(),
            },
        );
        assert!(h.contains("\"sin90\": {\"enabled\": false}"), "{h}");
        assert!(
            h.contains("add this ONE entry") && h.contains("keep everything else"),
            "it must say ADD, and say the rest is to be kept: {h}"
        );
        assert!(
            !h.contains("\"domainOs\": {\"sin90\""),
            "it must not print a whole document a user could paste over theirs: {h}"
        );
        assert!(h.contains("/home/u/.agent24/os.json"), "{h}");

        let h = offline_hint("/p/os.json", &OsAction::Enable { name: "c".into() });
        assert!(h.contains("\"c\": {\"enabled\": true}"), "{h}");
    }

    #[test]
    fn the_offline_hint_escapes_a_name_the_daemon_never_got_to_reject() {
        // This path runs precisely because the daemon is unreachable, so the name
        // has NOT been through its validation. Interpolating it raw produced
        // invalid JSON for the user to paste.
        let h = offline_hint(
            "/p/os.json",
            &OsAction::Disable {
                name: r#"a"b\c"#.to_owned(),
            },
        );
        // The suggested ENTRY is everything after the last ": " separator; wrapping
        // it in braces must give a parseable object with that exact key. Parsing
        // is the assertion — eyeballing the escapes is how the bug got in.
        let entry = h
            .rsplit_once("there): ")
            .expect("the hint must end with the entry to add")
            .1;
        let v: serde_json::Value = serde_json::from_str(&format!("{{{entry}}}"))
            .unwrap_or_else(|e| panic!("the suggested entry is not valid JSON: {e}\n{entry}"));
        assert_eq!(v[r#"a"b\c"#]["enabled"], serde_json::json!(false));
    }

    #[test]
    fn the_list_hint_only_suggests_reading() {
        let h = offline_hint("/p/os.json", &OsAction::List);
        assert!(h.contains("read /p/os.json"), "{h}");
        assert!(
            !h.contains("enabled"),
            "listing must not suggest an edit: {h}"
        );
    }
}
