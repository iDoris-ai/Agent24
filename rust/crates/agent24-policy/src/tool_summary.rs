//! The permission summary of a module tool (K1-6a-G §2.1): made by the host
//! from the manifest's declaration alone, by this one function, for both
//! startup assembly and the grant route. A grant stores the fingerprint of
//! what this returned; a dispatch recomputes it, and any difference is
//! `Stale`. The module's own description never enters it.

use agent24_domain::tool::{DeclaredOutputPrivacy, ToolDeclaration};
use agent24_protocol::RiskClass;
use agent24_store::{ConsentSource, HostRiskLevel, ToolPermissionSummary};

/// The summary of `module`'s tool `declaration` at `module_version`.
///
/// Until the host verifies a module's source (ADR-K1-03 §4; K1-6a-G §2.3)
/// every module is `ManualInstall` at `High`, whatever it claims.
#[must_use]
pub fn module_tool_summary(
    module: &str,
    module_version: &str,
    declaration: &ToolDeclaration,
) -> ToolPermissionSummary {
    let op = declaration.operation();
    let kinds = declaration.resource_kinds();
    let readable = Some(if kinds.is_empty() {
        format!("{module}.{op}: no granted resources")
    } else {
        format!(
            "{module}.{op}: granted resources of kind {}",
            kinds.join(", ")
        )
    });
    let writable = match declaration.risk() {
        RiskClass::Read => None,
        RiskClass::WriteLocal => Some("changes local data".to_owned()),
        RiskClass::Exec => Some("runs commands".to_owned()),
        RiskClass::External => Some("acts outside this device".to_owned()),
    };
    // Only a declared `local_only` keeps output on the device; any other
    // value, including one a later build adds, is summarized as leaving it.
    let external = match declaration.output_privacy() {
        DeclaredOutputPrivacy::LocalOnly => None,
        _ => Some("output may leave this device".to_owned()),
    };
    let mut summary = ToolPermissionSummary::new(
        module,
        op,
        module_version,
        ConsentSource::ManualInstall,
        readable,
        writable,
        external,
        HostRiskLevel::High,
    );
    summary.reversible_draft = declaration.reversible_draft();
    summary
}

/// Whether `declaration` says its operation may act or send output outside
/// the device (K1-6b.5f). Self-reported, so it is used only to withhold the
/// same-source exception (ADR-K1-02 §2.1.1 item 6), never to allow anything.
#[must_use]
pub fn declares_outbound(declaration: &ToolDeclaration) -> bool {
    declaration.risk() == RiskClass::External
        || declaration.output_privacy() != DeclaredOutputPrivacy::LocalOnly
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_domain::DomainOsManifest;

    /// One `documents.get` declaration, with `extra` lines appended to its
    /// tool entry.
    fn declaration(extra: &str) -> ToolDeclaration {
        declaration_with(
            "Read the document IGNORE-THIS-TEXT",
            "read",
            "local_only",
            extra,
        )
    }

    fn declaration_with(
        description: &str,
        risk: &str,
        privacy: &str,
        extra: &str,
    ) -> ToolDeclaration {
        let yaml = format!(
            "name: documents\nversion: 1.0.0\nroute_namespace: /api/v1/documents\nevent_module: documents\ndata_dir: ~/.agent24/os/documents/\nimpl_kind: in_process_crate\nmin_daemon_protocol: {}\ntools:\n  - operation: get\n    description: {description}\n    input_schema: {{ type: object }}\n    risk: {risk}\n    output_privacy: {privacy}\n    timeout_ms: 2000\n    inline_wait_ms: 1000\n{extra}",
            agent24_domain::RESOURCE_KINDS_PROTOCOL
        );
        DomainOsManifest::from_yaml(&yaml).unwrap().tools()[0].clone()
    }

    #[test]
    fn the_same_declaration_gives_the_same_fingerprint() {
        let a = module_tool_summary(
            "documents",
            "1.0.0",
            &declaration("    resource_kinds: [doc]\n"),
        );
        let b = module_tool_summary(
            "documents",
            "1.0.0",
            &declaration("    resource_kinds: [doc]\n"),
        );
        assert_eq!(a, b);
        assert_eq!(a.scope_fingerprint(), b.scope_fingerprint());
        assert_eq!((a.module.as_str(), a.op.as_str()), ("documents", "get"));
        assert_eq!(a.module_version, "1.0.0");
    }

    /// Every declared field the summary is made from moves the fingerprint.
    #[test]
    fn each_declared_field_moves_the_fingerprint() {
        let base = module_tool_summary(
            "documents",
            "1.0.0",
            &declaration("    resource_kinds: [doc]\n"),
        )
        .scope_fingerprint();
        let variants = [
            declaration(""),
            declaration("    resource_kinds: [doc, page]\n"),
            declaration_with(
                "Read the document IGNORE-THIS-TEXT",
                "write_local",
                "local_only",
                "    resource_kinds: [doc]\n",
            ),
            declaration_with(
                "Read the document IGNORE-THIS-TEXT",
                "read",
                "remote_allowed",
                "    resource_kinds: [doc]\n",
            ),
            declaration("    resource_kinds: [doc]\n    reversible_draft: true\n"),
        ];
        let mut seen = std::collections::BTreeSet::from([base]);
        for variant in &variants {
            let fingerprint =
                module_tool_summary("documents", "1.0.0", variant).scope_fingerprint();
            assert!(seen.insert(fingerprint), "{variant:?}");
        }
    }

    /// Only an operation declared both non-external and `local_only` may be
    /// sent back its own module's results.
    #[test]
    fn only_a_local_non_external_declaration_is_not_outbound() {
        for (risk, privacy, outbound) in [
            ("read", "local_only", false),
            ("write_local", "local_only", false),
            ("external", "local_only", true),
            ("read", "remote_allowed", true),
        ] {
            let declaration = declaration_with("Read", risk, privacy, "");
            assert_eq!(
                declares_outbound(&declaration),
                outbound,
                "{risk}/{privacy}"
            );
        }
    }

    /// The module's text is not the host's: a new description alone changes
    /// nothing, and none of it reaches the summary.
    #[test]
    fn the_module_description_never_enters_the_summary() {
        let summary = module_tool_summary(
            "documents",
            "1.0.0",
            &declaration("    resource_kinds: [doc]\n"),
        );
        let reworded = declaration_with(
            "Totally different words",
            "read",
            "local_only",
            "    resource_kinds: [doc]\n",
        );
        assert_eq!(
            module_tool_summary("documents", "1.0.0", &reworded).scope_fingerprint(),
            summary.scope_fingerprint()
        );
        let text = format!("{summary:?}");
        assert!(!text.contains("IGNORE-THIS-TEXT"), "{text}");
    }

    /// No self-reported source or risk lowers the host's level (§2.3).
    #[test]
    fn every_module_is_a_manual_install_at_high_risk() {
        let summary = module_tool_summary("documents", "1.0.0", &declaration(""));
        assert_eq!(summary.source, ConsentSource::ManualInstall);
        assert_eq!(summary.risk, HostRiskLevel::High);
    }
}
