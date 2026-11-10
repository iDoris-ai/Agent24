//! S1-7 gate. These checks fail when a dormant chain is deleted, compiled
//! back into the default build, or when `agent24-worker` returns to the
//! default member set.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

fn rust_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("rust workspace root")
}

fn read(rel: &str) -> String {
    let path = rust_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {rel}: {err}"))
}

/// Names in the `[features]` table's `default` array. No `default` key means
/// an empty list. A `#` comment, including one that spells the array, is not
/// a feature.
fn feature_defaults(toml: &str) -> Vec<String> {
    let Some(rest) = toml.split("[features]").nth(1) else {
        return Vec::new();
    };
    let section = rest.split("\n[").next().unwrap_or(rest);
    let mut buf = String::new();
    let mut collecting = false;
    for line in section.lines() {
        let code = line.split('#').next().unwrap_or(line);
        let code = code.split("//").next().unwrap_or(code);
        if !collecting {
            let trimmed = code.trim();
            let Some(rest) = trimmed.strip_prefix("default") else {
                continue;
            };
            if rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                continue;
            }
            let Some(after_eq) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            collecting = true;
            buf.push_str(after_eq);
            if buf.contains(']') {
                break;
            }
        } else {
            buf.push('\n');
            buf.push_str(code);
            if code.contains(']') {
                break;
            }
        }
    }
    if !collecting {
        return Vec::new();
    }
    let inside = buf
        .split_once('[')
        .map(|(_, rest)| rest.split(']').next().unwrap_or(""))
        .unwrap_or("");
    inside
        .split(',')
        .filter_map(|part| {
            let part = part.trim().trim_matches('"');
            if part.is_empty() {
                None
            } else {
                Some(part.to_owned())
            }
        })
        .collect()
}

fn member_list(toml: &str, key: &str) -> Vec<String> {
    let marker = format!("{key} = [");
    let start = toml
        .find(&marker)
        .unwrap_or_else(|| panic!("{key} missing"));
    let rest = &toml[start + marker.len()..];
    let end = rest.find(']').unwrap_or_else(|| panic!("{key} unclosed"));
    rest[..end]
        .lines()
        .filter_map(|line| {
            let line = line
                .split("//")
                .next()
                .unwrap_or(line)
                .trim()
                .trim_matches(',');
            let line = line.trim().trim_matches('"');
            if line.is_empty() {
                None
            } else {
                Some(line.to_owned())
            }
        })
        .collect()
}

#[test]
fn worker_stays_a_member_but_not_a_default_member() {
    let toml = read("Cargo.toml");
    let members = member_list(&toml, "members");
    let defaults = member_list(&toml, "default-members");
    assert!(
        members.iter().any(|m| m == "crates/agent24-worker"),
        "worker must stay a workspace member so --workspace and -p still build it"
    );
    assert!(
        !defaults.iter().any(|m| m.contains("agent24-worker")),
        "worker must not be a default member"
    );
    assert!(defaults.len() + 1 == members.len());
    let worker = read("crates/agent24-worker/Cargo.toml");
    assert!(
        worker.contains("experimental:"),
        "worker package description must say experimental"
    );
}

#[test]
fn store_dormant_chains_are_feature_gated_not_deleted() {
    let cargo = read("crates/agent24-store/Cargo.toml");
    assert!(cargo.contains("dormant-workspace = []"));
    assert!(
        !cargo.contains("default = [") || !cargo.contains("dormant-workspace"),
        "dormant-workspace must not be a default feature"
    );
    let lib = read("crates/agent24-store/src/lib.rs");
    for module in [
        "allocation_commitment",
        "allocation_materialization",
        "allocation_registration",
        "allocation_reservation",
        "allocation_retention_evidence",
        "allocation_retention_expectation",
        "allocation_retention_plan",
        "allocation_retention_writer",
        "terminal_core",
        "terminal_mutations",
        "terminal_plan",
    ] {
        let needle = format!("mod {module};");
        let start = lib
            .find(&needle)
            .unwrap_or_else(|| panic!("{module} missing from lib.rs"));
        let window = &lib[start.saturating_sub(320)..start];
        assert!(
            window.contains("#[cfg(feature = \"dormant-workspace\")]"),
            "{module} must stay, behind dormant-workspace"
        );
        assert!(
            window.contains("dormant-workspace:"),
            "{module} needs a module-level dead_code reason while it has no caller"
        );
        let path = format!("crates/agent24-store/src/{module}.rs");
        assert!(
            rust_root().join(&path).is_file(),
            "{path} was deleted; G series still needs it"
        );
        let src = read(&path);
        assert!(
            !src.starts_with("#![allow(dead_code)]"),
            "{module} still has a file-wide dead_code allow"
        );
    }
    let helpers = read("crates/agent24-store/src/terminal_helpers.rs");
    assert!(
        helpers.contains("pub(crate) fn decode_run"),
        "decode_run stays on the product path"
    );
    assert!(
        helpers.contains(
            "#[cfg(feature = \"dormant-workspace\")]\npub(crate) async fn read_terminal_run_tx"
        ),
        "the rest of the terminal helper chain must be gated"
    );
    assert!(!helpers.contains("#![allow(dead_code)]"));
}

#[test]
fn knowledge_store_is_feature_gated_not_deleted() {
    // S2-3 moved the module into agent24-knowledge. The feature stays off
    // for both crates that can turn it on. The array is parsed, so a longer
    // `default` list cannot hide `knowledge-store` inside it.
    let multiline = "\
[features]
# default = [\"knowledge-store\"]
knowledge-store = [\"dep:sqlx\"]
default = [
    \"other\",
    \"knowledge-store\",
]
";
    assert!(
        feature_defaults(multiline)
            .iter()
            .any(|name| name == "knowledge-store")
    );
    let commented = "[features]\n# default = [\"knowledge-store\"]\nknowledge-store = []\n";
    assert!(
        !feature_defaults(commented)
            .iter()
            .any(|name| name == "knowledge-store")
    );
    for rel in [
        "crates/agent24-knowledge/Cargo.toml",
        "apps/agent24d/Cargo.toml",
    ] {
        let cargo = read(rel);
        assert!(
            !feature_defaults(&cargo)
                .iter()
                .any(|name| name == "knowledge-store"),
            "{rel}: knowledge-store must not be a default feature"
        );
    }
    let cargo = read("crates/agent24-knowledge/Cargo.toml");
    assert!(cargo.contains("knowledge-store = [\"dep:sqlx\"]"));
    let lib = read("crates/agent24-knowledge/src/lib.rs");
    let start = lib.find("mod store;").expect("store module");
    let window = &lib[start.saturating_sub(240)..start];
    assert!(window.contains("#[cfg(feature = \"knowledge-store\")]"));
    assert!(window.contains("knowledge-store has no production caller"));
    assert!(
        !window.contains("allow(dead_code)"),
        "knowledge_store must not sit under a dead_code allow"
    );
    assert!(
        rust_root()
            .join("crates/agent24-knowledge/src/store/mod.rs")
            .is_file()
    );
    let daemon = read("apps/agent24d/src/main.rs");
    assert!(
        !daemon.contains("mod knowledge_store;"),
        "the store must not be compiled into the default daemon"
    );
}

#[test]
fn sidecar_host_is_marked_frozen_and_has_no_product_dependency() {
    let cargo = read("apps/agent24-sidecar-host/Cargo.toml");
    assert!(cargo.contains("frozen (ADR-038 Q1=(b))"));
    let lib = read("apps/agent24-sidecar-host/src/lib.rs");
    assert!(lib.contains("frozen (ADR-038 Q1=(b))"));
    assert!(
        !lib.contains("#[allow(dead_code)]\n"),
        "per-module dead_code allows in sidecar-host were replaced by one crate allow"
    );
    let root = rust_root();
    for entry in fs::read_dir(&root)
        .unwrap()
        .chain(fs::read_dir(root.join("crates")).unwrap())
        .chain(fs::read_dir(root.join("apps")).unwrap())
    {
        let entry = entry.unwrap();
        if !entry.path().is_dir() {
            continue;
        }
        let toml_path = entry.path().join("Cargo.toml");
        if !toml_path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "agent24-sidecar-host" {
            continue;
        }
        let text = fs::read_to_string(&toml_path).unwrap();
        assert!(
            !text.contains("agent24-sidecar-host") || name == "agent24-sidecar-host-protocol",
            "{name} Cargo.toml still names agent24-sidecar-host"
        );
    }
}

#[test]
fn workspace_service_files_have_no_file_wide_dead_code_allow() {
    for rel in [
        "crates/agent24-workspace/src/root.rs",
        "crates/agent24-workspace/src/service.rs",
        "crates/agent24-workspace/src/allocation_root.rs",
    ] {
        let src = read(rel);
        assert!(
            !src.contains("#![allow(dead_code)]"),
            "{rel} still blankets dead_code"
        );
    }
}
