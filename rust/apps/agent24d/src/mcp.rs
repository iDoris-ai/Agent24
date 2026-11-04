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

use std::collections::{BTreeMap, BTreeSet};
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
    /// File order, not `BTreeMap` order. A normalized-id collision gives the
    /// capability id to the server written first in `mcp.json`. The later
    /// server still connects.
    #[serde(
        rename = "mcpServers",
        default,
        deserialize_with = "servers_in_file_order"
    )]
    pub servers: Vec<(String, ServerEntry)>,
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

/// `mcpServers` in the order the keys appear in the file.
///
/// `serde_json::Map` is a `BTreeMap` in this workspace (`preserve_order` is
/// off, and `indexmap` is not a dependency). A custom visitor still sees the
/// parser's order, so the first key written keeps a normalized capability id.
fn servers_in_file_order<'de, D>(deserializer: D) -> Result<Vec<(String, ServerEntry)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct FileOrder;
    impl<'de> serde::de::Visitor<'de> for FileOrder {
        type Value = Vec<(String, ServerEntry)>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an object of MCP servers in file order")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut servers = Vec::with_capacity(map.size_hint().unwrap_or(0));
            while let Some(entry) = map.next_entry()? {
                servers.push(entry);
            }
            Ok(servers)
        }
    }
    deserializer.deserialize_map(FileOrder)
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
    /// Order is the key order of `mcp.json`.
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
    /// Component id. `mcp-<server>` when that id is free. `mcp-bad-<hash>` when
    /// the name is not a legal capability id, or a later server would reuse one.
    pub id: String,
    pub state: ComponentState,
    pub reason: Option<String>,
    /// Set when this server owns a legal capability id. Absent means `/components`
    /// writes no `provides` for it. Tools are still registered either way.
    pub capability_id: Option<String>,
    /// Qualified tool names from `tools/list`. Empty when the list never arrived.
    pub tools: Vec<String>,
    /// MCP `protocolVersion`, recorded and not compared (D3 does not apply).
    pub protocol_version: Option<String>,
}

impl McpRecord {
    /// Capability ids written into `/components` `provides`. Empty when this
    /// server has no legal id of its own.
    pub(crate) fn provide_ids(&self) -> Vec<String> {
        self.capability_id.iter().cloned().collect()
    }
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
/// (empty, or longer than 32). Those servers still connect; they are not given
/// a provides entry.
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

pub(crate) fn fallback_component_id(server: &str) -> String {
    let mut hash: u32 = 2_166_136_261;
    for b in server.as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(16_777_619);
    }
    format!("mcp-bad-{hash:08x}")
}

/// An id of the `mcp-bad-<hash>` shape that is not already in `seen`.
/// `salt` is the server name, or a component id that collided with one.
pub(crate) fn claim_fallback_id(salt: &str, seen: &mut BTreeSet<String>) -> String {
    let mut n = 0u32;
    loop {
        let flavored = if n == 0 {
            salt.to_owned()
        } else {
            format!("{salt}#{n}")
        };
        let id = fallback_component_id(&flavored);
        n += 1;
        if seen.insert(id.clone()) {
            return id;
        }
    }
}

/// `(component id, capability id, duplicate)`.
///
/// A legal id that nobody has taken yet is both the component id and the
/// capability id. Anything else connects under `mcp-bad-<hash>` and provides
/// nothing. `duplicate` is the later server of a normalized-id pair.
fn assign_component_id(name: &str, seen: &mut BTreeSet<String>) -> (String, Option<String>, bool) {
    let legal = mcp_capability_id(name);
    let duplicate = match &legal {
        Some(id) => !seen.insert(id.clone()),
        None => false,
    };
    if !duplicate && let Some(id) = legal {
        return (id.clone(), Some(id), false);
    }
    (claim_fallback_id(name, seen), None, duplicate)
}

/// `None` means the connect ended because the daemon cancelled it, so the row
/// is `stopped` rather than a start failure. `Cancelled` is always that case.
/// A spawn, handshake, or `tools/list` error is too, once the token has fired.
fn connect_fail_reason(err: &McpError, shutdown: bool) -> Option<&'static str> {
    match err {
        McpError::Cancelled => None,
        McpError::Spawn(_, _) => {
            if shutdown {
                None
            } else {
                Some("spawn_failed")
            }
        }
        McpError::Handshake(_, _) | McpError::Call(_) => {
            if shutdown {
                None
            } else {
                Some("handshake_failed")
            }
        }
    }
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
    let mut seen_tools: BTreeSet<String> = BTreeSet::new();
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let deadline = tokio::time::Instant::now() + MOUNT_TOTAL_BUDGET;
    for target in targets {
        let spec = &target.spec;
        let (id, capability_id, duplicate) = assign_component_id(&spec.name, &mut seen_ids);
        if !target.enabled {
            // A disabled duplicate still records why it has no provides. It does
            // not spawn: `enabled: false` means the user left it out.
            let reason = if duplicate {
                "duplicate_capability_id"
            } else {
                "disabled"
            };
            roster.push(
                outcome_record(spec, id, capability_id, ComponentState::Stopped, reason),
                None,
            );
            continue;
        }
        if duplicate {
            tracing::info!(
                "MCP server {} shares a capability id with an earlier server; connecting \
                 without provides",
                spec.name
            );
        } else if capability_id.is_none() {
            tracing::info!(
                "MCP server {} has no legal capability id; connecting without provides",
                spec.name
            );
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            if cancel.is_cancelled() {
                tracing::info!("MCP server {} not mounted: cancelled", spec.name);
                roster.push(stopped_record(spec, id, capability_id), None);
            } else {
                tracing::error!(
                    "MCP server {} not mounted: total mount budget {MOUNT_TOTAL_BUDGET:?} exhausted",
                    spec.name
                );
                roster.push(
                    failed_record(spec, id, capability_id, "start_timeout"),
                    None,
                );
            }
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
                    reason: duplicate.then(|| "duplicate_capability_id".to_owned()),
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
                // Daemon shutdown is `stopped`, including a handshake error that
                // rmcp reports while the token is already cancelled. `Cancelled`
                // is its own arm: treating it as a guard on the if makes the
                // later match arm unreachable under `-D warnings`.
                let shutdown = server_cancel.is_cancelled();
                let fail_reason = connect_fail_reason(&err, shutdown);
                if let Some(reason) = fail_reason {
                    tracing::error!("MCP server {} not mounted: {err}", spec.name);
                    roster.push(failed_record(spec, id, capability_id, reason), None);
                } else {
                    tracing::info!("MCP server {} not mounted: cancelled", spec.name);
                    roster.push(stopped_record(spec, id, capability_id), None);
                }
            }
            Err(_) => {
                if server_cancel.is_cancelled() {
                    tracing::info!("MCP server {} not mounted: cancelled", spec.name);
                    roster.push(stopped_record(spec, id, capability_id), None);
                } else {
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
    }
    (servers, tools, roster)
}

fn failed_record(
    spec: &McpServerSpec,
    id: String,
    capability_id: Option<String>,
    reason: &str,
) -> McpRecord {
    outcome_record(spec, id, capability_id, ComponentState::Failed, reason)
}

fn stopped_record(spec: &McpServerSpec, id: String, capability_id: Option<String>) -> McpRecord {
    outcome_record(spec, id, capability_id, ComponentState::Stopped, "stopped")
}

fn outcome_record(
    spec: &McpServerSpec,
    id: String,
    capability_id: Option<String>,
    state: ComponentState,
    reason: &str,
) -> McpRecord {
    McpRecord {
        server: spec.name.clone(),
        id,
        state,
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

    fn record(id: &str, state: ComponentState, reason: Option<&str>) -> McpRecord {
        McpRecord {
            server: id.to_owned(),
            id: id.to_owned(),
            state,
            reason: reason.map(str::to_owned),
            capability_id: None,
            tools: Vec::new(),
            protocol_version: None,
        }
    }

    /// `note_liveness` only moves `starting` and `ready`. A terminal row keeps the
    /// state and reason it already has: a late "the child ended" must not turn a
    /// `disabled` stop or a `start_timeout` failure into something else.
    #[test]
    fn note_liveness_leaves_terminal_rows_alone() {
        let roster = McpRoster::from_records(vec![
            record("a-disabled", ComponentState::Stopped, Some("disabled")),
            record("b-timeout", ComponentState::Failed, Some("start_timeout")),
            record("c-ready", ComponentState::Ready, None),
            record("d-starting", ComponentState::Starting, None),
        ]);
        for id in ["a-disabled", "b-timeout", "c-ready", "d-starting"] {
            roster.note_liveness(id, false);
            roster.note_liveness(id, true);
        }
        let by_id = |id: &str| roster.records().into_iter().find(|r| r.id == id).unwrap();
        let a = by_id("a-disabled");
        assert_eq!(
            (a.state, a.reason.as_deref()),
            (ComponentState::Stopped, Some("disabled"))
        );
        let b = by_id("b-timeout");
        assert_eq!(
            (b.state, b.reason.as_deref()),
            (ComponentState::Failed, Some("start_timeout"))
        );
        // Positive control: a live row does move (to failed first; the second,
        // stopped_by_us call then finds it terminal and leaves it failed).
        let c = by_id("c-ready");
        assert_eq!(
            (c.state, c.reason.as_deref()),
            (ComponentState::Failed, Some("process_exited"))
        );
        let d = by_id("d-starting");
        assert_eq!(
            (d.state, d.reason.as_deref()),
            (ComponentState::Failed, Some("process_exited"))
        );
    }

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

    /// `Cancelled` is `stopped`, including when the token is already cancelled
    /// and the error is a spawn or handshake failure rather than `Cancelled`.
    #[test]
    fn cancelled_errors_map_to_stopped_not_a_start_failure() {
        assert_eq!(connect_fail_reason(&McpError::Cancelled, false), None);
        assert_eq!(connect_fail_reason(&McpError::Cancelled, true), None);
        assert_eq!(
            connect_fail_reason(&McpError::Handshake("srv".into(), "bye".into()), true),
            None
        );
        assert_eq!(
            connect_fail_reason(&McpError::Call("bye".into()), true),
            None
        );
        assert_eq!(
            connect_fail_reason(&McpError::Spawn("srv".into(), "missing".into()), true),
            None
        );
        assert_eq!(
            connect_fail_reason(&McpError::Handshake("srv".into(), "bye".into()), false),
            Some("handshake_failed")
        );
        assert_eq!(
            connect_fail_reason(&McpError::Call("bye".into()), false),
            Some("handshake_failed")
        );
        assert_eq!(
            connect_fail_reason(&McpError::Spawn("srv".into(), "missing".into()), false),
            Some("spawn_failed")
        );
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

    fn live_target(
        python: &str,
        script: &Path,
        name: &str,
        stop: &Path,
        enabled: bool,
    ) -> McpTarget {
        target(
            name,
            python,
            vec![
                "-u".to_owned(),
                script.to_string_lossy().into_owned(),
                stop.to_string_lossy().into_owned(),
            ],
            enabled,
        )
    }

    /// A name that does not normalize, a name that is too long, and a second
    /// server that normalizes onto the first one's id all still connect. Tool
    /// names stay `mcp_{server}_{tool}`. None of those three writes `provides`.
    #[tokio::test]
    async fn illegal_and_duplicate_names_still_connect_without_provides() {
        let dir = tempfile::tempdir().unwrap();
        let python = real_python();
        let script = dir.path().join("server.py");
        std::fs::write(&script, EXIT_FIXTURE).unwrap();
        let long = "filesystem-readonly-projects-dir";
        assert!(long.len() > 28);
        assert!(mcp_capability_id("文件").is_none());
        assert!(mcp_capability_id(long).is_none());
        assert_eq!(mcp_capability_id("Files").as_deref(), Some("mcp-files"));
        let names = ["文件", long, "files", "Files"];
        let targets: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                live_target(
                    &python,
                    &script,
                    name,
                    &dir.path().join(format!("stop-{i}")),
                    true,
                )
            })
            .collect();
        let (servers, tools, roster) = mount(&targets, &CancellationToken::new()).await;
        let connected: Vec<_> = servers.iter().map(|s| s.name().to_owned()).collect();
        assert_eq!(
            connected,
            vec![
                "文件".to_owned(),
                long.to_owned(),
                "files".to_owned(),
                "Files".to_owned()
            ],
            "records={:?}",
            roster.records()
        );
        let registered: std::collections::BTreeSet<_> =
            tools.iter().map(|tool| tool.info().name).collect();
        let records = roster.records();
        let expect = |server: &str, tool_prefix: &str, provides: bool, duplicate: bool| {
            let record = records
                .iter()
                .find(|r| r.server == server)
                .unwrap_or_else(|| panic!("no {server} in {records:?}"));
            assert_eq!(record.state, ComponentState::Ready, "{server}");
            assert_eq!(
                record.tools,
                vec![
                    format!("mcp_{tool_prefix}_echo"),
                    format!("mcp_{tool_prefix}_ping"),
                ],
                "{server}"
            );
            for tool in &record.tools {
                assert!(registered.contains(tool), "{tool} was not registered");
            }
            if provides {
                assert_eq!(record.provide_ids(), vec!["mcp-files".to_owned()]);
                assert_eq!(record.id, "mcp-files");
                assert!(record.reason.is_none());
            } else {
                assert!(
                    record.provide_ids().is_empty(),
                    "{server} wrote provides {:?}",
                    record.provide_ids()
                );
                assert!(record.capability_id.is_none());
                assert!(record.id.starts_with("mcp-bad-"), "{}", record.id);
                assert_ne!(record.id, "mcp-files");
            }
            if duplicate {
                assert_eq!(
                    record.reason.as_deref(),
                    Some("duplicate_capability_id"),
                    "{server}"
                );
            }
        };
        // 「文件」sanitizes to `--` (one `-` per non-ASCII character).
        expect("文件", "--", false, false);
        expect(long, long, false, false);
        expect("files", "files", true, false);
        expect("Files", "Files", false, true);
        let ids: std::collections::BTreeSet<_> = records.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            records.len(),
            "component ids collided: {records:?}"
        );
        drop(servers);
    }

    /// The duplicate row's `mcp-bad-…` id is claimed. A later server whose legal
    /// id is that same string still connects, under a different component id,
    /// and does not provide.
    #[tokio::test]
    async fn a_duplicate_fallback_id_is_claimed_so_a_later_server_cannot_reuse_it() {
        let second = "Files";
        let fallback = fallback_component_id(second);
        let third = fallback.strip_prefix("mcp-").expect("mcp-bad- shape");
        assert_eq!(mcp_capability_id(third).as_deref(), Some(fallback.as_str()));
        let dir = tempfile::tempdir().unwrap();
        let python = real_python();
        let script = dir.path().join("server.py");
        std::fs::write(&script, EXIT_FIXTURE).unwrap();
        let names = ["files", second, third];
        let targets: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                live_target(
                    &python,
                    &script,
                    name,
                    &dir.path().join(format!("stop-claim-{i}")),
                    i != 0,
                )
            })
            .collect();
        let (servers, tools, roster) = mount(&targets, &CancellationToken::new()).await;
        let connected: Vec<_> = servers.iter().map(|s| s.name().to_owned()).collect();
        assert_eq!(connected, vec![second.to_owned(), third.to_owned()]);
        assert!(!tools.is_empty());
        let records = roster.records();
        assert_eq!(records[0].id, "mcp-files");
        assert_eq!(records[0].state, ComponentState::Stopped);
        assert_eq!(records[0].reason.as_deref(), Some("disabled"));
        assert_eq!(records[1].id, fallback);
        assert_eq!(records[1].state, ComponentState::Ready);
        assert_eq!(
            records[1].reason.as_deref(),
            Some("duplicate_capability_id")
        );
        assert!(records[1].provide_ids().is_empty());
        assert!(!records[1].tools.is_empty());
        assert_eq!(records[2].state, ComponentState::Ready);
        assert_eq!(
            records[2].reason.as_deref(),
            Some("duplicate_capability_id")
        );
        assert_ne!(records[2].id, records[1].id);
        assert!(records[2].capability_id.is_none());
        assert!(records[2].provide_ids().is_empty());
        assert!(!records[2].tools.is_empty());
        drop(servers);
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

    /// Speaks MCP and writes `argv[1]` to `argv[2]` as soon as the process starts,
    /// so a test can see which configured server was actually spawned.
    const MARKER_FIXTURE: &str = r#"import json,sys
open(sys.argv[2],"w").write(sys.argv[1])
for line in sys.stdin:
    try: req=json.loads(line)
    except Exception: continue
    ident=req.get("id")
    if ident is None: continue
    method=req.get("method")
    if method=="initialize":
        result={"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
    elif method=="tools/list":
        result={"tools":[{"name":"echo","description":"e","inputSchema":{"type":"object","properties":{}}}]}
    else:
        result={}
    print(json.dumps({"jsonrpc":"2.0","id":ident,"result":result}), flush=True)
"#;

    /// Absolute interpreter. `python3` on this machine is a pyenv shim, and the
    /// MCP child does not inherit the environment that shim needs, so `pyenv exec`
    /// can sit there until the connect budget expires.
    fn real_python() -> String {
        let out = std::process::Command::new("python3")
            .arg("-c")
            .arg("import sys; print(sys.executable)")
            .output()
            .expect("python3 -c");
        assert!(
            out.status.success(),
            "python3 -c failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("python path")
            .trim()
            .to_owned()
    }

    fn marker_entry(
        python: &str,
        script: &Path,
        name: &str,
        marker: &Path,
        enabled: bool,
    ) -> String {
        let python = serde_json::to_string(python).unwrap();
        let script = serde_json::to_string(&script.to_string_lossy().as_ref()).unwrap();
        let marker = serde_json::to_string(&marker.to_string_lossy().as_ref()).unwrap();
        let enabled = if enabled {
            String::new()
        } else {
            r#","enabled":false"#.to_owned()
        };
        format!(
            r#""{name}":{{"command":{python},"args":["-u",{script},{script_name},{marker}]{enabled}}}"#,
            script_name = serde_json::to_string(name).unwrap(),
        )
    }

    /// Disabled `Filesystem` written after enabled `filesystem` must connect
    /// `filesystem`. Swapping the keys connects the other entry's process
    /// (here: none, because the first key is the disabled one).
    #[tokio::test]
    async fn file_order_not_dictionary_order_decides_which_server_connects() {
        let dir = tempfile::tempdir().unwrap();
        let python = real_python();
        let script = dir.path().join("server.py");
        std::fs::write(&script, MARKER_FIXTURE).unwrap();

        let mount_raw = |raw: String| async move {
            let cfg = parse_config(&raw).unwrap();
            let names: Vec<String> = cfg.targets().iter().map(|t| t.spec.name.clone()).collect();
            let (servers, _, roster) = mount(&cfg.targets(), &CancellationToken::new()).await;
            let connected: Vec<String> = servers.iter().map(|s| s.name().to_owned()).collect();
            drop(servers);
            (names, connected, roster.records())
        };

        // Marker names must not differ only by case: the volume is case-insensitive.
        let fs = dir.path().join("ran-lower");
        let off = dir.path().join("ran-upper");
        let filesystem_first = format!(
            r#"{{"mcpServers":{{{a},{b}}}}}"#,
            a = marker_entry(&python, &script, "filesystem", &fs, true),
            b = marker_entry(&python, &script, "Filesystem", &off, false),
        );
        let cfg = parse_config(&filesystem_first).unwrap();
        let preview: Vec<_> = cfg
            .targets()
            .iter()
            .map(|t| {
                (
                    t.spec.name.clone(),
                    t.enabled,
                    t.spec.command.clone(),
                    t.spec.args.clone(),
                )
            })
            .collect();
        assert!(
            preview[0].1 && preview[0].3.len() == 4,
            "raw={filesystem_first} preview={preview:?}"
        );
        let (order, connected, records) = mount_raw(filesystem_first).await;
        assert_eq!(
            order,
            vec!["filesystem".to_owned(), "Filesystem".to_owned()]
        );
        assert_eq!(
            connected,
            vec!["filesystem".to_owned()],
            "records={records:?}"
        );
        assert_eq!(std::fs::read_to_string(&fs).unwrap(), "filesystem");
        assert!(!off.exists(), "the disabled server must not be spawned");
        assert_eq!(records[0].state, ComponentState::Ready);
        assert_eq!(records[0].server, "filesystem");
        assert_eq!(
            records[1].reason.as_deref(),
            Some("duplicate_capability_id")
        );
        assert_eq!(records[1].server, "Filesystem");

        let fs_swapped = dir.path().join("ran-lower-swapped");
        let off_swapped = dir.path().join("ran-upper-swapped");
        let disabled_first = format!(
            r#"{{"mcpServers":{{{a},{b}}}}}"#,
            a = marker_entry(&python, &script, "Filesystem", &off_swapped, false),
            b = marker_entry(&python, &script, "filesystem", &fs_swapped, true),
        );
        let (order, connected, records) = mount_raw(disabled_first).await;
        assert_eq!(
            order,
            vec!["Filesystem".to_owned(), "filesystem".to_owned()]
        );
        // The disabled first entry keeps the legal id. The enabled second
        // server still connects, without that id and without provides.
        assert_eq!(
            connected,
            vec!["filesystem".to_owned()],
            "records={records:?}"
        );
        assert_eq!(std::fs::read_to_string(&fs_swapped).unwrap(), "filesystem");
        assert!(!off_swapped.exists());
        assert_eq!(records[0].server, "Filesystem");
        assert_eq!(records[0].id, "mcp-filesystem");
        assert_eq!(records[0].state, ComponentState::Stopped);
        assert_eq!(records[0].reason.as_deref(), Some("disabled"));
        assert_eq!(records[1].server, "filesystem");
        assert_eq!(records[1].state, ComponentState::Ready);
        assert_eq!(
            records[1].reason.as_deref(),
            Some("duplicate_capability_id")
        );
        assert!(records[1].provide_ids().is_empty());
        assert!(records[1].id.starts_with("mcp-bad-"));
        assert!(!records[1].tools.is_empty());
    }

    const HANG_ON_LIST_FIXTURE: &str = r#"import json,sys,os,time
for line in sys.stdin:
    try: req=json.loads(line)
    except Exception: continue
    ident=req.get("id")
    if ident is None: continue
    method=req.get("method")
    if method=="initialize":
        result={"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
    elif method=="tools/list":
        open(sys.argv[1],"w").write("listed")
        while True:
            time.sleep(0.2)
            if os.getppid() == 1:
                os._exit(0)
    else:
        result={}
    print(json.dumps({"jsonrpc":"2.0","id":ident,"result":result}), flush=True)
"#;

    #[tokio::test]
    async fn cancelling_during_tools_list_is_stopped_not_a_start_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hang.py");
        let listed = dir.path().join("listed");
        std::fs::write(&script, HANG_ON_LIST_FIXTURE).unwrap();
        let spec = target(
            "hang",
            &real_python(),
            vec![
                "-u".to_owned(),
                script.to_string_lossy().into_owned(),
                listed.to_string_lossy().into_owned(),
            ],
            true,
        );
        let cancel = CancellationToken::new();
        let cancel_for_mount = cancel.clone();
        let mounted =
            tokio::spawn(
                async move { mount(std::slice::from_ref(&spec), &cancel_for_mount).await },
            );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !listed.exists() {
            if tokio::time::Instant::now() > deadline {
                panic!("tools/list did not start");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let started = std::time::Instant::now();
        cancel.cancel();
        let (servers, _, roster) = tokio::time::timeout(CONNECT_TIMEOUT / 2, mounted)
            .await
            .expect("cancel during tools/list waited on the connect budget")
            .unwrap();
        assert!(
            started.elapsed() < CONNECT_TIMEOUT / 2,
            "cancel during tools/list took {:?}, past half the connect budget",
            started.elapsed()
        );
        assert!(servers.is_empty());
        let record = &roster.records()[0];
        assert_eq!(record.state, ComponentState::Stopped);
        assert_eq!(record.reason.as_deref(), Some("stopped"));
        assert!(record.tools.is_empty());
        drop(servers);
    }
}
