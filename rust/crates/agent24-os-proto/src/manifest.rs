//! ME4-S3 §4.4 — manifest facts a module-side caller needs before it has a
//! connection, and the digest both sides compute the same way.
//!
//! `manifest_digest` moved here from `agent24-os-packages/src/discovery.rs`
//! (ME4-S3 §4.4, M4): the kernel's install-time discovery and a module's
//! handshake now compute the manifest digest with the same function.
//! `os-packages` keeps its old path working by re-exporting this one.
//!
//! `ManifestFacts`/`facts_from_yaml` are new: a **lenient** read of the three
//! fields a module needs out of its own `domain-os.yml` to build its
//! `initialize` request (name, route namespace, requested capabilities) —
//! deliberately NOT `agent24_domain::DomainOsManifest`, which is strict
//! (`deny_unknown_fields`, requires `impl_kind`/`spawn`/etc.) and validates
//! fields no module-side caller needs to enforce a second time; the kernel
//! already validated the manifest before it spawned the module (§4.4).

use serde::Deserialize;

/// The three fields a module reads out of its own manifest text to build its
/// `initialize` request. Anything else in the YAML is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFacts {
    pub name: String,
    pub route_namespace: String,
    pub kernel_capabilities: Vec<String>,
}

/// Lenient wire shape: no `deny_unknown_fields` (L4) — this is a read, not a
/// validation; the kernel already validated the manifest at mount time.
#[derive(Deserialize)]
struct RawFacts {
    name: String,
    route_namespace: String,
    #[serde(default)]
    kernel_capabilities: Vec<String>,
}

/// Parse the three facts out of a `domain-os.yml` document.
///
/// # Errors
/// The document is not valid YAML, or is missing `name`/`route_namespace`.
pub fn facts_from_yaml(yaml: &str) -> Result<ManifestFacts, String> {
    let raw: RawFacts = serde_yaml::from_str(yaml).map_err(|e| e.to_string())?;
    Ok(ManifestFacts {
        name: raw.name,
        route_namespace: raw.route_namespace,
        kernel_capabilities: raw.kernel_capabilities,
    })
}

/// `sha256:` and the lowercase hex of `bytes`: the manifest digest format
/// (SPEC §3). Moved from `agent24-os-packages/src/discovery.rs` (ME4-S3 §4.4,
/// M4) so the kernel's discovery and a module's handshake share one
/// function.
#[must_use]
pub fn manifest_digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for b in hash {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn digest_is_sha256_hex_with_prefix() {
        assert_eq!(
            manifest_digest(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn facts_from_yaml_reads_three_fields_and_ignores_the_rest() {
        let yaml = "name: sin90\nroute_namespace: /api/v1/sin90\nkernel_capabilities: [events, memory]\nimpl_kind: subprocess\nspawn:\n  command: bin/sin90\n";
        let facts = facts_from_yaml(yaml).unwrap();
        assert_eq!(facts.name, "sin90");
        assert_eq!(facts.route_namespace, "/api/v1/sin90");
        assert_eq!(facts.kernel_capabilities, vec!["events", "memory"]);
    }

    #[test]
    fn facts_from_yaml_defaults_missing_kernel_capabilities_to_empty() {
        let yaml = "name: minimal\nroute_namespace: /api/v1/minimal\n";
        let facts = facts_from_yaml(yaml).unwrap();
        assert!(facts.kernel_capabilities.is_empty());
    }

    #[test]
    fn facts_from_yaml_rejects_missing_required_field() {
        let yaml = "name: minimal\n";
        assert!(facts_from_yaml(yaml).is_err());
    }
}
