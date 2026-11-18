//! Kernel-capability consent for an out-of-process OS package.
//!
//! The trust list is unchanged. This module only answers whether a package
//! that is already allowed to load skips the consent card, and what the
//! permission fingerprint of its `requires` declaration is.
//!
//! Exempt only when both hold: the module name is on the bundled list
//! (`documents`, `sin90`), and on macOS the signing team is the built-in
//! team. The list is by module name, not by how the package was installed.
//! Attach and MCP are not classified here.

use serde::Serialize;

/// Modules that ship inside the desktop app. OSI-2 added `sin90`.
pub const BUNDLED_OS_MODULES: &[&str] = &["documents", "sin90"];

/// Host field on an OS package row. `missing` means the card applies.
/// It does not by itself change what is granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelConsentClass {
    Exempt,
    Missing,
}

/// `exempt` when `name` is bundled and, on macOS, `team` is the built-in
/// Developer ID team. Anywhere else a bundled name is exempt: there is no
/// team to read. A missing team on macOS is not exempt.
pub fn classify(name: &str, team: Option<&str>, macos: bool) -> KernelConsentClass {
    if !BUNDLED_OS_MODULES.contains(&name) {
        return KernelConsentClass::Missing;
    }
    if !macos {
        return KernelConsentClass::Exempt;
    }
    if team == Some(agent24_os_proc::DEVELOPER_ID_TEAM_ID) {
        KernelConsentClass::Exempt
    } else {
        KernelConsentClass::Missing
    }
}

/// Sorted `requires.kernel` names, then sorted `requires.operations`.
/// The version is not part of it. The two sides stay labeled so a kernel
/// name cannot collide with an operation name.
pub fn fingerprint(kernel: &[String], operations: &[String]) -> String {
    let mut kernel: Vec<&str> = kernel.iter().map(String::as_str).collect();
    kernel.sort_unstable();
    kernel.dedup();
    let mut operations: Vec<&str> = operations.iter().map(String::as_str).collect();
    operations.sort_unstable();
    operations.dedup();
    format!("k={};o={}", kernel.join(","), operations.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_builtin_team_is_exempt_and_any_other_team_is_missing() {
        let builtin = agent24_os_proc::DEVELOPER_ID_TEAM_ID;
        assert_eq!(
            classify("documents", Some(builtin), true),
            KernelConsentClass::Exempt
        );
        assert_eq!(
            classify("sin90", Some(builtin), true),
            KernelConsentClass::Exempt
        );
        assert_eq!(
            classify("documents", Some("OTHERTEAM1"), true),
            KernelConsentClass::Missing
        );
        assert_eq!(
            classify("sin90", None, true),
            KernelConsentClass::Missing,
            "macOS with no team read is not exempt"
        );
        assert_eq!(
            classify("widget", Some(builtin), true),
            KernelConsentClass::Missing
        );
    }

    #[test]
    fn a_bundled_name_is_exempt_where_there_is_no_team_to_read() {
        assert_eq!(
            classify("documents", None, false),
            KernelConsentClass::Exempt
        );
        assert_eq!(classify("sin90", None, false), KernelConsentClass::Exempt);
        assert_eq!(classify("widget", None, false), KernelConsentClass::Missing);
    }

    #[test]
    fn the_fingerprint_ignores_version_and_changes_when_a_capability_is_added() {
        let events = fingerprint(&["events".into()], &[]);
        assert_eq!(events, fingerprint(&["events".into()], &[]));
        assert_ne!(
            events,
            fingerprint(&["events".into(), "models".into()], &[])
        );
        assert_eq!(
            fingerprint(&["models".into(), "events".into()], &[]),
            fingerprint(&["events".into(), "models".into()], &[])
        );
        assert_ne!(
            fingerprint(&["events".into()], &[]),
            fingerprint(&["events".into()], &["scheduler.upsert".into()])
        );
    }
}
