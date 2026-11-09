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

/// Self-check failures never contain manifest values or parser diagnostics.
#[derive(Debug)]
pub enum ManifestCheckError {
    InvalidYaml { origin: &'static str },
    InvalidMapping { origin: &'static str },
    Mismatch { keys: String },
    Read(std::io::Error),
    NotRegular,
    TooLarge,
}

impl std::fmt::Display for ManifestCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidYaml { origin } => write!(f, "{origin} manifest is invalid YAML"),
            Self::InvalidMapping { origin } => write!(
                f,
                "{origin} manifest must be a mapping with string top-level keys"
            ),
            Self::Mismatch { keys } => write!(
                f,
                "manifest differs from compiled baseline at top-level keys: {keys}"
            ),
            Self::Read(error) => write!(f, "reading published manifest: {error}"),
            Self::NotRegular => write!(f, "published manifest must be a regular file"),
            Self::TooLarge => write!(f, "published manifest exceeds YAML size limit"),
        }
    }
}

impl std::error::Error for ManifestCheckError {}

impl From<std::io::Error> for ManifestCheckError {
    fn from(error: std::io::Error) -> Self {
        Self::Read(error)
    }
}

fn parse_baseline(
    yaml: &[u8],
    origin: &'static str,
) -> Result<serde_yaml::Mapping, ManifestCheckError> {
    let value: serde_yaml::Value =
        serde_yaml::from_slice(yaml).map_err(|_| ManifestCheckError::InvalidYaml { origin })?;
    match value {
        serde_yaml::Value::Mapping(mapping) if mapping.keys().all(|key| key.as_str().is_some()) => {
            Ok(mapping)
        }
        _ => Err(ManifestCheckError::InvalidMapping { origin }),
    }
}

/// Select exact handshake bytes, comparing published semantics against the compiled baseline.
/// Only the published document's top-level `integrity` is excluded.
///
/// # Errors
/// Invalid YAML/mappings or changed semantics; diagnostics never echo untrusted text.
pub fn select_manifest<'a>(
    embedded: &'a [u8],
    published: Option<&'a [u8]>,
) -> Result<&'a [u8], ManifestCheckError> {
    let Some(published) = published else {
        return Ok(embedded);
    };
    let baseline = parse_baseline(embedded, "embedded")?;
    let mut disk = parse_baseline(published, "on-disk")?;
    disk.remove(serde_yaml::Value::String("integrity".into()));
    if baseline != disk {
        let mut keys: Vec<_> = baseline
            .keys()
            .filter(|key| baseline.get(*key) != disk.get(*key))
            .filter_map(serde_yaml::Value::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if disk.keys().any(|key| !baseline.contains_key(key)) {
            keys.push("<unknown keys>");
        }
        return Err(ManifestCheckError::Mismatch {
            keys: keys.join(", "),
        });
    }
    Ok(published)
}

/// Read a published manifest without following symlinks, with a bounded allocation.
/// The explicit path lets development/tests avoid changing the process cwd.
///
/// # Errors
/// Every read failure except NotFound, non-regular files, and oversized files.
pub fn read_published_manifest(
    path: &std::path::Path,
) -> Result<Option<Vec<u8>>, ManifestCheckError> {
    use rustix::fs::{Mode, OFlags, open};
    use std::io::Read;
    let fd = match open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(ManifestCheckError::Read(error.into())),
    };
    let file = std::fs::File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(ManifestCheckError::NotRegular);
    }
    let limit = agent24_domain::DomainOsManifest::MAX_YAML_BYTES;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(ManifestCheckError::TooLarge);
    }
    Ok(Some(bytes))
}

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

#[cfg(test)]
mod selfcheck_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    const BASE: &[u8] = b"name: module\nspawn: {command: bin/module}\n";

    #[test]
    fn absent_and_reformatted_preserve_bytes() {
        assert_eq!(select_manifest(BASE, None).unwrap(), BASE);
        let disk =
            b"# published\nintegrity: {files: {}}\nspawn:\n  command: bin/module\nname: module\n";
        assert_eq!(select_manifest(BASE, Some(disk)).unwrap(), disk);
    }

    #[test]
    fn changes_report_only_known_keys() {
        for (disk, keys) in [
            ("name: secret\nspawn: {command: bin/module}\n", "name"),
            ("name: module\n", "spawn"),
            (
                "name: secret\nsecret-key: secret-value\n",
                "name, spawn, <unknown keys>",
            ),
            (
                "name: module\nspawn: {command: bin/module}\nsecret-key: secret-value\n",
                "<unknown keys>",
            ),
        ] {
            let error = select_manifest(BASE, Some(disk.as_bytes())).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("manifest differs from compiled baseline at top-level keys: {keys}")
            );
            assert!(!format!("{error:?}").contains("secret"));
        }
    }

    #[test]
    fn invalid_yaml_duplicate_keys_and_documents_are_rejected() {
        for disk in [
            "name: [secret",
            "name: a\nname: b",
            "spawn: {command: a, command: b}",
            "name: a\n---\nname: b",
            "---\n---",
        ] {
            for (embedded, published, origin) in [
                (BASE, disk.as_bytes(), "on-disk"),
                (disk.as_bytes(), BASE, "embedded"),
            ] {
                let error = select_manifest(embedded, Some(published)).unwrap_err();
                assert!(
                    matches!(error, ManifestCheckError::InvalidYaml { origin: actual } if actual == origin),
                    "{error:?}"
                );
            }
        }
    }

    #[test]
    fn non_mapping_and_non_string_keys_are_rejected() {
        for disk in ["", "[]", "42", "1: secret"] {
            for (embedded, published) in [(BASE, disk.as_bytes()), (disk.as_bytes(), BASE)] {
                assert!(matches!(
                    select_manifest(embedded, Some(published)),
                    Err(ManifestCheckError::InvalidMapping { .. })
                ));
            }
        }
    }

    #[test]
    fn file_missing_falls_back_but_other_open_errors_do_not() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            read_published_manifest(&dir.path().join("absent"))
                .unwrap()
                .is_none()
        );
        let path = dir.path().join("file");
        std::fs::write(&path, BASE).unwrap();
        assert_eq!(read_published_manifest(&path).unwrap().unwrap(), BASE);
        assert!(matches!(
            read_published_manifest(&path.join("child")),
            Err(ManifestCheckError::Read(_))
        ));
    }

    #[test]
    fn file_links_and_non_regular_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, BASE).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(read_published_manifest(&link).is_err());
        std::fs::remove_file(&file).unwrap();
        assert!(read_published_manifest(&link).is_err());
        assert!(matches!(
            read_published_manifest(dir.path()),
            Err(ManifestCheckError::NotRegular)
        ));
        let fifo = dir.path().join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        assert!(matches!(
            read_published_manifest(&fifo),
            Err(ManifestCheckError::NotRegular)
        ));
    }

    #[test]
    fn file_size_boundary_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let limit = agent24_domain::DomainOsManifest::MAX_YAML_BYTES;
        std::fs::write(&path, vec![b'#'; limit]).unwrap();
        assert_eq!(
            read_published_manifest(&path).unwrap().unwrap().len(),
            limit
        );
        std::fs::write(&path, vec![b'#'; limit + 1]).unwrap();
        assert!(matches!(
            read_published_manifest(&path),
            Err(ManifestCheckError::TooLarge)
        ));
    }
}
