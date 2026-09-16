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
    // `Disable`'s line EXPIRED with SUP-5: the daemon's disable path now drains
    // and stops a running out-of-process module at once, so that line says so,
    // and keeps "the next daemon start" only for a compiled-in module, which
    // nothing stops at runtime. The test that pins it is
    // `a_disable_drains_and_stops_the_running_package` (agent24d
    // `tests/daemon_modules.rs`).
    //
    // `Enable`'s line is STILL true, for one reason only: nothing starts a
    // module at runtime — a switched-on module waits for the next start. What
    // makes it false is the daemon spawning a module at runtime (not planned:
    // the user's decision D2 was hot disable only). Whoever does that changes
    // this line, the `os_routes.rs` module docs, and `restart_required` for an
    // enable, together.
    //
    // The note lives here rather than in the follow-ups ledger so that it is
    // read by whoever changes the thing that makes it false.
    /// Turn one on (applies at the next daemon start)
    Enable {
        /// Module name, e.g. sin90
        name: String,
    },
    /// Turn one off (a running out-of-process module stops taking requests now, drains for up to 30s, then is stopped; a compiled-in one at the next daemon start)
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
    ///
    /// File removal works with the daemon down, same as install. If a daemon is
    /// reachable, a running module of it is also told to stop now (best-effort).
    /// If no daemon was reachable, or the daemon accepted the stop but could not
    /// confirm it took effect, a module of it still running elsewhere keeps
    /// serving until its own next restart, which will report `package_changed`
    /// instead of crash-looping.
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

/// The shutdown part of `agent24 daemon status` (SHUT-1c): the budgets and
/// the bound they give, anything rejected, the daemon before this one, and
/// which modules its shutdown found too slow — each with the knob to turn.
fn shutdown_lines(r: &agent24_protocol::ShutdownReport) -> Vec<String> {
    let mut out = vec![format!(
        "shutdown · drain {}ms · stop grace {}ms · exits within {}ms of SIGTERM",
        r.drain_ms, r.stop_grace_ms, r.exit_bound_ms
    )];
    for w in &r.config_warnings {
        out.push(format!("  ! {w}"));
    }
    match (&r.last_shutdown, r.previous.as_str()) {
        _ if r.ephemeral => {}
        (Some(last), "clean") => out.push(format!(
            "  previous shutdown: {} ({}ms)",
            last.stop_result, last.took_ms
        )),
        (_, "no_history") => {}
        (_, other) => out.push(format!(
            "  previous shutdown: {other}{}",
            r.previous_detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
        )),
    }
    if let Some(last) = &r.last_shutdown {
        if !last.killed_after_grace.is_empty() {
            out.push(format!(
                "  SIGKILLed after their stop grace: {} — raise A24_MODULE_STOP_GRACE_MS \
                 (then re-run `agent24 service install` for the launchd service)",
                last.killed_after_grace.join(", ")
            ));
        }
        if !last.cut_requests.is_empty() {
            out.push(format!(
                "  requests cut when the drain ran out: {} — raise A24_MODULE_DRAIN_MS",
                last.cut_requests.join(", ")
            ));
        }
    }
    out
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

/// Attaches to an already-running, healthy daemon; NEVER starts one (unlike
/// `connect`, which falls back to `spawn_daemon` when none is found) — for a
/// best-effort step (FU-61's `uninstall` hot-disable) that must not bring up
/// a fresh ephemeral daemon, which could run the very package being removed.
/// `None` if no live daemon is discoverable or healthy.
async fn attach_only() -> Option<Endpoint> {
    let state = state_file::read_live()?;
    let base = format!("http://127.0.0.1:{}", state.port);
    health_ok(&base, &state.token).await.then_some(Endpoint {
        base,
        token: state.token,
        child: None,
    })
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

/// Where installed packages live, honoring `A24_OS_PACKAGES` over `$HOME`.
///
/// The state dir is OPTIONAL here, and passing it as an option rather than
/// resolving it first is the whole difference: `A24_OS_PACKAGES` is consulted
/// before it, so a container or CI runner with the override set and no `HOME`
/// installs into the directory it asked for. Resolving `state_dir()` first
/// reimposed the `HOME` requirement that the override exists to lift, and
/// reported it with a message identical to the one for "neither is set" —
/// indistinguishable outputs for two situations, one of which was wrong.
fn packages_root() -> Result<PathBuf, String> {
    agent24_os_packages::resolve_packages_root(
        agent24_os_packages::env_override().as_deref(),
        state_file::state_dir().as_deref(),
        false,
    )
    .map_err(|e| {
        format!(
            "{e} (set HOME, or set {})",
            agent24_os_packages::PACKAGES_ROOT_ENV
        )
    })
}

/// `agent24 os install` — does NOT go through the daemon.
///
/// Installing writes files into the packages root; the daemon reads them when it
/// next starts. Making this an RPC would make writing depend on someone reading,
/// and would mean a module cannot be installed while the daemon is down — which
/// is exactly when an operator is most likely to be fixing one. `uninstall` is
/// handled separately by [`cmd_uninstall`] (FU-61: it also does a best-effort hot
/// stop of a running daemon, which `install` never needs to).
///
/// Every decision lives in `agent24-os-packages`: which directory to write to,
/// what the installed name is (the manifest's, not the source directory's), and
/// whether one is already there. This function maps arguments onto that and
/// prints the result. If it ever needs to compute a path or check for a duplicate
/// itself, the seam is in the wrong place and that logic belongs in the library.
fn os_local(action: &OsAction) -> Option<Result<(), String>> {
    let root = match packages_root() {
        Ok(root) => root,
        Err(e) => return Some(Err(e)),
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
        // Handled before this function is ever called; see `cmd_uninstall`.
        OsAction::Uninstall { .. } => None,
        _ => None,
    }
}

/// `agent24 os uninstall` — file removal decides success or failure by
/// itself (unchanged contract: works with the daemon down); hot-disable
/// (FU-61) is a best-effort step tried ONLY after a real removal, whose
/// outcome can only add an informational line, never flip the `Result` this
/// function already decided. This split (rather than folding into
/// `os_local`) exists because the daemon call needs the REAL `bool`
/// `install::uninstall` returns (was something actually removed just now?)
/// — `os_local`'s shared `Option<Result<(), String>>` shape has nowhere to
/// carry that.
async fn cmd_uninstall(name: &str) -> Result<(), String> {
    let root = packages_root()?;
    match agent24_os_packages::install::uninstall(name, &root) {
        Err(e) => Err(e.to_string()), // nothing removed: no hot-stop, ever
        Ok(false) => {
            // Not an error: the end state the operator asked for is the one
            // they have. Saying so beats a failure they must decide to
            // ignore. Idempotent: nothing NEW was removed, so there is
            // nothing for a hot-stop to respond to.
            println!("{name} was not installed; nothing to remove");
            Ok(())
        }
        Ok(true) => {
            println!("removed {name}");
            println!("  it takes effect at the next daemon start");
            hot_disable_best_effort(name).await;
            Ok(())
        }
    }
}

/// Best-effort: tells an already-running daemon to stop serving `name` now,
/// rather than leaving a healthy module to keep answering until it next
/// happens to restart (which the daemon's own re-check then reports as
/// `package_changed` — but only for a module that DOES eventually restart;
/// for a long-healthy one that could be indefinite). Never starts a daemon
/// (`attach_only`, not `connect`) and never turns into an `Err` — by the
/// time this runs, `cmd_uninstall`'s `Ok(())` is already final.
async fn hot_disable_best_effort(name: &str) {
    let Some(ep) = attach_only().await else {
        println!(
            "  no reachable daemon right now; a module of it running elsewhere will report \
             `package_changed` after its own next restart"
        );
        return;
    };
    // POST .../stop, NOT the PATCH `agent24 os disable` uses — that one
    // persists `enabled:false` into `os.json`, which is actively wrong for a
    // package about to vanish from discovery entirely (it would trip
    // `unknown_disabled`'s fail-closed check on the daemon's next start and
    // degrade every OTHER module too). This is a one-shot "stop it now",
    // nothing written for next time.
    let sent = bearer(
        &ep,
        client().post(format!("{}/api/v1/os/{name}/stop", ep.base)),
    )
    .timeout(Duration::from_secs(5))
    .send()
    .await;
    match sent {
        // 2xx covers the daemon's `Stopping`/`Already`/`NotRunning` alike —
        // the body alone does not say which, so this must not claim "it was
        // running and I stopped it".
        Ok(res) if res.status().is_success() => println!(
            "  told the running daemon to stop serving it — `agent24 os list` shows whether a \
             running module was actually there to stop"
        ),
        // `disable_pending` / `stop_failed`: the stop was handed off before
        // either of these is returned, and nothing was persisted either
        // way — this module will not restart on its own from a config
        // change it never received. Relay the daemon's own message.
        Ok(res) => {
            let body: serde_json::Value = res.json().await.unwrap_or_default();
            let msg = body["error"]["message"].as_str().unwrap_or("(no detail)");
            println!("  the daemon could not fully confirm the stop: {msg}");
        }
        // Genuinely ambiguous: the request may never have reached the
        // daemon, or it may have applied the change and the response was
        // lost. Neither "not stopped" nor "will report package_changed" can
        // be asserted here — say so plainly instead of guessing.
        Err(e) => println!(
            "  could not confirm the daemon received this ({e}) — if it did not, a module of \
             it still running there will report `package_changed` after its own next restart; \
             if it did, that module has already been told to stop"
        ),
    }
}

/// `agent24 os` — read and toggle the domain-OS registry.
///
/// `list` / `enable` / `disable` go through the daemon; this never touches
/// `os.json`. That is what makes `agent24 os disable sin09` fail HERE, naming the
/// modules that do exist, instead of writing a file that breaks the registry at
/// the next start. `install` / `uninstall` are different in kind and are handled
/// before any of that — `install` by [`os_local`], `uninstall` by
/// [`cmd_uninstall`] (FU-61: it also does a best-effort hot stop, which needs
/// the daemon call `os_local`'s shared return shape cannot carry).
async fn cmd_os(action: OsAction) -> Result<(), String> {
    if let OsAction::Uninstall { name } = &action {
        return cmd_uninstall(name).await;
    }
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

/// How long `agent24 daemon stop` waits for the old daemon to actually
/// release its singleton lock before giving up and saying so. `POST
/// /shutdown` returns as soon as shutdown is REQUESTED — draining modules,
/// persisting the shutdown summary and the runtime's own teardown all still
/// run after that. `lifecycle.rs`'s real worst case is ~15.7s (10s drain +
/// 5s stop grace + ~0.5s persistence/teardown margin, modules draining
/// concurrently via a `JoinSet`, not sequentially); 30s leaves close to a
/// full extra margin over that on top.
const STOP_CONFIRM_BUDGET: Duration = Duration::from_secs(30);

/// Waits until the daemon that was just asked to stop has actually released
/// its singleton lock — the SAME lock `agent24 daemon start` will contend
/// for — rather than trusting `POST /shutdown`'s immediate `202`. Health
/// going false is not enough evidence: it only means the accept loop closed,
/// not that the process exited or released the lock (a daemon started by
/// `agent24 service install` may also still be mid-teardown after that,
/// which is a distinct, tracked limitation — see FU-68 in followups.md, not
/// solved here). Probing the lock directly and releasing it at once is a
/// cheap, non-blocking, cross-process check (`try_lock_exclusive`) — the
/// exact same test `daemon start`'s own spawn path needs to pass.
async fn wait_for_stop(state: &DaemonState) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + STOP_CONFIRM_BUDGET;
    loop {
        match agent24_protocol::state_file::try_acquire_singleton() {
            Ok(Some(lock)) => {
                drop(lock); // release at once — this call only probes
                println!("stopped (pid {}, port {})", state.pid, state.port);
                return Ok(());
            }
            Ok(None) => {}
            Err(e) => return Err(format!("could not check whether it stopped: {e}")),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "shutdown requested (pid {}, port {}) but it did not release its lock \
                 within {}s — check its logs before starting a new one",
                state.pid,
                state.port,
                STOP_CONFIRM_BUDGET.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
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
                    // SHUT-1c. Only a daemon from before it (405) is skipped
                    // quietly; any other failure is said, since the report is
                    // how a budget that is too tight gets noticed (review of
                    // SHUT-1c, round 1).
                    // The whole exchange is bounded, not just the connect: a
                    // daemon that stalls mid-response must not hang `status`
                    // (review of SHUT-1c, round 2).
                    match client()
                        .get(format!("{base}/api/v1/shutdown"))
                        .bearer_auth(&state.token)
                        .timeout(Duration::from_secs(5))
                        .send()
                        .await
                    {
                        Ok(res) if res.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED => {}
                        Ok(res) if res.status().is_success() => {
                            match res.json::<agent24_protocol::ShutdownReport>().await {
                                Ok(report) => {
                                    for line in shutdown_lines(&report) {
                                        println!("{line}");
                                    }
                                }
                                Err(e) => println!("  (shutdown report unreadable: {e})"),
                            }
                        }
                        Ok(res) => {
                            println!("  (shutdown report: daemon returned {})", res.status())
                        }
                        Err(e) => println!("  (shutdown report unavailable: {e})"),
                    }
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
                    Ok(r) if r.status().is_success() => wait_for_stop(&state).await,
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

    /// `daemon status` says what the shutdown report says, and names the knob
    /// for each module found too slow (SHUT-1c).
    #[test]
    fn the_status_names_the_knob_for_each_slow_module() {
        let r = agent24_protocol::ShutdownReport {
            ephemeral: false,
            evidence_dir: Some("/h/.agent24/run".into()),
            drain_ms: 800,
            stop_grace_ms: 500,
            exit_bound_ms: 2000,
            config_warnings: vec!["A24_MODULE_DRAIN_MS=\"x\" … using the default".into()],
            previous: "clean".into(),
            previous_detail: None,
            last_shutdown: Some(agent24_protocol::LastShutdown {
                stop_result: "clean".into(),
                began_at_ms: 0,
                took_ms: 1120,
                killed_after_grace: vec!["slow".into()],
                cut_requests: vec!["busy (3)".into()],
                omitted_records: 0,
            }),
        };
        let lines = shutdown_lines(&r).join("\n");
        assert!(lines.contains("drain 800ms · stop grace 500ms · exits within 2000ms of SIGTERM"));
        assert!(lines.contains("! A24_MODULE_DRAIN_MS"));
        assert!(lines.contains("previous shutdown: clean (1120ms)"));
        assert!(lines.contains("slow — raise A24_MODULE_STOP_GRACE_MS"));
        assert!(lines.contains("busy (3) — raise A24_MODULE_DRAIN_MS"));

        let unconfirmed = agent24_protocol::ShutdownReport {
            previous: "unconfirmed".into(),
            previous_detail: Some("did not confirm a clean shutdown".into()),
            last_shutdown: None,
            config_warnings: vec![],
            ..r
        };
        let lines = shutdown_lines(&unconfirmed).join("\n");
        assert!(
            lines.contains("previous shutdown: unconfirmed — did not confirm"),
            "{lines}"
        );
    }

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
