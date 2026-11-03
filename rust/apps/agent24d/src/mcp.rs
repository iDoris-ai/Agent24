//! Mounting external MCP servers into the daemon (M-E / E1b).
//!
//! Config lives at `~/.agent24/mcp.json` and deliberately uses the SAME shape
//! the rest of the MCP ecosystem uses (Claude Desktop, Cursor, …):
//!
//! ```json
//! { "mcpServers": { "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"] } } }
//! ```
//!
//! Sharing the shape means an existing config can be copied over unchanged
//! instead of being re-authored in a bespoke format.
//!
//! Two safety properties this module must preserve:
//! - a broken or hostile server must never stop the daemon from starting, so
//!   every connection is bounded by a timeout and failures are logged and
//!   skipped rather than propagated;
//! - the connected handles must be kept alive for the daemon's lifetime —
//!   dropping an `McpServer` kills its child process, so losing the handle
//!   would silently break every tool it contributed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use agent24_components::ComponentState;
use agent24_mcp::{McpError, McpServer, McpServerSpec};
use agent24_tools::Tool;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::component_manifest::capability_id_ok;

/// Per-server connect budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);

/// How often a mounted server is asked whether its child is still there.
const LIVENESS_POLL: Duration = Duration::from_millis(200);

/// Budget for mounting ALL servers, enforced across the whole loop.
///
/// This MUST stay comfortably below the CLI's 15s ready-line deadline
/// (`agent24-cli` `spawn_daemon`): mounting happens BEFORE the ready line, and
/// servers are mounted sequentially, so a per-server timeout alone could exceed
/// that deadline in aggregate. When it did, the CLI declared the daemon dead —
/// and in ephemeral mode (`kill_on_drop`) actually killed an otherwise-healthy
/// daemon (reviewer-found). Bounding the total, not just each server, is what
/// makes that impossible regardless of how many servers are configured.
const MOUNT_TOTAL_BUDGET: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize, Default)]
pub struct McpConfig {
    #[serde(rename = "mcpServers", default)]
    pub servers: BTreeMap<String, ServerEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ServerEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Set false to keep an entry in the file without mounting it.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Which input field names *where a call sends things* (H4), per tool —
    /// `{"post_message": "channel"}`. A tool listed here becomes eligible for a
    /// target-scoped standing grant; one that is not stays "ask every time".
    ///
    /// Declared by the user rather than guessed from the schema. Guessing binds
    /// a lasting authorisation to whichever field a heuristic liked, and the
    /// cost of getting that wrong is a permission nobody meant to give. Tool
    /// names here are the server's own (unqualified) names.
    #[serde(default, rename = "targetArgs")]
    pub target_args: BTreeMap<String, String>,
    /// FU-103/J-6: this server's own environment variables (Claude Desktop's
    /// `env` shape: `{"command","args","env":{...}}`). The server's child
    /// process no longer inherits the daemon's ambient environment at all —
    /// a credential it genuinely needs (its own API token, say) must be
    /// given here, explicitly, by the user, and reaches ONLY this server's
    /// child process, never any other server's or `shell_exec`'s.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn default_true() -> bool {
    true
}

pub fn config_path() -> Option<PathBuf> {
    agent24_protocol::state_file::state_dir().map(|d| d.join("mcp.json"))
}

/// Parse a config. A malformed file is an error the caller reports and skips —
/// never a reason to fail startup.
pub fn parse_config(raw: &str) -> Result<McpConfig, String> {
    serde_json::from_str(raw).map_err(|e| e.to_string())
}

/// Read the config file; absent means "no MCP servers", not an error.
pub fn load_config(path: &Path) -> Result<McpConfig, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => parse_config(&raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(McpConfig::default()),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

impl McpConfig {
    /// Enabled entries as connection specs, in a stable order.
    /// Production mounting uses [`Self::targets`] so a disabled server still
    /// gets a component row. This view remains for the parser tests.
    #[cfg(test)]
    pub fn specs(&self) -> Vec<McpServerSpec> {
        self.targets()
            .into_iter()
            .filter(|t| t.enabled)
            .map(|t| t.spec)
            .collect()
    }

    /// Every configured server, including ones the user switched off.
    pub fn targets(&self) -> Vec<McpTarget> {
        self.servers
            .iter()
            .map(|(name, e)| McpTarget {
                spec: McpServerSpec::new(name.clone(), e.command.clone(), e.args.clone())
                    .with_target_args(e.target_args.clone())
                    .with_env(e.env.clone()),
                enabled: e.enabled,
            })
            .collect()
    }
}

/// One configured MCP server, whether or not it should be connected.
pub struct McpTarget {
    pub spec: McpServerSpec,
    pub enabled: bool,
}

/// What `/components` needs for one MCP server. The live handle, when there is
/// one, stays in [`McpRoster`] so dropping the snapshot does not kill the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpRecord {
    /// Server name as written in `mcp.json`.
    pub server: String,
    /// Component id. `mcp-<server>` when that is a legal capability id.
    pub id: String,
    pub state: ComponentState,
    pub reason: Option<String>,
    /// Set when `id` is a legal capability id. Absent ids are not provides.
    pub capability_id: Option<String>,
    /// Qualified tool names from `tools/list`. Empty when the list never arrived.
    pub tools: Vec<String>,
    /// MCP `protocolVersion`, recorded and not compared (D3 does not apply).
    pub protocol_version: Option<String>,
}

struct McpSlot {
    record: McpRecord,
    /// Same RAII guard as `AppState::mcp_servers`: dropping the last clone
    /// kills the child. Nothing reads the handle; it has to stay here.
    #[allow(dead_code, reason = "RAII: keeps MCP child processes alive")]
    server: Option<Arc<McpServer>>,
}

/// Configured MCP servers and the D5 state C7 keeps for them.
///
/// The handles inside are the same RAII guard `AppState::mcp_servers` holds:
/// dropping the last one kills the child. Status is updated in place when the
/// child exits or this daemon asks it to stop.
#[derive(Default)]
pub struct McpRoster {
    slots: Mutex<Vec<McpSlot>>,
}

impl McpRoster {
    #[cfg(test)]
    pub(crate) fn from_records(records: Vec<McpRecord>) -> Self {
        Self {
            slots: Mutex::new(
                records
                    .into_iter()
                    .map(|record| McpSlot {
                        record,
                        server: None,
                    })
                    .collect(),
            ),
        }
    }

    pub(crate) fn records(&self) -> Vec<McpRecord> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|s| s.record.clone())
            .collect()
    }

    /// A child that was `starting` or `ready` ended.
    ///
    /// D5: a start that dies, or a crash we did not ask for, is `failed`. Our
    /// own stop (daemon shutdown, the server's cancel token) is `stopped`.
    /// MCP has no `stopping`. Anything already terminal is left alone.
    pub(crate) fn note_liveness(&self, id: &str, stopped_by_us: bool) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(slot) = slots.iter_mut().find(|s| s.record.id == id) else {
            return;
        };
        if !matches!(
            slot.record.state,
            ComponentState::Starting | ComponentState::Ready
        ) {
            return;
        }
        if stopped_by_us {
            slot.record.state = ComponentState::Stopped;
            slot.record.reason = Some("stopped".to_owned());
        } else {
            slot.record.state = ComponentState::Failed;
            slot.record.reason = Some("process_exited".to_owned());
        }
    }

    fn push(&self, record: McpRecord, server: Option<Arc<McpServer>>) {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(McpSlot { record, server });
    }
}

/// Capability id for one MCP server (ADR-038 D9): `mcp-<server>`, normalized
/// onto the capability-id alphabet. `None` when the result is not a legal id
/// (empty, or longer than 32). Those servers are not given a provides entry.
pub(crate) fn mcp_capability_id(server: &str) -> Option<String> {
    let mut body = String::new();
    for c in server.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            body.push(c);
        } else {
            body.push('-');
        }
    }
    let body = body.trim_matches('-');
    if body.is_empty() || body.len() > 28 {
        return None;
    }
    let id = format!("mcp-{body}");
    capability_id_ok(&id).then_some(id)
}

fn fallback_component_id(server: &str) -> String {
    let mut hash: u32 = 2_166_136_261;
    for b in server.as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(16_777_619);
    }
    format!("mcp-bad-{hash:08x}")
}

/// Connect every configured server and collect their tools.
///
/// Returns the live handles (which the caller MUST keep alive) alongside the
/// tools to register, and a roster that also remembers the servers that did
/// not come up. Individual failures are logged and recorded as `failed`; they
/// are never propagated, so one broken server cannot stop the daemon.
pub async fn mount(
    targets: &[McpTarget],
    cancel: &CancellationToken,
) -> (Vec<Arc<McpServer>>, Vec<Arc<dyn Tool>>, Arc<McpRoster>) {
    let roster = Arc::new(McpRoster::default());
    let mut servers = Vec::new();
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut seen_tools: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut seen_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let deadline = tokio::time::Instant::now() + MOUNT_TOTAL_BUDGET;
    for target in targets {
        let spec = &target.spec;
        let capability_id = mcp_capability_id(&spec.name);
        let id = capability_id
            .clone()
            .unwrap_or_else(|| fallback_component_id(&spec.name));
        if !seen_ids.insert(id.clone()) {
            tracing::error!(
                "MCP server {} not mounted: capability id {id} is already used",
                spec.name
            );
            // A second row must not reuse the first server's component id.
            roster.push(
                McpRecord {
                    server: spec.name.clone(),
                    id: fallback_component_id(&spec.name),
                    state: ComponentState::Failed,
                    reason: Some("duplicate_capability_id".to_owned()),
                    capability_id: None,
                    tools: Vec::new(),
                    protocol_version: None,
                },
                None,
            );
            continue;
        }
        if capability_id.is_none() {
            tracing::error!(
                "MCP server {} not mounted: its name is not a capability id under mcp-<server>",
                spec.name
            );
            roster.push(
                McpRecord {
                    server: spec.name.clone(),
                    id,
                    state: ComponentState::Failed,
                    reason: Some("invalid_capability_id".to_owned()),
                    capability_id: None,
                    tools: Vec::new(),
                    protocol_version: None,
                },
                None,
            );
            continue;
        }
        if !target.enabled {
            roster.push(
                McpRecord {
                    server: spec.name.clone(),
                    id,
                    state: ComponentState::Stopped,
                    reason: Some("disabled".to_owned()),
                    capability_id,
                    tools: Vec::new(),
                    protocol_version: None,
                },
                None,
            );
            continue;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::error!(
                "MCP server {} not mounted: total mount budget {MOUNT_TOTAL_BUDGET:?} exhausted",
                spec.name
            );
            roster.push(
                failed_record(spec, id, capability_id, "start_timeout"),
                None,
            );
            continue;
        }
        let budget = CONNECT_TIMEOUT.min(remaining);
        // A child token so this server's own teardown does not cancel the daemon.
        // Cancelling `cancel` still cancels the child (daemon shutdown).
        let server_cancel = cancel.child_token();
        let connect = agent24_mcp::connect_and_build_tools(spec, server_cancel.clone());
        match tokio::time::timeout(budget, connect).await {
            Ok(Ok((server, built))) => {
                // Belt-and-braces after the separator fix: distinct raw names can
                // still sanitize to the same string (e.g. `a.b` and `a-b`), and
                // ToolRegistry::with() is a plain insert that would silently
                // shadow. Refuse the duplicate loudly instead.
                let mut kept: Vec<Arc<dyn Tool>> = Vec::new();
                let mut qualified = Vec::new();
                for tool in built {
                    let name = tool.info().name;
                    if !seen_tools.insert(name.clone()) {
                        tracing::error!(
                            "MCP tool {name} from server {} collides with an already \
                             registered tool and was NOT registered — rename the server",
                            spec.name
                        );
                        continue;
                    }
                    qualified.push(name);
                    kept.push(tool);
                }
                qualified.sort();
                let protocol_version = server.protocol_version();
                tracing::info!(
                    "mounted MCP server {} with {} tool(s)",
                    spec.name,
                    kept.len()
                );
                let record = McpRecord {
                    server: spec.name.clone(),
                    id: id.clone(),
                    state: ComponentState::Ready,
                    reason: None,
                    capability_id,
                    tools: qualified,
                    protocol_version,
                };
                roster.push(record, Some(Arc::clone(&server)));
                watch_liveness(id, Arc::clone(&server), Arc::clone(&roster), server_cancel);
                servers.push(server);
                tools.extend(kept);
            }
            Ok(Err(err)) => {
                tracing::error!("MCP server {} not mounted: {err}", spec.name);
                let reason = match err {
                    McpError::Spawn(_, _) => "spawn_failed",
                    McpError::Handshake(_, _) => "handshake_failed",
                    McpError::Call(_) | McpError::Cancelled => "handshake_failed",
                };
                roster.push(failed_record(spec, id, capability_id, reason), None);
            }
            Err(_) => {
                tracing::error!(
                    "MCP server {} not mounted: handshake exceeded its {budget:?} budget",
                    spec.name
                );
                roster.push(
                    failed_record(spec, id, capability_id, "start_timeout"),
                    None,
                );
            }
        }
    }
    (servers, tools, roster)
}

fn failed_record(
    spec: &McpServerSpec,
    id: String,
    capability_id: Option<String>,
    reason: &str,
) -> McpRecord {
    McpRecord {
        server: spec.name.clone(),
        id,
        state: ComponentState::Failed,
        reason: Some(reason.to_owned()),
        capability_id,
        tools: Vec::new(),
        protocol_version: None,
    }
}

/// `stopped` when we cancelled the server, `failed` when the child died on its own.
fn watch_liveness(
    id: String,
    server: Arc<McpServer>,
    roster: Arc<McpRoster>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            if cancel.is_cancelled() {
                roster.note_liveness(&id, true);
                return;
            }
            if server.transport_closed() {
                roster.note_liveness(&id, false);
                return;
            }
            tokio::time::sleep(LIVENESS_POLL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn parses_the_ecosystem_standard_shape() {
        // Same key layout as Claude Desktop / Cursor, so a config can be copied.
        let cfg = parse_config(
            r#"{"mcpServers":{"files":{"command":"npx","args":["-y","pkg","/tmp"]}}}"#,
        )
        .unwrap();
        let specs = cfg.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "files");
        assert_eq!(specs[0].command, "npx");
        assert_eq!(specs[0].args, vec!["-y", "pkg", "/tmp"]);
    }

    #[test]
    fn args_are_optional_and_entries_default_to_enabled() {
        let cfg = parse_config(r#"{"mcpServers":{"a":{"command":"x"}}}"#).unwrap();
        assert_eq!(cfg.specs().len(), 1);
        assert!(cfg.specs()[0].args.is_empty());
    }

    #[test]
    fn disabled_entries_stay_in_the_file_but_are_not_mounted() {
        let cfg = parse_config(
            r#"{"mcpServers":{"on":{"command":"a"},"off":{"command":"b","enabled":false}}}"#,
        )
        .unwrap();
        let specs = cfg.specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["on"]);
    }

    #[test]
    fn empty_or_absent_config_yields_no_servers() {
        assert!(parse_config("{}").unwrap().specs().is_empty());
        assert!(
            parse_config(r#"{"mcpServers":{}}"#)
                .unwrap()
                .specs()
                .is_empty()
        );
        // A missing file is "no servers", not a startup failure.
        let missing = Path::new("/definitely/not/here/mcp.json");
        assert!(load_config(missing).unwrap().specs().is_empty());
    }

    #[test]
    fn malformed_config_is_an_error_not_a_panic() {
        assert!(parse_config("{ not json").is_err());
        assert!(parse_config(r#"{"mcpServers":{"a":{}}}"#).is_err()); // command required
    }

    #[test]
    fn mount_budget_stays_under_the_cli_ready_deadline() {
        // The CLI (agent24-cli spawn_daemon) waits 15s for the ready line, and
        // mounting runs BEFORE that line. If this budget ever creeps past it,
        // a slow server makes the CLI declare a healthy daemon dead — and kill
        // it in ephemeral mode. Pin the relationship so it can't drift silently.
        const CLI_READY_DEADLINE: Duration = Duration::from_secs(15);
        assert!(
            MOUNT_TOTAL_BUDGET < CLI_READY_DEADLINE,
            "mount budget {MOUNT_TOTAL_BUDGET:?} must stay under the CLI's {CLI_READY_DEADLINE:?}"
        );
        // And a single server can never consume the whole budget on its own.
        assert!(CONNECT_TIMEOUT <= MOUNT_TOTAL_BUDGET);
    }

    #[tokio::test]
    async fn total_budget_is_enforced_across_servers_not_just_each_one() {
        // Several broken servers must not add up past the total budget.
        let targets: Vec<McpTarget> = (0..5)
            .map(|i| McpTarget {
                spec: McpServerSpec::new(format!("broken{i}"), "/definitely/not/real", vec![]),
                enabled: true,
            })
            .collect();
        let started = std::time::Instant::now();
        let (servers, tools, roster) = mount(&targets, &CancellationToken::new()).await;
        assert!(servers.is_empty() && tools.is_empty());
        assert_eq!(roster.records().len(), 5);
        assert!(
            roster
                .records()
                .iter()
                .all(|r| r.state == ComponentState::Failed)
        );
        assert!(
            started.elapsed() < MOUNT_TOTAL_BUDGET + Duration::from_secs(2),
            "mount overran its total budget: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_server_that_cannot_start_is_skipped_not_fatal() {
        // The daemon must still come up when a configured server is broken.
        let targets = vec![McpTarget {
            spec: McpServerSpec::new("broken", "/definitely/not/a/real/binary", vec![]),
            enabled: true,
        }];
        let (servers, tools, roster) = mount(&targets, &CancellationToken::new()).await;
        assert!(servers.is_empty());
        assert!(tools.is_empty());
        let record = &roster.records()[0];
        assert_eq!(record.state, ComponentState::Failed);
        assert_eq!(record.reason.as_deref(), Some("spawn_failed"));
        assert_eq!(record.id, "mcp-broken");
        assert!(record.capability_id.is_some());
        assert!(record.tools.is_empty());
    }

    /// H4: a tool is eligible for a target-scoped standing grant only if the
    /// USER named its target argument. Absent means "ask every time", which is
    /// the right default for a schema we cannot interpret.
    #[test]
    fn target_args_are_read_per_tool_and_default_to_none() {
        let cfg = parse_config(
            r#"{"mcpServers":{"slack":{"command":"x","targetArgs":{"post_message":"channel"}}}}"#,
        )
        .unwrap();
        let spec = &cfg.specs()[0];
        assert_eq!(
            spec.target_args.get("post_message").map(String::as_str),
            Some("channel")
        );
        assert_eq!(spec.target_args.get("list_channels"), None);
    }

    /// An entry with no targetArgs at all parses fine and grants nothing.
    #[test]
    fn a_server_without_target_args_is_never_grant_eligible() {
        let cfg = parse_config(r#"{"mcpServers":{"fs":{"command":"x"}}}"#).unwrap();
        assert!(cfg.specs()[0].target_args.is_empty());
    }

    /// FU-103/J-6: `mcp.json`'s Claude-Desktop-standard `env` block is parsed
    /// and reaches that server's own spec — it used to be silently dropped
    /// (no `env` field on `ServerEntry` at all), which meant a server that
    /// legitimately needed a token had no way to get one now that the daemon
    /// stopped handing child processes its own ambient environment.
    #[test]
    fn a_servers_env_block_is_parsed_onto_its_own_spec() {
        let cfg = parse_config(
            r#"{"mcpServers":{"slack":{"command":"x","env":{"SLACK_TOKEN":"xoxb-123"}}}}"#,
        )
        .unwrap();
        let spec = &cfg.specs()[0];
        assert_eq!(
            spec.env.get("SLACK_TOKEN").map(String::as_str),
            Some("xoxb-123")
        );
    }

    /// Server A's `env` block must never bleed into server B's spec — each
    /// server's env is scoped to its own entry in the config.
    #[test]
    fn each_servers_env_block_stays_scoped_to_that_server() {
        let cfg = parse_config(
            r#"{"mcpServers":{
                "a":{"command":"x","env":{"A_TOKEN":"a-secret"}},
                "b":{"command":"y","env":{"B_TOKEN":"b-secret"}}
            }}"#,
        )
        .unwrap();
        let specs = cfg.specs();
        let a = specs.iter().find(|s| s.name == "a").unwrap();
        let b = specs.iter().find(|s| s.name == "b").unwrap();
        assert_eq!(a.env.get("A_TOKEN").map(String::as_str), Some("a-secret"));
        assert!(!a.env.contains_key("B_TOKEN"));
        assert_eq!(b.env.get("B_TOKEN").map(String::as_str), Some("b-secret"));
        assert!(!b.env.contains_key("A_TOKEN"));
    }

    /// An entry with no `env` block at all parses fine and grants nothing
    /// beyond the fixed child-process whitelist.
    #[test]
    fn a_server_without_an_env_block_gets_an_empty_env() {
        let cfg = parse_config(r#"{"mcpServers":{"fs":{"command":"x"}}}"#).unwrap();
        assert!(cfg.specs()[0].env.is_empty());
    }

    #[test]
    fn capability_ids_follow_the_mcp_server_rule() {
        assert_eq!(mcp_capability_id("Files").as_deref(), Some("mcp-files"));
        assert_eq!(mcp_capability_id("a_b").as_deref(), Some("mcp-a-b"));
        assert_eq!(mcp_capability_id("--x--").as_deref(), Some("mcp-x"));
        assert!(mcp_capability_id("!!!").is_none());
        assert!(mcp_capability_id(&"a".repeat(29)).is_none());
        assert!(mcp_capability_id("").is_none());
        assert!(mcp_capability_id("-").is_none());
        assert!(capability_id_ok("mcp-files"));
    }

    fn target(name: &str, command: &str, args: Vec<String>, enabled: bool) -> McpTarget {
        McpTarget {
            spec: McpServerSpec::new(name, command, args),
            enabled,
        }
    }

    #[tokio::test]
    async fn a_disabled_server_is_stopped_and_not_spawned() {
        let targets = vec![target(
            "off",
            "/definitely/not/a/real/binary",
            vec![],
            false,
        )];
        let (servers, tools, roster) = mount(&targets, &CancellationToken::new()).await;
        assert!(servers.is_empty() && tools.is_empty());
        let record = &roster.records()[0];
        assert_eq!(record.state, ComponentState::Stopped);
        assert_eq!(record.reason.as_deref(), Some("disabled"));
        assert_eq!(record.capability_id.as_deref(), Some("mcp-off"));
        assert!(record.tools.is_empty());
    }

    #[tokio::test]
    async fn an_illegal_name_is_failed_and_does_not_provide() {
        let targets = vec![target("!!!", "/definitely/not/a/real/binary", vec![], true)];
        let (_, _, roster) = mount(&targets, &CancellationToken::new()).await;
        let record = &roster.records()[0];
        assert_eq!(record.state, ComponentState::Failed);
        assert_eq!(record.reason.as_deref(), Some("invalid_capability_id"));
        assert!(record.capability_id.is_none());
        assert!(record.id.starts_with("mcp-bad-"));
        assert!(record.tools.is_empty());
    }

    #[tokio::test]
    async fn two_servers_that_normalize_to_one_id_do_not_both_provide_it() {
        let targets = vec![
            target("a_b", "/definitely/not/a/real/binary", vec![], false),
            target("a-b", "/definitely/not/a/real/binary", vec![], true),
        ];
        let (servers, _, roster) = mount(&targets, &CancellationToken::new()).await;
        assert!(servers.is_empty(), "the duplicate must not be connected");
        let records = roster.records();
        assert_eq!(records[0].state, ComponentState::Stopped);
        assert_eq!(records[0].capability_id.as_deref(), Some("mcp-a-b"));
        assert_eq!(records[1].state, ComponentState::Failed);
        assert_eq!(
            records[1].reason.as_deref(),
            Some("duplicate_capability_id")
        );
        assert!(records[1].capability_id.is_none());
        assert_ne!(records[0].id, records[1].id);
    }

    /// One tool, then the process ends itself when `stop` appears. No signal:
    /// a daemon thread watches the file and calls `os._exit`.
    const EXIT_FIXTURE: &str = r#"import json,sys,os,threading,time
stop=sys.argv[1]
def watch():
    while not os.path.exists(stop):
        time.sleep(0.05)
    os._exit(0)
threading.Thread(target=watch, daemon=True).start()
for line in sys.stdin:
    try: req=json.loads(line)
    except Exception: continue
    ident=req.get("id")
    if ident is None: continue
    method=req.get("method")
    if method=="initialize":
        result={"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
    elif method=="tools/list":
        result={"tools":[
            {"name":"echo","description":"e","inputSchema":{"type":"object","properties":{}}},
            {"name":"ping","description":"p","inputSchema":{"type":"object","properties":{}}}
        ]}
    else:
        result={}
    print(json.dumps({"jsonrpc":"2.0","id":ident,"result":result}), flush=True)
"#;

    fn fixture_target(dir: &Path, name: &str) -> (McpTarget, PathBuf) {
        let script = dir.join("server.py");
        let stop = dir.join("stop");
        std::fs::write(&script, EXIT_FIXTURE).unwrap();
        (
            target(
                name,
                "python3",
                vec![
                    "-u".to_owned(),
                    script.to_string_lossy().into_owned(),
                    stop.to_string_lossy().into_owned(),
                ],
                true,
            ),
            stop,
        )
    }

    async fn until_terminal(roster: &McpRoster, id: &str) -> McpRecord {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let record = roster
                .records()
                .into_iter()
                .find(|r| r.id == id)
                .expect("record");
            if !matches!(
                record.state,
                ComponentState::Ready | ComponentState::Starting
            ) {
                return record;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("still {record:?}");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn a_ready_server_lists_its_tools_and_a_child_exit_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        let (target, stop) = fixture_target(dir.path(), "fixture");
        let cancel = CancellationToken::new();
        let (servers, tools, roster) = mount(std::slice::from_ref(&target), &cancel).await;
        assert_eq!(servers.len(), 1);
        assert_eq!(tools.len(), 2);
        let ready = &roster.records()[0];
        assert_eq!(ready.state, ComponentState::Ready);
        assert_eq!(ready.id, "mcp-fixture");
        assert_eq!(ready.capability_id.as_deref(), Some("mcp-fixture"));
        assert_eq!(
            ready.tools,
            vec!["mcp_fixture_echo".to_owned(), "mcp_fixture_ping".to_owned()]
        );
        assert_eq!(ready.protocol_version.as_deref(), Some("2024-11-05"));
        std::fs::write(&stop, "1").unwrap();
        let ended = until_terminal(&roster, "mcp-fixture").await;
        assert_eq!(ended.state, ComponentState::Failed);
        assert_eq!(ended.reason.as_deref(), Some("process_exited"));
        assert_eq!(ended.tools, ready.tools, "a crash keeps the generated list");
        drop(servers);
    }

    #[tokio::test]
    async fn cancelling_the_daemon_token_stops_a_ready_server() {
        let dir = tempfile::tempdir().unwrap();
        let (target, _stop) = fixture_target(dir.path(), "fixture");
        let cancel = CancellationToken::new();
        let (servers, _, roster) = mount(std::slice::from_ref(&target), &cancel).await;
        assert_eq!(roster.records()[0].state, ComponentState::Ready);
        cancel.cancel();
        let ended = until_terminal(&roster, "mcp-fixture").await;
        assert_eq!(ended.state, ComponentState::Stopped);
        assert_eq!(ended.reason.as_deref(), Some("stopped"));
        assert_eq!(ended.tools.len(), 2);
        drop(servers);
    }
}
