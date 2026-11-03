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
