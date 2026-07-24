//! H2: user-local risk overrides.
//!
//! A conservative default that cannot be relaxed is a feature users switch off.
//! Every MCP tool is classified `external` (E1) because it is third-party code
//! whose effects we cannot bound — correct, but it means a filesystem MCP
//! server prompts on every `read_file`, and after the third prompt the user
//! disconnects the server. This is the release valve: the person who owns the
//! machine marks that server's read-only tools `read` and they stop gating.
//!
//! ## Why this exists next to D3 Guardian rather than instead of it
//!
//! Guardian can already auto-approve low-risk gated calls, but it is a model
//! inference: non-deterministic, re-run per call, and OFF by default because
//! letting a model approve tool calls must be a deliberate operator choice.
//! An override is the opposite kind of thing — a rule the user wrote down, that
//! holds identically every time, that they can list and revoke.
//!
//! The two compose without a precedence fight, because they act at different
//! points: an override decides the CLASS, the class decides whether the gate
//! runs at all, and only then does the gate consult Guardian. Relaxing a tool
//! to `read` therefore doesn't "beat" Guardian — it means there is no gated
//! call for Guardian to have an opinion about.
//!
//! ## The inviolable rule
//!
//! **This store is user-local and is NEVER written by a module, persona, or MCP
//! server.** A package may declare what tools it wants; only the user decides
//! how far to trust them. An install path that could write here would let a
//! marketplace entry ship its own exemption.

use std::sync::RwLock;

use agent24_protocol::RiskClass;
use agent24_store::{RiskOverrideRow, Store, StoreError};
use agent24_tools::RiskOverrides;

/// Glob match over a tool name supporting `*` (any run, including empty) and
/// `?` (exactly one char).
///
/// Hand-rolled rather than pulled from a crate for one reason that matters: the
/// classic two-pointer algorithm below backtracks only to the last `*`, so it
/// is O(n·m) worst case with no recursion. A regex translation of user-supplied
/// patterns would put catastrophic backtracking on the approval path, which is
/// the last place that belongs.
fn glob_match(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0usize, 0usize);
    // Position to resume from if the current `*` expansion turns out too short.
    let (mut star, mut resume) = (None, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            resume = ni;
            pi += 1;
        } else if let Some(s) = star {
            // Backtrack: let the last `*` swallow one more character.
            pi = s + 1;
            resume += 1;
            ni = resume;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// How specific a pattern is. More literal characters = more specific, and a
/// pattern with no wildcards at all outranks every glob regardless of length —
/// naming one tool exactly is a stronger statement of intent than any pattern
/// that happens to cover it.
fn specificity(pattern: &str) -> usize {
    let literals = pattern.chars().filter(|c| !matches!(c, '*' | '?')).count();
    if pattern.contains(['*', '?']) {
        literals
    } else {
        literals + 1000
    }
}

/// The user's rules, resolved against tool names.
///
/// Rules are snapshotted into memory at load; [`Self::reload`] refreshes them
/// after a write. Resolution happens on the dispatch path, so it must not touch
/// the database.
pub struct RiskOverrideStore {
    /// Sorted at load: most specific first, then pattern ascending so equally
    /// specific rules resolve the same way on every process start (insertion
    /// order would not).
    rules: RwLock<Vec<(String, RiskClass)>>,
}

impl RiskOverrideStore {
    pub fn from_rows(rows: Vec<RiskOverrideRow>) -> Self {
        let store = Self {
            rules: RwLock::new(Vec::new()),
        };
        store.replace(rows);
        store
    }

    /// Load the user's rules from the database.
    pub async fn load(store: &Store) -> Result<Self, StoreError> {
        Ok(Self::from_rows(store.list_risk_overrides().await?))
    }

    /// Re-read after the user adds or removes a rule.
    pub async fn reload(&self, store: &Store) -> Result<(), StoreError> {
        self.replace(store.list_risk_overrides().await?);
        Ok(())
    }

    fn replace(&self, rows: Vec<RiskOverrideRow>) {
        let mut sorted: Vec<(String, RiskClass)> = rows
            .into_iter()
            .map(|r| (r.pattern, r.risk_class))
            .collect();
        sorted.sort_by(|a, b| {
            specificity(&b.0)
                .cmp(&specificity(&a.0))
                .then_with(|| a.0.cmp(&b.0))
        });
        match self.rules.write() {
            Ok(mut guard) => *guard = sorted,
            // A poisoned lock means a panic while holding it. Keeping the old
            // snapshot is the safe read: stale rules were user-authored too,
            // and dropping them silently re-tightens tools the user relaxed.
            Err(err) => *err.into_inner() = sorted,
        }
    }

    pub fn len(&self) -> usize {
        self.rules.read().map_or(0, |r| r.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl RiskOverrides for RiskOverrideStore {
    fn resolve(&self, tool_name: &str) -> Option<RiskClass> {
        let rules = self.rules.read().ok()?;
        rules
            .iter()
            .find(|(pattern, _)| glob_match(pattern, tool_name))
            .map(|(_, class)| *class)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn row(pattern: &str, risk_class: RiskClass) -> RiskOverrideRow {
        RiskOverrideRow {
            pattern: pattern.to_owned(),
            risk_class,
            source: "cli".to_owned(),
            created_at: "2026-07-25T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn glob_basics() {
        assert!(glob_match("mcp_fs_*", "mcp_fs_read-file"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("mcp_fs_read", "mcp_fs_read"));
        assert!(glob_match("mcp_?s_read", "mcp_fs_read"));
        assert!(!glob_match("mcp_fs_*", "mcp_git_status"));
        assert!(!glob_match("mcp_fs_read", "mcp_fs_read2"));
        assert!(!glob_match("mcp_?_read", "mcp_fs_read"));
        // trailing star may swallow nothing
        assert!(glob_match("exact*", "exact"));
    }

    /// The backtracking path: several `*` that each have to give ground before
    /// the tail matches. Included because a naive matcher passes the simple
    /// cases above and fails exactly here.
    #[test]
    fn glob_backtracks_across_multiple_stars() {
        assert!(glob_match("*_fs_*_file", "mcp_fs_read_file"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
        assert!(glob_match("*a*a*a", "aaaa"));
    }

    #[test]
    fn exact_pattern_beats_any_glob() {
        let store = RiskOverrideStore::from_rows(vec![
            row("mcp_fs_*", RiskClass::Read),
            row("mcp_fs_write-file", RiskClass::External),
        ]);
        assert_eq!(store.resolve("mcp_fs_read-file"), Some(RiskClass::Read));
        assert_eq!(
            store.resolve("mcp_fs_write-file"),
            Some(RiskClass::External),
            "the exactly-named tool must win over the pattern covering it"
        );
    }

    #[test]
    fn longer_literal_prefix_beats_shorter() {
        let store = RiskOverrideStore::from_rows(vec![
            row("mcp_*", RiskClass::External),
            row("mcp_fs_*", RiskClass::Read),
        ]);
        assert_eq!(store.resolve("mcp_fs_read"), Some(RiskClass::Read));
        assert_eq!(store.resolve("mcp_git_push"), Some(RiskClass::External));
    }

    /// Two equally specific patterns that both match must resolve identically
    /// on every process start — otherwise a user's effective permissions would
    /// depend on row order, which is exactly the kind of thing nobody debugs.
    #[test]
    fn equal_specificity_ties_break_deterministically() {
        let a = RiskOverrideStore::from_rows(vec![
            row("mcp_a*_x", RiskClass::Read),
            row("mcp_*a_x", RiskClass::Exec),
        ]);
        let b = RiskOverrideStore::from_rows(vec![
            row("mcp_*a_x", RiskClass::Exec),
            row("mcp_a*_x", RiskClass::Read),
        ]);
        assert_eq!(a.resolve("mcp_aa_x"), b.resolve("mcp_aa_x"));
        // The tie-break is the lexicographically first pattern; `*` (0x2A)
        // sorts before `a`, so `mcp_*a_x` wins over `mcp_a*_x`. The rule that
        // matters is that it is fixed, not which side it happens to pick.
        assert_eq!(a.resolve("mcp_aa_x"), Some(RiskClass::Exec));
    }

    #[test]
    fn no_match_defers_to_the_declared_class() {
        let store = RiskOverrideStore::from_rows(vec![row("mcp_fs_*", RiskClass::Read)]);
        assert_eq!(store.resolve("shell_exec"), None);
    }
}
