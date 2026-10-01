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
use std::sync::Arc;
use std::time::Duration;

use agent24_mcp::{McpServer, McpServerSpec};
use agent24_tools::Tool;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

/// Per-server connect budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);

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
    pub fn specs(&self) -> Vec<McpServerSpec> {
        self.servers
            .iter()
            .filter(|(_, e)| e.enabled)
            .map(|(name, e)| {
                McpServerSpec::new(name.clone(), e.command.clone(), e.args.clone())
                    .with_target_args(e.target_args.clone())
                    .with_env(e.env.clone())
            })
            .collect()
    }
}

/// Connect every configured server and collect their tools.
///
/// Returns the live handles (which the caller MUST keep alive) alongside the
/// tools to register. Individual failures are logged and skipped.
pub async fn mount(
    specs: &[McpServerSpec],
    cancel: &CancellationToken,
) -> (Vec<Arc<McpServer>>, Vec<Arc<dyn Tool>>) {
    let mut servers = Vec::new();
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let deadline = tokio::time::Instant::now() + MOUNT_TOTAL_BUDGET;
    for spec in specs {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::error!(
                "MCP server {} not mounted: total mount budget {MOUNT_TOTAL_BUDGET:?} exhausted",
                spec.name
            );
            continue;
        }
        let budget = CONNECT_TIMEOUT.min(remaining);
        let connect = agent24_mcp::connect_and_build_tools(spec, cancel.clone());
        match tokio::time::timeout(budget, connect).await {
            Ok(Ok((server, built))) => {
                // Belt-and-braces after the separator fix: distinct raw names can
                // still sanitize to the same string (e.g. `a.b` and `a-b`), and
                // ToolRegistry::with() is a plain insert that would silently
                // shadow. Refuse the duplicate loudly instead.
                let mut kept: Vec<Arc<dyn Tool>> = Vec::new();
                for tool in built {
                    let name = tool.info().name;
                    if !seen.insert(name.clone()) {
                        tracing::error!(
                            "MCP tool {name} from server {} collides with an already \
                             registered tool and was NOT registered — rename the server",
                            spec.name
                        );
                        continue;
                    }
                    kept.push(tool);
                }
                tracing::info!(
                    "mounted MCP server {} with {} tool(s)",
                    spec.name,
                    kept.len()
                );
                servers.push(server);
                tools.extend(kept);
            }
            Ok(Err(err)) => {
                tracing::error!("MCP server {} not mounted: {err}", spec.name);
            }
            Err(_) => {
                tracing::error!(
                    "MCP server {} not mounted: handshake exceeded its {budget:?} budget",
                    spec.name
                );
            }
        }
    }
    (servers, tools)
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
        let specs: Vec<McpServerSpec> = (0..5)
            .map(|i| McpServerSpec::new(format!("broken{i}"), "/definitely/not/real", vec![]))
            .collect();
        let started = std::time::Instant::now();
        let (servers, tools) = mount(&specs, &CancellationToken::new()).await;
        assert!(servers.is_empty() && tools.is_empty());
        assert!(
            started.elapsed() < MOUNT_TOTAL_BUDGET + Duration::from_secs(2),
            "mount overran its total budget: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_server_that_cannot_start_is_skipped_not_fatal() {
        // The daemon must still come up when a configured server is broken.
        let specs = vec![McpServerSpec::new(
            "broken",
            "/definitely/not/a/real/binary",
            vec![],
        )];
        let (servers, tools) = mount(&specs, &CancellationToken::new()).await;
        assert!(servers.is_empty());
        assert!(tools.is_empty());
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
}
