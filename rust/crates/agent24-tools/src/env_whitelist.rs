//! Minimal environment whitelist for child processes this crate (and the MCP
//! adapter built on top of it) spawns: `shell_exec` and external MCP servers.
//!
//! FU-103 / J-6 (`docs/agent/followups.md`): `shell_exec` and MCP server
//! child processes used to inherit `tokio::process::Command`'s DEFAULT
//! environment — the daemon's *entire* process environment, unfiltered. An
//! LLM-issued shell command, or any third-party MCP server binary the user
//! configured, could read every `*_KEY` / `*_TOKEN` / `*_SECRET` and every
//! `A24_*`/`OMLX_*`/(future) `IDORIS_*` variable the daemon happened to hold,
//! straight out of its own environment — no tool call, no approval gate, no
//! audit trail. Neither needs the daemon's secrets to do its job: they need a
//! *working process* (PATH, HOME, locale, tmp dir), nothing else. So every
//! child process spawned here gets `env_clear()`'d and then repopulated ONLY
//! from this fixed list — opt-in, never opt-out.
//!
//! A single MCP server that genuinely needs a credential (its own API token,
//! say) gets it explicitly, from that server's own `env` block in
//! `~/.agent24/mcp.json` (see `agent24d::mcp::ServerEntry`) — never by
//! inheriting the daemon's ambient environment, and never shared with any
//! other server.

use tokio::process::Command;

/// Environment variables passed through to EVERY child process spawned here
/// (`shell_exec`, every MCP server), regardless of what else the daemon's own
/// environment holds. This is an ALLOWLIST — each entry needs a reason below,
/// and silence is not permission. In particular: no `*_KEY`, `*_TOKEN`,
/// `*_SECRET`, or daemon-private (`A24_*`, `OMLX_*`, `IDORIS_*`, …) variable
/// belongs here, ever.
pub const CHILD_ENV_WHITELIST: &[&str] = &[
    // The single most load-bearing entry: without it almost no real binary
    // (`git`, `node`, `python3`, `npx`, …) can even be found, let alone run.
    "PATH",
    // Expanded by countless tools (git config, npm/pnpm, cargo, shells,
    // language runtimes) to find config/cache/state dirs; without it tools
    // misbehave or write into the daemon's own cwd instead.
    "HOME",
    // Read by some CLIs for prompts/logging and by libc user-lookup calls;
    // cheap to pass and occasionally required for a tool to run at all.
    "USER",
    // Locale. Without these some tools fall back to a "C" locale that mangles
    // non-ASCII output/arguments in ways that are surprising to debug.
    "LANG", "LC_ALL", "LC_CTYPE",
    // Where a child should put scratch files. Without it tools fall back to
    // a hard-coded `/tmp`, which usually works but should be the platform's
    // choice, not an accident of whatever the daemon's ambient env held.
    "TMPDIR",
    // A spawned shell script (`#!/bin/sh …`) or a tool that shells out
    // internally needs to know which shell to use.
    "SHELL",
    // Terminal capability reporting; several CLIs (git, ls --color, …) change
    // output formatting based on this and behave oddly without it.
    "TERM",
];

/// Reset `cmd`'s environment to exactly [`CHILD_ENV_WHITELIST`]: `env_clear()`
/// first (so nothing is inherited from the daemon's ambient environment),
/// then copy through only the whitelisted variables that are actually set in
/// the daemon's own environment. Absent stays absent — this never invents a
/// value for a variable the daemon doesn't have.
pub fn clear_and_whitelist(cmd: &mut Command) {
    cmd.env_clear();
    for key in CHILD_ENV_WHITELIST {
        if let Some(val) = std::env::var_os(key) {
            cmd.env(key, val);
        }
    }
}

/// [`clear_and_whitelist`] plus an explicit extra set of variables layered on
/// top — for a single MCP server's own `env` block in `mcp.json`. `extra` is
/// applied AFTER the whitelist, so a server-declared value can override a
/// whitelisted one (e.g. a server-specific `TMPDIR`) if the user wants that,
/// but it is scoped to the one `Command` being built here: it is never
/// visible to any other server or to `shell_exec`.
pub fn clear_whitelist_and_extend<'a, I>(cmd: &mut Command, extra: I)
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    clear_and_whitelist(cmd);
    for (key, val) in extra {
        cmd.env(key, val);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The whitelist itself must never grow a credential-shaped entry — this
    /// is the guardrail against a future edit quietly widening the allowlist
    /// to something that defeats the whole point of FU-103.
    #[test]
    fn whitelist_contains_no_credential_shaped_names() {
        for key in CHILD_ENV_WHITELIST {
            let upper = key.to_uppercase();
            assert!(
                !upper.ends_with("_KEY")
                    && !upper.ends_with("_TOKEN")
                    && !upper.ends_with("_SECRET")
                    && !upper.starts_with("A24_")
                    && !upper.starts_with("OMLX_")
                    && !upper.starts_with("IDORIS_"),
                "{key} looks credential-shaped and must not be in the child-process whitelist"
            );
        }
    }

    /// Positive control for the two tests below: confirms `printenv` is on
    /// PATH and prints in the expected `KEY=value` shape, so a failure to
    /// find `secret_test_key` in the assertions below is the whitelist
    /// working, not the probe command silently failing to run at all.
    #[tokio::test]
    async fn probe_command_prints_a_variable_that_is_actually_set() {
        let mut cmd = Command::new("printenv");
        cmd.env("PROBE_SANITY_VAR", "present");
        let out = cmd.output().await.expect("spawn printenv");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("PROBE_SANITY_VAR=present"),
            "probe setup is broken, unrelated to the whitelist:\n{stdout}"
        );
    }

    /// The core guarantee: a variable present on the `Command` builder before
    /// whitelisting runs (standing in for "the daemon's ambient process
    /// environment held a secret") does not survive into the child, while a
    /// whitelisted variable that is genuinely set in this test process (PATH
    /// always is) does. Uses `Command::env` to inject the stand-in secret
    /// rather than `std::env::set_var` on the whole test binary — edition
    /// 2024 forbids `set_var` outside `unsafe` (workspace-wide policy, see
    /// `agent24-models::router` tests), and mutating process-global state
    /// would race every other test in this binary; injecting onto a single
    /// `Command` builder is scoped to this test and cannot leak sideways.
    #[tokio::test]
    async fn shell_exec_style_child_does_not_inherit_unlisted_vars_but_keeps_path() {
        let mut cmd = Command::new("printenv");
        cmd.env("secret_test_key", "leaked-if-this-appears");
        clear_and_whitelist(&mut cmd);
        let out = cmd.output().await.expect("spawn printenv");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("secret_test_key"),
            "secret leaked into whitelisted child env:\n{stdout}"
        );
        assert!(
            stdout.contains("PATH="),
            "PATH must still reach the child:\n{stdout}"
        );
    }

    /// Mutation guard: without `env_clear()`, `Command` inherits the full
    /// ambient/override environment by default — this pins that baseline so
    /// the test above is proven to be exercising `env_clear()` specifically,
    /// not an environment that happened to be empty already. If someone
    /// deletes the `cmd.env_clear()` line from `clear_and_whitelist`, the
    /// PREVIOUS test starts behaving like this one and goes red.
    #[tokio::test]
    async fn without_env_clear_the_secret_would_have_leaked() {
        let mut cmd = Command::new("printenv");
        cmd.env("secret_test_key", "leaked-if-this-appears");
        // Deliberately NOT calling clear_and_whitelist here.
        let out = cmd.output().await.expect("spawn printenv");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("secret_test_key=leaked-if-this-appears"),
            "test baseline assumption broke: Command should inherit by default:\n{stdout}"
        );
    }

    /// A server-specific extra variable (an MCP server's own `env` block)
    /// reaches its own child and does not implicitly widen the shared
    /// whitelist for anyone else.
    #[tokio::test]
    async fn extend_adds_only_the_given_extra_vars_on_top_of_the_whitelist() {
        let mut cmd = Command::new("printenv");
        clear_whitelist_and_extend(&mut cmd, [("SERVER_A_TOKEN", "a-token")]);
        let out = cmd.output().await.expect("spawn printenv");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("SERVER_A_TOKEN=a-token"));
        assert!(stdout.contains("PATH="));
        assert!(!stdout.contains("SERVER_B_TOKEN"));
    }
}
