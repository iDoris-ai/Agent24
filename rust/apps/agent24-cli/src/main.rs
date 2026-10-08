//! agent24 — CLI for the Agent24 daemon (B6 skeleton).
//!
//! Two connection modes:
//! - Attached: a running agent24d is discovered via ~/.agent24/daemon.json
//! - Standalone: no daemon found → spawn an ephemeral agent24d for this
//!   invocation and terminate it afterwards

use std::io::BufRead;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use agent24_protocol::state_file::{self, DaemonState};
use agent24_protocol::{
    AttachedAddRequest, AttachedAddResponse, AttachedList, ChatMessage, ChatRequest, ChatResponse,
    Health,
};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, BufReader};
use zeroize::{Zeroize, Zeroizing};

mod acp;
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
    /// Manage the embedded Hyphae-based agent communication layer:
    /// identities, contacts, and relays (COMM-HYPHAE.md §4). A REST client
    /// of `/api/v1/comm/*`, same shape as `agent24 os`.
    Comm {
        #[command(subcommand)]
        action: CommAction,
    },
    /// Serve agent24d as an MCP server over stdio, so an external MCP client
    /// (Claude Desktop, another agent) can run tasks on it and introspect it.
    /// Risky actions are still approved on THIS host, never by the caller (E4).
    Mcp,
    /// Serve Agent24 as an ACP agent over newline-delimited JSON-RPC on stdio.
    /// Open Design uses this bridge for its Creative runtime.
    Acp,
    /// Export/delete the local decision log (D0-2; `docs/agent/PLAN-DECIDE.md`
    /// §2.1-§2.3). Like `os install`/`uninstall`, this does NOT go through the
    /// daemon — see `cmd_decide`'s doc comment.
    Decide {
        #[command(subcommand)]
        action: DecideAction,
    },
}

/// `agent24 decide …` (D0-2).
#[derive(Subcommand)]
enum DecideAction {
    /// Export the decision log as JSONL — one line per decision, including
    /// every outcome recorded against it (§2.1/§2.3: "可查看、导出（JSONL）").
    Export {
        /// Required for now: JSONL is the only export shape PLAN-DECIDE.md
        /// asks for. Kept as an explicit flag (rather than assumed) so a
        /// future second shape cannot silently change today's default.
        #[arg(long)]
        jsonl: bool,
        /// Only decisions at this decision point (e.g. `retain.intent`).
        #[arg(long)]
        point: Option<String>,
        /// Only decisions at/after this ISO-8601 timestamp.
        #[arg(long)]
        since: Option<String>,
    },
    /// Delete entries from the decision log — exactly one granularity per
    /// invocation (§2.3: "按条删除、按 point 删除、全部删除").
    Delete {
        #[command(subcommand)]
        action: DecideDeleteAction,
    },
}

#[derive(Subcommand)]
enum DecideDeleteAction {
    /// Delete one decision (and its outcomes) by id.
    Id {
        /// The `decision_id` field, as printed by `export`.
        decision_id: String,
    },
    /// Delete every decision recorded at one decision point.
    Point { point: String },
    /// Delete the ENTIRE decision log. Destructive — requires `--confirm`.
    All {
        #[arg(long)]
        confirm: bool,
    },
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
    /// If no daemon was reachable at all, a module of it still running elsewhere
    /// keeps serving until its own next restart, which will report
    /// `package_changed` instead of crash-looping. If a daemon WAS reachable but
    /// could not confirm the stop, `agent24 os list` shows why — that module will
    /// not restart on its own from this.
    Uninstall { name: String },
    /// A3: manage attached modules (a user-started process registering
    /// itself against a running daemon — `docs/design/A3-ATTACHED-MODULE.md`
    /// §3). Unlike install/uninstall this always goes through the daemon:
    /// the registry it writes to (`attached.json`) is daemon-owned the same
    /// way `os.json` is.
    Attach {
        #[command(subcommand)]
        action: OsAttachAction,
    },
}

/// `agent24 os attach …` (A3-2a; §3.2/§3.6).
#[derive(Subcommand)]
enum OsAttachAction {
    /// Register (first time) or rotate the token of an already-registered
    /// attached module, from a `domain-os.yml`-shaped manifest file.
    ///
    /// Prints the new token on success — SAVE IT, the daemon never shows it
    /// again (`GET`/`os attach list` never include it, §3.2/§3.3).
    Add {
        /// Path to the manifest file (§3.1)
        manifest: PathBuf,
        /// Confirm a registration that WIDENS privacy (first-time
        /// `remote_allowed`, or a new capability, §3.5). Only takes effect
        /// when stdin is a TTY and the prompt is answered "yes" — run
        /// non-interactively (as AgentEar's own auto-pairing does), this
        /// flag has no effect and a relaxing manifest is refused by the
        /// daemon (`relax_requires_confirmation`). This is deliberate: A3
        /// does not let an unattended process silently widen its own
        /// privacy.
        #[arg(long)]
        allow_remote: bool,
        /// Print exactly one JSON object to stdout: the same shape as the
        /// daemon's `201`/`200` body on success, or `{"error":{...}}` on
        /// failure (§3.6) — what AgentEar's own auto-pairing parses.
        #[arg(long)]
        json: bool,
    },
    /// List attached modules (never shows the token, §3.2)
    List {
        #[arg(long)]
        json: bool,
    },
    /// Revoke (de-register) an attached module. `revoke` is accepted as an
    /// alias — the design doc's own CLI contract (§3.6) spells this verb
    /// `revoke`; this project's task brief spelled it `remove`. Both work.
    #[command(alias = "revoke")]
    Remove {
        name: String,
        #[arg(long)]
        json: bool,
    },
}

/// `agent24 comm …` (COMM-2a/2b; COMM-HYPHAE.md §4's CLI/REST 1:1 mapping).
#[derive(Subcommand)]
enum CommAction {
    /// Manage Hyphae identities
    Identity {
        #[command(subcommand)]
        action: CommIdentityAction,
    },
    /// Manage Hyphae contacts
    Contact {
        #[command(subcommand)]
        action: CommContactAction,
    },
    /// Manage Hyphae relays
    Relay {
        #[command(subcommand)]
        action: CommRelayAction,
    },
    /// Import an existing, unmanaged `~/.hyphae` HOME (COMM-HYPHAE.md §4.1,
    /// D3). The source is only ever read — nothing is deleted, moved, or
    /// modified there (non-blocking probes of `outbox.json.lock` and, when
    /// present, `daemon.lock` aside).
    ///
    /// STOP the `hyphae` daemon using `from` before running this. The
    /// server-side probes catch a daemon mid-outbox-operation, and — only
    /// on a Hyphae build that ships `daemon.lock` (Hyphae#104+) — an idle
    /// one too, but neither can prove every daemon touching this HOME has
    /// actually stopped.
    Import {
        /// The old Hyphae HOME to import — the directory that used to be
        /// `$HOME` when `hyphae` ran unmanaged (i.e. the parent of its own
        /// `.hyphae/`, not `.hyphae` itself).
        from: PathBuf,
        /// Confirm the import. Required even with `--dry-run` — import
        /// always needs an explicit confirmation (COMM-HYPHAE.md §4's
        /// `confirm_required` row). Make sure the `hyphae` daemon using
        /// `from` is stopped before passing this — see the command's own
        /// help text above.
        #[arg(long)]
        yes: bool,
        /// Only validate the source and report identity/contact/outbox
        /// counts; never writes a password or touches the real Hyphae
        /// HOME.
        #[arg(long)]
        dry_run: bool,
    },
    /// Verify and temporarily unlock the encrypted Hyphae keystore
    Unlock {
        /// Also persist the verified password in the OS keychain
        #[arg(long)]
        remember: bool,
    },
    /// Manage the Hyphae daemon `agent24d` supervises (COMM-4a)
    Daemon {
        #[command(subcommand)]
        action: CommDaemonAction,
    },
    /// Send a message (COMM-3; COMM-HYPHAE.md §4/§5.1). There is no "resend"
    /// anywhere under `comm` — `outbox retry` is the only way to retry an
    /// already-sent message, and it reuses the original event_id.
    Send {
        /// Recipient nickname or npub
        to: String,
        /// Message content (COMM-HYPHAE.md §3 M7: this goes to Hyphae via
        /// argv, so it is visible to `ps` run by other users on this host)
        content: String,
        /// Sender identity nickname (defaults to the default identity)
        #[arg(long)]
        from: Option<String>,
        /// Send unencrypted (encrypted is the default)
        #[arg(long)]
        no_encrypt: bool,
    },
    /// Read received message history — pull-based, not push-based (see
    /// `pull`): a daemon normally keeps this current; without one, run
    /// `pull` first to see new messages here.
    History {
        /// Identity nickname to read as (defaults to the default identity)
        #[arg(long = "as")]
        as_: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Manually pull new inbound messages once. The daemon's own relay
    /// polling normally does this automatically (COMM-4a); use this only
    /// when no daemon is managing the relay connection for you.
    Pull {
        #[arg(long = "as")]
        as_: Option<String>,
    },
    /// Manage the outbox (queued/failed sends)
    Outbox {
        #[command(subcommand)]
        action: CommOutboxAction,
    },
}

#[derive(Subcommand)]
enum CommDaemonAction {
    /// Show the supervised Hyphae daemon's state
    Status,
    /// Start the Hyphae daemon
    Start,
    /// Stop the Hyphae daemon
    Stop,
}

#[derive(Subcommand)]
enum CommIdentityAction {
    /// List every identity
    List,
    /// Create a new identity (the very first one encrypts the keystore and
    /// generates its password automatically — see COMM-HYPHAE.md §6.4)
    Create {
        nickname: String,
        /// Make this the default identity
        #[arg(long)]
        default: bool,
    },
    /// Switch the default identity
    Use { nickname: String },
}

#[derive(Subcommand)]
enum CommContactAction {
    /// List every contact
    List,
    /// Add a contact
    Add {
        nickname: String,
        npub: String,
        #[arg(long)]
        role: Option<String>,
    },
}

#[derive(Subcommand)]
enum CommRelayAction {
    /// Show the configured relays and whether they were actually configured
    /// (vs. Hyphae's own built-in default, COMM-HYPHAE.md §9 R1)
    List,
    /// Replace the relay set — one call, full replace, not an incremental add
    Set {
        /// One or more `ws://`/`wss://` relay urls (1..=8)
        #[arg(required = true)]
        relays: Vec<String>,
    },
    /// Probe a relay (or every configured one) for connectivity
    Probe { url: Option<String> },
}

#[derive(Subcommand)]
enum CommOutboxAction {
    /// List outbox entries
    List {
        /// Only entries that have failed at least once
        #[arg(long)]
        failed_only: bool,
    },
    /// Retry a queued/failed outbox entry — reuses its original event_id,
    /// never mints a new one
    Retry { event_id: String },
    /// Clear local outbox bookkeeping (never un-sends an event a relay
    /// already accepted)
    Clear {
        /// Required: this is a destructive, local-only operation
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        min_failures: Option<u32>,
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

const HOST_AUTHORITY_UNAVAILABLE: &str = "host authority unavailable";

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

/// Client for CLI/TUI → daemon calls, which always target `127.0.0.1` (see the
/// `format!("http://127.0.0.1:{}", state.port)` call sites below) and carry the
/// bearer token plus chat content.
///
/// FU-74: the default reqwest client reads `HTTP_PROXY`/`ALL_PROXY` and does
/// NOT bypass loopback for them, and follows redirects — either behaviour
/// would send the bearer token and message content somewhere other than the
/// daemon whenever the user's shell happens to export a proxy. The daemon
/// itself never redirects, so a 3xx means something else is impersonating it;
/// following it would hand over the bearer token to that impersonator.
fn client() -> reqwest::Client {
    #[expect(
        clippy::expect_used,
        reason = "unwrap_or_default() here would silently rebuild the proxy-reading, \
                  redirect-following client FU-74 exists to rule out; fail closed instead"
    )]
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("building the loopback-only daemon HTTP client failed")
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

fn discovery_health_token(state: &DaemonState) -> &str {
    &state.token
}

/// Resolve a discovery record for an operation that needs the host bearer.
/// Capability discovery is intentionally not upgraded into authority by
/// treating an absent token as an anonymous request.
fn host_token(state: &DaemonState) -> Result<&str, String> {
    state.bearer_token().map_err(|err| match err {
        HOST_AUTHORITY_UNAVAILABLE => HOST_AUTHORITY_UNAVAILABLE.to_owned(),
        _ => format!("invalid daemon discovery state: {err}"),
    })
}

fn endpoint_from_state(state: DaemonState) -> Result<Endpoint, String> {
    let token = host_token(&state)?.to_owned();
    Ok(Endpoint {
        base: format!("http://127.0.0.1:{}", state.port),
        token,
        child: None,
    })
}

/// Parse the ready line without ever manufacturing a host token for a
/// capability daemon. The daemon output remains backward-compatible: missing
/// `auth_mode` means legacy, and legacy ready lines must still carry `token`.
fn parse_ready_state(value: &serde_json::Value, pid: u32) -> Result<DaemonState, String> {
    if value["type"] != "ready" {
        return Err("not a daemon ready line".to_owned());
    }
    let port = value["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p != 0)
        .ok_or_else(|| "ready line has invalid port".to_owned())?;
    let auth_mode = value
        .get("auth_mode")
        .cloned()
        .map(serde_json::from_value::<agent24_protocol::state_file::AuthMode>)
        .transpose()
        .map_err(|_| "ready line has unknown auth_mode".to_owned())?
        .unwrap_or_default();
    let token = value
        .get("token")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let state = DaemonState {
        port,
        token,
        pid,
        version: value["version"].as_str().unwrap_or("").to_owned(),
        generation: value["generation"].as_str().unwrap_or("").to_owned(),
        auth_mode,
    };
    state
        .validate()
        .map_err(|e| format!("invalid ready line: {e}"))?;
    Ok(state)
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
            let pid = child.id().unwrap_or(0);
            let parsed = parse_ready_state(&state, pid)?;
            return Ok((parsed, child));
        }
    }
}

/// Attached if a live daemon is discoverable and healthy; standalone otherwise.
async fn connect() -> Result<Endpoint, String> {
    if let Some(state) = state_file::read_live() {
        let base = format!("http://127.0.0.1:{}", state.port);
        if state.auth_mode.is_capabilities() {
            // Health is deliberately anonymous, but it never grants the
            // bearer needed by the operation that called `connect`.
            let _ = health_ok(&base, "").await;
            return Err(HOST_AUTHORITY_UNAVAILABLE.to_owned());
        }
        if health_ok(&base, discovery_health_token(&state)).await {
            return endpoint_from_state(state);
        }
    }
    let (state, child) = spawn_daemon(true).await?;
    let base = format!("http://127.0.0.1:{}", state.port);
    Ok(Endpoint {
        base,
        token: host_token(&state)?.to_owned(),
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
    if state.auth_mode.is_capabilities() {
        return None;
    }
    let base = format!("http://127.0.0.1:{}", state.port);
    health_ok(&base, discovery_health_token(&state))
        .await
        .then(|| endpoint_from_state(state).ok())
        .flatten()
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

/// Serve the running daemon through the Agent Client Protocol over stdio.
/// Prototype-first: the bridge reuses the daemon's existing run/session/event
/// APIs; richer ACP capabilities are added only after the M4 product path runs.
async fn cmd_acp() -> Result<(), String> {
    let ep = connect().await?;
    let result = acp::serve(&ep).await;
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
        session_id: None,
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

/// Render a daemon error envelope the way every call site here should
/// (ERR-1 — "CLI 原样打印"): the `message`, and — when the daemon gave one —
/// the `hint` on its own line right after it, unchanged text, no extra
/// wrapping. Shared by the two call sites that actually parse
/// `error.message` (`hot_disable_best_effort` and `cmd_os`); a bare
/// "daemon returned {status}" fallback for a response with no parseable
/// body is each call site's own concern, not this function's.
fn daemon_error_line(body: &serde_json::Value) -> String {
    let message = body["error"]["message"].as_str().unwrap_or("(no detail)");
    match body["error"]["hint"].as_str() {
        Some(hint) if !hint.is_empty() => format!("{message}\n  {hint}"),
        _ => message.to_owned(),
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
            println!(
                "  the daemon could not fully confirm the stop: {}",
                daemon_error_line(&body)
            );
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
    // A3-2a: always goes through the daemon (the registry it writes,
    // `attached.json`, is daemon-owned the same way `os.json` is) but has its
    // own request/response shapes and its own `--json`/TTY handling, so it is
    // its own function rather than another arm of the `req`/`out` match below.
    if let OsAction::Attach { action } = action {
        return cmd_os_attach(action).await;
    }
    if let OsAction::Uninstall { name } = &action {
        return cmd_uninstall(name).await;
    }
    if let Some(done) = os_local(&action) {
        return done;
    }
    let ep = match attach_only().await {
        Some(ep) => ep,
        // `connect` would start an ephemeral daemon with an isolated package
        // root. Its empty catalogue would report false facts about the user's
        // installed OS packages, so these commands only use a resident daemon.
        None => {
            let path = agent24_protocol::state_file::state_dir()
                .map(|d| d.join("os.json").display().to_string())
                .unwrap_or_else(|| "~/.agent24/os.json".to_owned());
            let hint = format!(
                "daemon not running; start it with `agent24 daemon start`.\n  {}",
                offline_hint(&path, &action)
            );
            if matches!(action, OsAction::List) {
                println!("{hint}");
                return Ok(());
            }
            return Err(hint);
        }
    };
    let req = match &action {
        // Handled before the daemon lookup above; see `os_local`.
        OsAction::Install { .. } | OsAction::Uninstall { .. } => unreachable!(),
        // Handled at the top of this function, before it ever reaches here.
        OsAction::Attach { .. } => unreachable!(),
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
            Err(if body["error"]["message"].as_str().is_some() {
                daemon_error_line(&body)
            } else {
                format!("daemon returned {status}")
            })
        }
        Err(e) => Err(e.to_string()),
    };
    finish(ep).await;
    out
}

/// `agent24 decide export`/`delete` — D0-2 (`docs/agent/PLAN-DECIDE.md`
/// §2.1-§2.3). Unlike every other CLI command except `os install`/`uninstall`
/// (see `os_local`'s doc comment), this does NOT go through the daemon: the
/// decision log is a local-only privacy feature (§2.3 — "日志只存本地…永不
/// 上传"), and a user must be able to export or delete their own data even
/// if the daemon that would otherwise own `~/.agent24/agent24.db` is not
/// running (or has crashed) — the same shape of argument `os install` makes
/// for not depending on "someone reading", just for "someone running".
/// SQLite's WAL mode (how `Store::open` already connects) supports a second
/// process reading/writing the same file concurrently with a running
/// daemon's own connection pool; this is not a new concurrency hazard.
///
/// This talks to `agent24-store` directly, NOT through
/// `agent24_decide::log::DecisionLog` — that trait is D0-2's write-path
/// contract for whatever D1 component actually runs decisions (composed
/// inside `agent24d`, see `docs/decision.md` ADR-034). Export/delete are
/// read/delete-only maintenance operations on already-written rows; they
/// have no need for an abstraction over "which store backs this", because
/// for a local CLI tool SQLite-via-`agent24-store` IS the implementation,
/// today and for the foreseeable future.
async fn cmd_decide(action: DecideAction) -> Result<(), String> {
    match &action {
        DecideAction::Export { jsonl, .. } => require_jsonl(*jsonl)?,
        DecideAction::Delete { .. } => {}
    }
    let dir = agent24_protocol::state_file::state_dir().ok_or_else(|| "HOME not set".to_owned())?;
    let store = agent24_store::Store::open(&dir.join("agent24.db"))
        .await
        .map_err(|e| e.to_string())?;
    match action {
        DecideAction::Export { point, since, .. } => {
            let rows = store
                .export_decision_log(point.as_deref(), since.as_deref())
                .await
                .map_err(|e| e.to_string())?;
            for row in &rows {
                println!("{}", decision_log_export_line(row)?);
            }
            Ok(())
        }
        DecideAction::Delete { action } => cmd_decide_delete(&store, action).await,
    }
}

/// `export`'s `--jsonl` guard, pulled out to a pure function so it is
/// directly unit-testable (pre-pr-check T2) without a `Store` or `HOME` —
/// see `jsonl_flag_is_required_and_checked_before_touching_the_store` below.
fn require_jsonl(jsonl: bool) -> Result<(), String> {
    if jsonl {
        Ok(())
    } else {
        Err("only --jsonl export is supported today (PLAN-DECIDE.md's own ask)".to_owned())
    }
}

/// `delete all`'s `--confirm` guard, pulled out for the same reason — see
/// `delete_all_without_confirm_is_rejected_and_deletes_nothing` below.
fn require_confirm(confirm: bool) -> Result<(), String> {
    if confirm {
        Ok(())
    } else {
        Err("this deletes the ENTIRE decision log — pass --confirm to proceed".to_owned())
    }
}

async fn cmd_decide_delete(
    store: &agent24_store::Store,
    action: DecideDeleteAction,
) -> Result<(), String> {
    match action {
        DecideDeleteAction::Id { decision_id } => {
            let deleted = store
                .delete_decision_log_by_id(&decision_id)
                .await
                .map_err(|e| e.to_string())?;
            println!(
                "{}",
                if deleted {
                    "deleted 1 decision"
                } else {
                    "no decision with that id"
                }
            );
            Ok(())
        }
        DecideDeleteAction::Point { point } => {
            let deleted = store
                .delete_decision_log_by_point(&point)
                .await
                .map_err(|e| e.to_string())?;
            println!("deleted {deleted} decision(s) at point {point:?}");
            Ok(())
        }
        DecideDeleteAction::All { confirm } => {
            require_confirm(confirm)?;
            let deleted = store
                .delete_all_decision_log()
                .await
                .map_err(|e| e.to_string())?;
            println!("deleted {deleted} decision(s)");
            Ok(())
        }
    }
}

/// Assembles one `export`'s JSONL line from a stored row: every `*_json`
/// column agent24-store hands back as raw text gets parsed into real nested
/// JSON (never double-encoded as a string), and the key is `final` — not
/// `final_action` — to match `PLAN-DECIDE.md` §2.1's field name exactly (same
/// rename `agent24_decide::log::LogEntry` applies on the write side).
fn decision_log_export_line(row: &agent24_store::DecisionLogExportRow) -> Result<String, String> {
    let log = &row.log;
    let parse = |s: &str| -> Result<serde_json::Value, String> {
        serde_json::from_str(s).map_err(|e| format!("corrupt stored JSON in decision log: {e}"))
    };
    let context = match &log.context_json {
        Some(s) => parse(s)?,
        None => serde_json::Value::Null,
    };
    let question = parse(&log.question_json)?;
    let layers = parse(&log.layers_json)?;
    let outcomes = row
        .outcomes
        .iter()
        .map(|o| -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({
                "ts": o.ts,
                "signal": o.signal,
                "label": parse(&o.label_json)?,
                "quality": o.quality,
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let obj = serde_json::json!({
        "schema_version": log.schema_version,
        "decision_id": log.decision_id,
        "ts": log.ts,
        "point": log.point,
        "input": log.input,
        "context": context,
        "question": question,
        "layers": layers,
        "final": log.final_action,
        "hw_tier": log.hw_tier,
        "scrubbed_at": log.scrubbed_at,
        "outcomes": outcomes,
    });
    serde_json::to_string(&obj).map_err(|e| e.to_string())
}

/// Minimal query-string/path-segment percent-encoding (no new dependency):
/// everything but unreserved characters is escaped, which is always safe
/// even though most nicknames/event_ids here never actually need it.
fn qs_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `agent24 comm …`: a REST client of `/api/v1/comm/*`
/// (COMM-HYPHAE.md §4) — one method/path/body per leaf subcommand, sent with
/// the same bearer/timeout/error-reporting shape as `cmd_os`. Unlike `cmd_os`
/// there is no local/offline path: every comm operation needs a running
/// daemon (it is the only thing holding the Hyphae runner).
/// Codex 挑战（B 端 gpt-6-astra）Medium：`send`/`outbox retry` 的
/// `partial`/`conflict` 错误带着 `data`（`event_id`、relay 接受情况
/// `published_to`、记账异常 `audit_error`，或 retry 的
/// `sent`/`superseded`），而旧实现只打印 `error`/`message` 两个字段，这些
/// data 在 CLI 上直接丢了，用户拿不到可用来查 outbox/history 的
/// event_id。这里直接把完整 envelope（而不只是 `data`）打到错误行里——
/// `error`/`message` 不带 data 的路由（identity/contact/relay/…）行为不变，
/// 因为它们的 `body["data"]` 本就是 `null`。
fn format_comm_error(body: &serde_json::Value) -> String {
    let error = body["error"].as_str().unwrap_or("error");
    let message = body["message"].as_str().unwrap_or_default();
    let mut out = format!("comm: {error} — {message}");
    if let Some(data) = body.get("data").filter(|d| !d.is_null())
        && let Ok(pretty) = serde_json::to_string_pretty(data)
    {
        out.push_str(&format!("\n  data: {pretty}"));
    }
    out
}

async fn cmd_comm(action: CommAction) -> Result<(), String> {
    // Import's server-side work (copy + verify + a Hyphae-side password
    // check) is real file and subprocess I/O, not a quick REST round trip —
    // give it a much longer client-side timeout than every other comm
    // action's 15s.
    //
    // `send`/`outbox retry` can take up to `5s × 8 relays + 10s = 50s`
    // server-side (COMM-HYPHAE.md §3); give those two a generous timeout too
    // rather than the default 15s.
    let timeout = if matches!(action, CommAction::Unlock { .. }) {
        Duration::from_secs(30)
    } else if matches!(action, CommAction::Import { .. }) {
        Duration::from_secs(120)
    } else if matches!(
        action,
        CommAction::Send { .. }
            | CommAction::Outbox {
                action: CommOutboxAction::Retry { .. },
            }
    ) {
        Duration::from_secs(65)
    } else {
        Duration::from_secs(15)
    };
    let (method, path, body) = match action {
        CommAction::Import { from, yes, dry_run } => {
            let password = if dry_run {
                None
            } else {
                Some(read_password_from_stdin()?)
            };
            (
                reqwest::Method::POST,
                "/api/v1/comm/import".to_owned(),
                Some(serde_json::json!({
                    "from": from.to_string_lossy(),
                    "confirm": yes,
                    "password": password.as_deref(),
                    "dry_run": dry_run,
                })),
            )
        }
        CommAction::Unlock { remember } => {
            let password = read_password_from_stdin()?;
            (
                reqwest::Method::POST,
                "/api/v1/comm/unlock".to_owned(),
                Some(serde_json::json!({
                    "password": password.as_str(),
                    "remember": remember,
                })),
            )
        }
        CommAction::Identity { action } => match action {
            CommIdentityAction::List => (
                reqwest::Method::GET,
                "/api/v1/comm/identity".to_owned(),
                None,
            ),
            CommIdentityAction::Create { nickname, default } => (
                reqwest::Method::POST,
                "/api/v1/comm/identity".to_owned(),
                Some(serde_json::json!({"nickname": nickname, "default": default})),
            ),
            CommIdentityAction::Use { nickname } => (
                reqwest::Method::POST,
                "/api/v1/comm/identity/default".to_owned(),
                Some(serde_json::json!({"nickname": nickname})),
            ),
        },
        CommAction::Contact { action } => match action {
            CommContactAction::List => (
                reqwest::Method::GET,
                "/api/v1/comm/contact".to_owned(),
                None,
            ),
            CommContactAction::Add {
                nickname,
                npub,
                role,
            } => (
                reqwest::Method::POST,
                "/api/v1/comm/contact".to_owned(),
                Some(serde_json::json!({"nickname": nickname, "npub": npub, "role": role})),
            ),
        },
        CommAction::Relay { action } => match action {
            CommRelayAction::List => (reqwest::Method::GET, "/api/v1/comm/relay".to_owned(), None),
            CommRelayAction::Set { relays } => (
                reqwest::Method::PUT,
                "/api/v1/comm/relay".to_owned(),
                Some(serde_json::json!({"relays": relays})),
            ),
            CommRelayAction::Probe { url } => (
                reqwest::Method::POST,
                "/api/v1/comm/relay/probe".to_owned(),
                Some(serde_json::json!({"url": url})),
            ),
        },
        CommAction::Daemon { action } => match action {
            CommDaemonAction::Status => {
                (reqwest::Method::GET, "/api/v1/comm/daemon".to_owned(), None)
            }
            CommDaemonAction::Start => (
                reqwest::Method::POST,
                "/api/v1/comm/daemon/start".to_owned(),
                None,
            ),
            CommDaemonAction::Stop => (
                reqwest::Method::POST,
                "/api/v1/comm/daemon/stop".to_owned(),
                None,
            ),
        },
        CommAction::Send {
            to,
            content,
            from,
            no_encrypt,
        } => (
            reqwest::Method::POST,
            "/api/v1/comm/send".to_owned(),
            Some(serde_json::json!({
                "to": to, "content": content, "from": from, "encrypt": !no_encrypt
            })),
        ),
        CommAction::History { as_, limit } => {
            let mut qs: Vec<String> = Vec::new();
            if let Some(a) = &as_ {
                qs.push(format!("as={}", qs_encode(a)));
            }
            if let Some(l) = limit {
                qs.push(format!("limit={l}"));
            }
            let path = if qs.is_empty() {
                "/api/v1/comm/history".to_owned()
            } else {
                format!("/api/v1/comm/history?{}", qs.join("&"))
            };
            (reqwest::Method::GET, path, None)
        }
        CommAction::Pull { as_ } => (
            reqwest::Method::POST,
            "/api/v1/comm/inbox/pull".to_owned(),
            Some(serde_json::json!({"as": as_})),
        ),
        CommAction::Outbox { action } => match action {
            CommOutboxAction::List { failed_only } => {
                let path = if failed_only {
                    "/api/v1/comm/outbox?failed_only=true".to_owned()
                } else {
                    "/api/v1/comm/outbox".to_owned()
                };
                (reqwest::Method::GET, path, None)
            }
            CommOutboxAction::Retry { event_id } => (
                reqwest::Method::POST,
                format!("/api/v1/comm/outbox/{}/retry", qs_encode(&event_id)),
                None,
            ),
            CommOutboxAction::Clear {
                confirm,
                min_failures,
            } => (
                reqwest::Method::POST,
                "/api/v1/comm/outbox/clear".to_owned(),
                Some(serde_json::json!({"confirm": confirm, "min_failures": min_failures})),
            ),
        },
    };
    let mut body = CommRequestBody(body);

    let ep = connect().await.map_err(|e| {
        format!("{e}\n  `agent24 comm` always goes through the daemon — there is no offline path")
    })?;
    let mut req = bearer(&ep, client().request(method, format!("{}{path}", ep.base)));
    if let Some(body) = &body.0 {
        req = req.json(body);
    }
    let result = req.timeout(timeout).send().await;
    body.zeroize_password();
    let out = match result {
        Ok(res) => {
            let status = res.status();
            let body: serde_json::Value = res.json().await.unwrap_or_default();
            if status.is_success() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&body["data"]).unwrap_or_default()
                );
                Ok(())
            } else {
                Err(format_comm_error(&body))
            }
        }
        Err(e) => Err(e.to_string()),
    };
    finish(ep).await;
    out
}

/// A comm JSON body can contain a password for import/unlock. Clear that
/// application-owned string as soon as the request has been serialized and
/// sent, and also on every earlier return path.
struct CommRequestBody(Option<serde_json::Value>);

impl CommRequestBody {
    fn zeroize_password(&mut self) {
        if let Some(serde_json::Value::String(password)) =
            self.0.as_mut().and_then(|body| body.get_mut("password"))
        {
            password.zeroize();
        }
    }
}

impl Drop for CommRequestBody {
    fn drop(&mut self) {
        self.zeroize_password();
    }
}

/// Disables local terminal echo on stdin for the lifetime of the guard,
/// restoring the original termios settings on drop — including on an
/// early return or a panic unwind (Codex 挑战 Medium #6: "异常退出时也要
/// 恢复"). Only ever constructed when stdin is actually a TTY; a non-TTY
/// pipe has no termios to touch and `disable` simply isn't called for it.
struct EchoGuard {
    original: rustix::termios::Termios,
    fd: rustix::fd::OwnedFd,
}

impl EchoGuard {
    /// Returns `None` (leaving the terminal untouched) if `tcgetattr`
    /// itself fails — a best-effort feature, not something worth failing
    /// the whole password read over.
    fn disable() -> Option<Self> {
        let stdin = std::io::stdin();
        Self::disable_fd(&stdin)
    }

    fn disable_fd(fd: impl rustix::fd::AsFd) -> Option<Self> {
        let owned_fd = rustix::io::dup(&fd).ok()?;
        let original = rustix::termios::tcgetattr(&owned_fd).ok()?;
        let mut silenced = original.clone();
        silenced
            .local_modes
            .remove(rustix::termios::LocalModes::ECHO);
        rustix::termios::tcsetattr(&owned_fd, rustix::termios::OptionalActions::Now, &silenced)
            .ok()?;
        Some(Self {
            original,
            fd: owned_fd,
        })
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        // Best-effort restore: if this fails, the user's shell is left
        // with echo off until they run `stty sane`/open a new shell —
        // there is no better fallback at this point, and panicking out of
        // a `Drop` would only make things worse.
        let _ = rustix::termios::tcsetattr(
            &self.fd,
            rustix::termios::OptionalActions::Now,
            &self.original,
        );
    }
}

/// Reads the Hyphae keystore password for `agent24 comm import` and
/// `agent24 comm unlock` from
/// stdin — never from a `--password`-style CLI flag, which would put the
/// plaintext on argv, visible to any other user on the same host via `ps`
/// (COMM-HYPHAE.md §3's rule for Hyphae's own `--password-stdin`, applied
/// here too). When stdin is a TTY, local echo is turned off for the
/// duration of the read (Codex 挑战 Medium #6) and always restored before
/// returning, including on an error path — a non-TTY pipe (the normal
/// `echo "$PASSWORD" | agent24 comm import ...` usage) is left completely
/// untouched. Reads one line and strips exactly one trailing `\n`/`\r\n`
/// (keeping any other whitespace the password itself might contain, same
/// rule the design doc uses for message content).
fn read_password_from_stdin() -> Result<Zeroizing<String>, String> {
    use std::io::Write;
    let is_tty = std::io::stdin().is_terminal();
    let _echo_guard = if is_tty {
        eprint!("Hyphae keystore password (input hidden): ");
        let _ = std::io::stderr().flush();
        EchoGuard::disable()
    } else {
        None
    };

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let result = read_password_line(&mut input);
    if is_tty {
        // With echo off, the Enter key the user pressed never printed a
        // newline on the terminal — print one ourselves so whatever comes
        // next doesn't start on the same line as the prompt.
        eprintln!();
    }
    // `_echo_guard` drops here on both success and error, restoring terminal echo.
    result
}

const PASSWORD_MAX_BYTES: usize = 4096;

fn read_password_line(reader: &mut impl BufRead) -> Result<Zeroizing<String>, String> {
    let mut bytes = Zeroizing::new(Vec::new());
    let mut limited = std::io::Read::take(&mut *reader, (PASSWORD_MAX_BYTES + 3) as u64);
    let read_result = limited.read_until(b'\n', &mut bytes);
    read_result.map_err(|e| format!("reading password from stdin: {e}"))?;

    let content = strip_one_password_line_ending(&bytes);
    if content.is_empty() {
        return Err(
            "no password was read from stdin; pipe the Hyphae keystore password in".to_owned(),
        );
    }
    if content.len() > PASSWORD_MAX_BYTES {
        return Err("password must be between 1 and 4096 UTF-8 bytes".to_owned());
    }
    let password =
        std::str::from_utf8(content).map_err(|_| "password must be valid UTF-8".to_owned())?;
    Ok(Zeroizing::new(password.to_owned()))
}

fn strip_one_password_line_ending(line: &[u8]) -> &[u8] {
    if let Some(without_lf) = line.strip_suffix(b"\n") {
        without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
    } else {
        line
    }
}

/// The v1 error envelope's `error.code`/`error.message`, as a bare JSON value
/// — `serde_json::Value::default()` reads as `null`, which is why every call
/// site below only trusts this when `["error"]["message"]` is actually a
/// string (same rule `daemon_error_line` already applies).
fn error_envelope_or(status: reqwest::StatusCode, body: &serde_json::Value) -> serde_json::Value {
    if body["error"]["message"].as_str().is_some() {
        body.clone()
    } else {
        serde_json::json!({"error": {"code": "daemon_unavailable", "message": format!("daemon returned {status}")}})
    }
}

fn daemon_unavailable_envelope(e: &str) -> serde_json::Value {
    serde_json::json!({"error": {"code": "daemon_unavailable", "message": e}})
}

/// §3.5/§3.6: whether to send `allow_relax: true`. Only when the caller asked
/// (`--allow-remote`) AND stdin is a TTY AND the prompt is answered "yes".
/// Run non-interactively — exactly how AgentEar's own auto-pairing invokes
/// this (§5.6) — this is always `false`, so a relaxing manifest is refused by
/// the daemon rather than silently approved by an unattended process.
fn resolve_allow_relax(allow_remote: bool) -> bool {
    if !allow_remote || !std::io::stdin().is_terminal() {
        return false;
    }
    use std::io::Write;
    eprint!(
        "this registration requests wider privacy (remote model access, or a new capability) — \
         type \"yes\" to confirm: "
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    line.trim() == "yes"
}

/// `agent24 os attach …` (A3-2a). Always goes through the daemon — see
/// `cmd_os`'s doc comment on why `Attach` is intercepted before that
/// function's own `req`/`out` match, which has different response shapes.
async fn cmd_os_attach(action: OsAttachAction) -> Result<(), String> {
    match action {
        OsAttachAction::Add {
            manifest,
            allow_remote,
            json,
        } => cmd_attach_add(&manifest, allow_remote, json).await,
        OsAttachAction::List { json } => cmd_attach_list(json).await,
        OsAttachAction::Remove { name, json } => cmd_attach_remove(&name, json).await,
    }
}

async fn cmd_attach_add(
    manifest_path: &std::path::Path,
    allow_remote: bool,
    json: bool,
) -> Result<(), String> {
    let manifest = std::fs::read_to_string(manifest_path).map_err(|e| {
        let msg = format!("cannot read {}: {e}", manifest_path.display());
        if json {
            println!(
                "{}",
                serde_json::json!({"error": {"code": "invalid_manifest", "message": msg}})
            );
        }
        msg
    })?;
    // Resolved BEFORE contacting the daemon: a TTY prompt after an ephemeral
    // daemon has already been spawned would be a strange place to block.
    let allow_relax = resolve_allow_relax(allow_remote);
    let ep = match connect().await {
        Ok(ep) => ep,
        Err(e) => {
            if json {
                println!("{}", daemon_unavailable_envelope(&e));
            }
            return Err(e);
        }
    };
    let sent = bearer(&ep, client().post(format!("{}/api/v1/attached", ep.base)))
        .json(&AttachedAddRequest {
            manifest,
            allow_relax,
        })
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    // No `?` below: any early return here would skip `finish(ep)` (the
    // ephemeral daemon it may have spawned would never be reaped), and under
    // `--json` a 2xx with an unreadable/unresizable body must still print
    // exactly one JSON object rather than nothing — the non-zero exit
    // already comes from returning `Err` below, `main` maps that to
    // `ExitCode::FAILURE`.
    let out = match sent {
        Ok(res) if res.status().is_success() => match res.json::<AttachedAddResponse>().await {
            Ok(body) => match serde_json::to_string(&body) {
                Ok(text) => {
                    if json {
                        // Exactly one JSON object on stdout (§3.6) — the same
                        // shape the daemon's own `201`/`200` body has.
                        println!("{text}");
                    } else {
                        println!("registered {} (token_id {})", body.name, body.token_id);
                        println!("  token (shown once — save it now): {}", body.token);
                        println!("  manifest digest: {}", body.manifest_digest);
                        println!("  socket: {}", body.socket_path);
                    }
                    Ok(())
                }
                Err(e) => {
                    let msg = format!("could not re-serialize the daemon's response: {e}");
                    if json {
                        println!("{}", daemon_unavailable_envelope(&msg));
                    }
                    Err(msg)
                }
            },
            Err(e) => {
                let msg = format!("daemon returned an unreadable response: {e}");
                if json {
                    println!("{}", daemon_unavailable_envelope(&msg));
                }
                Err(msg)
            }
        },
        Ok(res) => {
            let status = res.status();
            let body: serde_json::Value = res.json().await.unwrap_or_default();
            if json {
                println!("{}", error_envelope_or(status, &body));
            }
            Err(if body["error"]["message"].as_str().is_some() {
                daemon_error_line(&body)
            } else {
                format!("daemon returned {status}")
            })
        }
        Err(e) => {
            if json {
                println!("{}", daemon_unavailable_envelope(&e.to_string()));
            }
            Err(e.to_string())
        }
    };
    finish(ep).await;
    out
}

fn print_attached(list: &AttachedList) {
    if list.modules.is_empty() {
        println!("(no attached module registered)");
        return;
    }
    for m in &list.modules {
        println!("{}  [{}]  token_id {}", m.name, m.attach_status, m.token_id);
        println!(
            "    digest {}  registered {}",
            m.manifest_digest, m.created_at
        );
    }
}

async fn cmd_attach_list(json: bool) -> Result<(), String> {
    let ep = match connect().await {
        Ok(ep) => ep,
        Err(e) => {
            if json {
                println!("{}", daemon_unavailable_envelope(&e));
            }
            return Err(e);
        }
    };
    let sent = bearer(&ep, client().get(format!("{}/api/v1/attached", ep.base)))
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    // See `cmd_attach_add`'s comment: no `?` here either, for the same two
    // reasons (must not skip `finish(ep)`; `--json` must still emit exactly
    // one JSON object even on an unreadable/unresizable 2xx body).
    let out = match sent {
        Ok(res) if res.status().is_success() => match res.json::<AttachedList>().await {
            Ok(body) => match serde_json::to_string(&body) {
                Ok(text) => {
                    if json {
                        println!("{text}");
                    } else {
                        print_attached(&body);
                    }
                    Ok(())
                }
                Err(e) => {
                    let msg = format!("could not re-serialize the daemon's response: {e}");
                    if json {
                        println!("{}", daemon_unavailable_envelope(&msg));
                    }
                    Err(msg)
                }
            },
            Err(e) => {
                let msg = format!("daemon returned an unreadable response: {e}");
                if json {
                    println!("{}", daemon_unavailable_envelope(&msg));
                }
                Err(msg)
            }
        },
        Ok(res) => {
            let status = res.status();
            let body: serde_json::Value = res.json().await.unwrap_or_default();
            if json {
                println!("{}", error_envelope_or(status, &body));
            }
            Err(if body["error"]["message"].as_str().is_some() {
                daemon_error_line(&body)
            } else {
                format!("daemon returned {status}")
            })
        }
        Err(e) => {
            if json {
                println!("{}", daemon_unavailable_envelope(&e.to_string()));
            }
            Err(e.to_string())
        }
    };
    finish(ep).await;
    out
}

async fn cmd_attach_remove(name: &str, json: bool) -> Result<(), String> {
    let ep = match connect().await {
        Ok(ep) => ep,
        Err(e) => {
            if json {
                println!("{}", daemon_unavailable_envelope(&e));
            }
            return Err(e);
        }
    };
    let sent = bearer(
        &ep,
        client().delete(format!("{}/api/v1/attached/{name}", ep.base)),
    )
    .timeout(Duration::from_secs(10))
    .send()
    .await;
    let out = match sent {
        Ok(res) if res.status() == reqwest::StatusCode::NO_CONTENT => {
            if json {
                println!("{}", serde_json::json!({"removed": name}));
            } else {
                println!("removed {name}");
            }
            Ok(())
        }
        Ok(res) => {
            let status = res.status();
            let body: serde_json::Value = res.json().await.unwrap_or_default();
            if json {
                println!("{}", error_envelope_or(status, &body));
            }
            Err(if body["error"]["message"].as_str().is_some() {
                daemon_error_line(&body)
            } else {
                format!("daemon returned {status}")
            })
        }
        Err(e) => {
            if json {
                println!("{}", daemon_unavailable_envelope(&e.to_string()));
            }
            Err(e.to_string())
        }
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
        // Unreachable for the same reason as the `Install`/`Uninstall` arms
        // above are commented unreachable in `cmd_os`'s own match: `Attach`
        // returns from `cmd_os` before this function is ever called.
        OsAction::Attach { .. } => unreachable!(),
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
                if health_ok(&base, discovery_health_token(&state)).await {
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
                            if health_ok(&base, discovery_health_token(&state)).await {
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
                if health_ok(&base, "").await {
                    let res = client()
                        .get(format!("{base}/api/v1/health"))
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
                    if state.auth_mode.is_capabilities() {
                        println!("  shutdown report unavailable: {HOST_AUTHORITY_UNAVAILABLE}");
                    } else {
                        match client()
                            .get(format!("{base}/api/v1/shutdown"))
                            .bearer_auth(host_token(&state).map_err(|e| e.to_owned())?)
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
                if state.auth_mode.is_capabilities() {
                    return Err(HOST_AUTHORITY_UNAVAILABLE.to_owned());
                }
                // Authenticated shutdown: the bearer token proves this is OUR
                // daemon — a reused pid of an unrelated process can never be
                // hit (review B6)
                let base = format!("http://127.0.0.1:{}", state.port);
                let res = client()
                    .post(format!("{base}/api/v1/shutdown"))
                    .bearer_auth(host_token(&state).map_err(|e| e.to_owned())?)
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
        Command::Comm { action } => cmd_comm(action).await,
        Command::Mcp => cmd_mcp().await,
        Command::Acp => cmd_acp().await,
        Command::Decide { action } => cmd_decide(action).await,
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

    // ── D0-2 `agent24 decide` guards (pre-pr-check T2) ──────────────────────

    /// Removing the `require_jsonl` call from `cmd_decide` (or short-circuiting
    /// it to always `Ok(())`) makes this test red: `jsonl: false` must be
    /// rejected, not silently treated as "export anyway".
    #[test]
    fn jsonl_flag_is_required_and_checked_before_touching_the_store() {
        assert!(require_jsonl(false).is_err());
        assert!(require_jsonl(true).is_ok());
    }

    /// Same for `delete all`'s `--confirm`: removing `require_confirm`'s call
    /// in `cmd_decide_delete` (or inlining `confirm` as always-true) makes
    /// this red.
    #[test]
    fn confirm_flag_is_required_for_delete_all() {
        assert!(require_confirm(false).is_err());
        assert!(require_confirm(true).is_ok());
    }

    /// S3: both guards are evaluated by `cmd_decide`/`cmd_decide_delete`
    /// themselves, at the point the action would actually run (export's
    /// query, delete's `DELETE`) — not once at CLI entry/arg-parsing time
    /// and then trusted for the rest of the call. There is no separate
    /// "validate the whole `Cli` up front" pass whose result could go stale
    /// between parsing and execution: `require_jsonl`/`require_confirm` are
    /// called with the SAME `bool` clap just parsed, in the same function
    /// that performs the action, on every invocation (a fresh CLI process
    /// each time — there is no cached/startup-time decision to go stale).
    #[tokio::test]
    async fn delete_all_without_confirm_is_rejected_and_deletes_nothing() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&agent24_store::NewDecisionLogEntry {
                decision_id: "d1".to_owned(),
                ts: "2026-10-07T00:00:00Z".to_owned(),
                schema_version: 1,
                point: "retain.intent".to_owned(),
                input: Some("记住我对花生过敏".to_owned()),
                context_json: Some("{}".to_owned()),
                question_json: "[]".to_owned(),
                layers_json: "[]".to_owned(),
                final_action: "execute".to_owned(),
                hw_tier: None,
            })
            .await
            .unwrap();

        let result = cmd_decide_delete(&store, DecideDeleteAction::All { confirm: false }).await;
        assert!(result.is_err());

        let remaining = store.export_decision_log(None, None).await.unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "a rejected --confirm must not delete anything"
        );
    }

    #[test]
    fn decision_log_export_line_uses_the_key_final_not_final_action_and_nests_json() {
        let row = agent24_store::DecisionLogExportRow {
            log: agent24_store::DecisionLogRow {
                decision_id: "d1".to_owned(),
                ts: "2026-10-07T00:00:00Z".to_owned(),
                schema_version: 1,
                point: "retain.intent".to_owned(),
                input: Some("记住我对花生过敏".to_owned()),
                context_json: Some(r#"{"turn":1}"#.to_owned()),
                question_json: r#"[{"kind":"noul","id":"q1"}]"#.to_owned(),
                layers_json: r#"[{"backend":"rule"}]"#.to_owned(),
                final_action: "execute".to_owned(),
                hw_tier: Some("t2".to_owned()),
                scrubbed_at: None,
            },
            outcomes: vec![agent24_store::DecisionOutcomeRow {
                ts: "2026-10-07T00:01:00Z".to_owned(),
                signal: "clarify_answer".to_owned(),
                label_json: r#"{"answer":true}"#.to_owned(),
                quality: "high".to_owned(),
            }],
        };
        let line = decision_log_export_line(&row).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["final"], "execute");
        assert!(v.get("final_action").is_none());
        assert_eq!(
            v["context"]["turn"], 1,
            "context must be nested JSON, not a string"
        );
        assert_eq!(v["outcomes"][0]["label"]["answer"], true);
    }

    #[test]
    fn old_ready_line_requires_and_keeps_legacy_token() {
        let ready = serde_json::json!({
            "type": "ready", "port": 8080, "token": "legacy", "version": "v"
        });
        let state = parse_ready_state(&ready, 7).unwrap();
        assert_eq!(
            state.auth_mode,
            agent24_protocol::state_file::AuthMode::LegacySingleToken
        );
        assert_eq!(host_token(&state), Ok("legacy"));
    }

    /// Codex 挑战（B 端 gpt-6-astra）Medium：`send` 返回 502 partial 时，CLI
    /// 必须把 event_id、relay 接受情况（`published_to`）和记账异常
    /// （`audit_error`）打出来，不能只剩 `error`/`message` 两行字。
    #[test]
    fn format_comm_error_includes_partial_event_id_and_audit_error() {
        let body = serde_json::json!({
            "ok": false,
            "error": "partial",
            "message": "hyphae reported a partial result",
            "data": {
                "event_id": "e9",
                "published_to": 1,
                "history_stored": true,
                "audit_error": "permission denied"
            }
        });
        let out = format_comm_error(&body);
        assert!(out.contains("partial"), "{out}");
        assert!(out.contains("e9"), "{out}");
        assert!(out.contains("published_to"), "{out}");
        assert!(out.contains("permission denied"), "{out}");
    }

    /// Same bug, `outbox retry`'s `write_conflict`: `sent`/`superseded` and
    /// the retried event_id must show up too.
    #[test]
    fn format_comm_error_includes_conflict_sent_and_superseded() {
        let body = serde_json::json!({
            "ok": false,
            "error": "conflict",
            "message": "queue entry superseded",
            "data": {"event_id": "e2", "sent": true, "superseded": true}
        });
        let out = format_comm_error(&body);
        assert!(out.contains("e2"), "{out}");
        assert!(out.contains("\"sent\": true"), "{out}");
        assert!(out.contains("\"superseded\": true"), "{out}");
    }

    /// Routes with no `data` (identity/contact/relay/…) must keep the exact
    /// old one-line shape — no empty `data:` section appended.
    #[test]
    fn format_comm_error_without_data_is_unchanged() {
        let body = serde_json::json!({
            "ok": false,
            "error": "not_configured",
            "message": "no default identity"
        });
        let out = format_comm_error(&body);
        assert_eq!(out, "comm: not_configured — no default identity");
    }

    #[test]
    fn comm_unlock_has_only_the_remember_flag() {
        let cli = Cli::try_parse_from(["agent24", "comm", "unlock", "--remember"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Comm {
                action: CommAction::Unlock { remember: true }
            }
        ));
    }

    #[test]
    fn password_line_removes_only_one_lf_or_crlf_and_keeps_spaces() {
        assert_eq!(strip_one_password_line_ending(b" pass \n"), b" pass ");
        assert_eq!(strip_one_password_line_ending(b"pass\r\n"), b"pass");
        assert_eq!(strip_one_password_line_ending(b"pass\r"), b"pass\r");
        assert_eq!(strip_one_password_line_ending(b"pass\r\r\n"), b"pass\r");
        assert_eq!(strip_one_password_line_ending(b"no-newline"), b"no-newline");
    }

    #[test]
    fn password_line_enforces_utf8_byte_limit_and_reads_bounded_without_newline() {
        use std::io::Cursor;

        let max_utf8 = "é".repeat(2048);
        let mut valid = Cursor::new(max_utf8.as_bytes());
        assert_eq!(read_password_line(&mut valid).unwrap().len(), 4096);

        let too_many_utf8 = "é".repeat(2049);
        let mut over = Cursor::new(too_many_utf8.as_bytes());
        assert!(read_password_line(&mut over).is_err());

        let long_without_newline = vec![b'x'; 100_000];
        let mut bounded = Cursor::new(long_without_newline);
        assert!(read_password_line(&mut bounded).is_err());
        assert_eq!(bounded.position(), (PASSWORD_MAX_BYTES + 3) as u64);
    }

    #[test]
    fn echo_guard_restores_terminal_after_early_error() {
        fn fail_with_echo_disabled(slave: &std::fs::File) -> Result<(), ()> {
            let _guard = EchoGuard::disable_fd(slave).unwrap();
            assert!(
                !rustix::termios::tcgetattr(slave)
                    .unwrap()
                    .local_modes
                    .contains(rustix::termios::LocalModes::ECHO)
            );
            Err(())
        }

        let master =
            rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
                .unwrap();
        rustix::pty::grantpt(&master).unwrap();
        rustix::pty::unlockpt(&master).unwrap();
        let slave_name = rustix::pty::ptsname(&master, Vec::new()).unwrap();
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(slave_name.to_str().unwrap())
            .unwrap();
        let initially_echo = rustix::termios::tcgetattr(&slave)
            .unwrap()
            .local_modes
            .contains(rustix::termios::LocalModes::ECHO);
        let result = fail_with_echo_disabled(&slave);
        assert!(result.is_err());
        assert_eq!(
            rustix::termios::tcgetattr(&slave)
                .unwrap()
                .local_modes
                .contains(rustix::termios::LocalModes::ECHO),
            initially_echo
        );
    }

    #[test]
    fn comm_request_body_zeroizes_password_field() {
        let mut body = CommRequestBody(Some(serde_json::json!({"password":"secret"})));
        body.zeroize_password();
        assert_eq!(body.0.as_ref().unwrap()["password"], "");
    }

    #[test]
    fn capability_ready_line_never_mints_a_host_token() {
        let ready = serde_json::json!({
            "type": "ready", "port": 8080, "version": "v",
            "auth_mode": "capabilities", "generation": "gen-1"
        });
        let state = parse_ready_state(&ready, 7).unwrap();
        assert!(state.token.is_empty());
        assert_eq!(
            host_token(&state),
            Err(HOST_AUTHORITY_UNAVAILABLE.to_owned())
        );
    }

    #[test]
    fn capability_ready_line_with_token_fails_closed() {
        let ready = serde_json::json!({
            "type": "ready", "port": 8080, "token": "must-not-be-here",
            "auth_mode": "capabilities", "generation": "gen-1"
        });
        let err = parse_ready_state(&ready, 7).unwrap_err();
        assert!(err.contains("must not contain a token"), "{err}");
    }

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

    // ── FU-74: the daemon-facing `client()` must not honour HTTP_PROXY ─────
    //
    // Same shape as agent24-models' `from_env_local_providers_ignore_http_proxy`
    // and agent24-worker's `http_ml_worker_ignores_http_proxy`: a child process
    // gets HTTP_PROXY/ALL_PROXY pointed at a proxy stub and no NO_PROXY, then
    // makes one request; the proxy stub's connection count tells us whether the
    // client obeyed the proxy env. A positive control (plain
    // `reqwest::Client::builder()...build()`, no `no_proxy()`) proves the env
    // was actually in effect for the child.
    //
    // `apps/agent24-cli` is NOT scanned by
    // `passthrough_list_matches_what_the_daemon_actually_reads` (that scanner
    // only walks `apps/agent24d/src` and `crates/`), so the child's target-port
    // env var can use an ordinary SCREAMING_SNAKE_CASE name without tripping it.

    /// A blocking stub on its own thread: counts connections, answers `reply`.
    fn thread_stub(reply: String) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n2 = n.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                n2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
                let mut buf = [0u8; 65536];
                let _ = s.read(&mut buf);
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (port, n)
    }

    fn health_ok_reply() -> String {
        let body = r#"{"status":"ok"}"#;
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn target_port() -> u16 {
        std::env::var("AGENT24_CLI_TEST_TARGET_PORT")
            .expect("run_child must set AGENT24_CLI_TEST_TARGET_PORT")
            .parse()
            .expect("AGENT24_CLI_TEST_TARGET_PORT must be a u16")
    }

    /// Child: the production path — `client()` must be loopback-only.
    #[tokio::test]
    #[ignore = "child process of cli_client_ignores_http_proxy"]
    async fn proxy_child_cli_client() {
        let url = format!("http://127.0.0.1:{}/api/v1/health", target_port());
        let _ = client()
            .get(url)
            .timeout(Duration::from_millis(500))
            .send()
            .await;
    }

    /// Child: positive control — the bare default client, same URL, same env.
    #[tokio::test]
    #[ignore = "child process of cli_client_ignores_http_proxy"]
    async fn proxy_child_default_client() {
        let url = format!("http://127.0.0.1:{}/api/v1/health", target_port());
        let raw_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let _ = raw_client
            .get(url)
            .timeout(Duration::from_millis(500))
            .send()
            .await;
    }

    fn run_child(test: &str, target: u16, proxy: u16) {
        let exe = std::env::current_exe().unwrap();
        let proxy_url = format!("http://127.0.0.1:{proxy}");
        let status = std::process::Command::new(exe)
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env("AGENT24_CLI_TEST_TARGET_PORT", target.to_string())
            .env("HTTP_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn cli_client_ignores_http_proxy() {
        let (tp, target) = thread_stub(health_ok_reply());
        let (pp, proxy) = thread_stub(health_ok_reply());
        run_child("tests::proxy_child_cli_client", tp, pp);
        assert_eq!(
            proxy.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the proxy saw the CLI daemon client's request"
        );
        assert_eq!(target.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Positive control: the default client under the same env goes through
        // the proxy — proves HTTP_PROXY was actually live for the child.
        let (tp2, target2) = thread_stub(health_ok_reply());
        let (pp2, proxy2) = thread_stub(health_ok_reply());
        run_child("tests::proxy_child_default_client", tp2, pp2);
        assert_eq!(
            proxy2.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "measuring instrument: proxy env must take effect"
        );
        assert_eq!(target2.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
