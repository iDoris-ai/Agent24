//! D9 snapshots for domain OS modules (ADR-038 C7).
//!
//! A package's manifest is the expectation. `requires.kernel` is the manifest's
//! `kernel_capabilities` (what the module asks the kernel for, not what it
//! provides). `provides` is the manifest's capability list, which may be empty
//! when the package has not declared any.

use agent24_domain::DomainOsManifest;

/// ADR-038 D9 capability id: `[a-z0-9][a-z0-9-]{1,31}`.
pub(crate) fn capability_id_ok(id: &str) -> bool {
    (2..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && id
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Capability ids that `docs/capabilities/REGISTRY.md` ties to one provider
/// (column 提供方). Only that provider may declare them. A drift test pins this
/// table to the markdown.
pub(crate) const REGISTERED_PROVIDERS: &[(&str, &str)] = &[
    ("model-gateway", "idoris"),
    ("nostr-dm", "hyphae"),
    ("nostr-group", "hyphae"),
    ("nostr-identity", "hyphae"),
    ("nostr-relay", "hyphae"),
    ("documents", "documents"),
];

/// Whether module `owner` may not declare capability `id` in its `domain-os.yml`.
/// Reserved: `mcp-*` (generated for MCP servers), a builtin component id, and a
/// registered id whose registry provider is another module.
pub(crate) fn capability_id_reserved(owner: &str, id: &str) -> bool {
    // The Documents package comes from the desktop bundle and may be another
    // build than this daemon, so it is held to the compiled-in D9 catalog.
    if owner == "documents" {
        return !crate::component_routes::documents_provide_ids().contains(&id.to_owned());
    }
    if id.starts_with("mcp-") {
        return true;
    }
    if let Some((_, provider)) = REGISTERED_PROVIDERS.iter().find(|(cap, _)| *cap == id) {
        return *provider != owner;
    }
    crate::component_routes::BUILTIN_COMPONENT_IDS.contains(&id) && id != owner
}

impl OsD9 {
    /// The first declared capability id this module may not declare.
    pub(crate) fn reserved_capability(&self) -> Option<&str> {
        self.provides
            .iter()
            .map(|p| p.id.as_str())
            .find(|id| capability_id_reserved(&self.name, id))
    }

    /// The same record with no capabilities, for a refused module: it provides nothing.
    pub(crate) fn without_provides(mut self) -> Self {
        self.provides.clear();
        self
    }
}

/// One declared capability, carried onto `/components`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapProvide {
    pub id: String,
    pub tools: Vec<String>,
}

/// The part of a domain-OS manifest `/components` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OsD9 {
    pub name: String,
    pub in_process: bool,
    pub provides: Vec<CapProvide>,
    pub requires_kernel: Vec<String>,
}

impl OsD9 {
    pub(crate) fn from_manifest(manifest: &DomainOsManifest) -> Self {
        Self {
            name: manifest.name().to_owned(),
            in_process: manifest.is_mountable_in_process(),
            provides: manifest
                .provides()
                .iter()
                .map(|p| CapProvide {
                    id: p.id().to_owned(),
                    tools: p.tools().to_vec(),
                })
                .collect(),
            requires_kernel: manifest
                .kernel_capabilities()
                .iter()
                .map(|c| c.as_str().to_owned())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn reserved_ids_follow_the_registry_provider_the_builtins_and_mcp() {
        // registry id, other provider / own provider
        assert!(capability_id_reserved("sin90", "model-gateway"));
        assert!(capability_id_reserved("sin90", "nostr-relay"));
        assert!(capability_id_reserved("sin90", "documents"));
        assert!(!capability_id_reserved("documents", "documents"));
        // generated MCP ids and builtin component ids
        assert!(capability_id_reserved("sin90", "mcp-files"));
        assert!(capability_id_reserved("documents", "mcp-files"));
        for id in crate::component_routes::BUILTIN_COMPONENT_IDS {
            assert!(capability_id_reserved("sin90", id), "{id}");
        }
        // ordinary
        assert!(!capability_id_reserved("sin90", "sin90-tasks"));
        assert!(!capability_id_reserved("sin90", "mcpfiles"));
    }

    /// The table is a copy of REGISTRY.md's 提供方 column. If one moves, this goes red.
    #[test]
    fn registered_providers_match_registry_md() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/capabilities/REGISTRY.md");
        let markdown = std::fs::read_to_string(&path).unwrap();
        let mut seen = Vec::new();
        for line in markdown
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with('|'))
        {
            let cells: Vec<_> = line.split('|').map(str::trim).collect();
            let Some(id) = cells.get(1).map(|c| c.trim_matches('`')) else {
                continue;
            };
            if !capability_id_ok(id) || id == "id" {
                continue;
            }
            assert!(
                cells.len() > 5,
                "REGISTRY.md row has too few columns for the 提供方 column (index 5): {line}"
            );
            seen.push((id.to_owned(), cells[5].to_owned()));
        }
        let mut table: Vec<_> = REGISTERED_PROVIDERS
            .iter()
            .map(|(c, p)| ((*c).to_owned(), (*p).to_owned()))
            .collect();
        table.sort();
        seen.sort();
        assert_eq!(table, seen);
    }
}
