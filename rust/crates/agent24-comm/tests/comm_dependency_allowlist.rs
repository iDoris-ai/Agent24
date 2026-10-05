//! COMM-5b structural boundary: walk Cargo's *resolved normal dependency*
//! graph (including transitive dependencies), not just Cargo.toml's direct
//! list. This prevents comm from acquiring a path back into the run/model
//! stack or an HTTP client that could call agent24d over loopback.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    resolve: Resolve,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: String,
    #[serde(default)]
    deps: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    pkg: String,
    #[serde(default)]
    dep_kinds: Vec<DepKind>,
}

#[derive(Debug, Deserialize)]
struct DepKind {
    kind: Option<String>,
}

fn normal_closure(root_name: &str) -> BTreeSet<String> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata must be available to COMM-5b tests");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Metadata =
        serde_json::from_slice(&output.stdout).expect("cargo metadata must produce valid JSON");
    let packages: BTreeMap<_, _> = metadata
        .packages
        .iter()
        .map(|package| (package.id.clone(), package.name.clone()))
        .collect();
    let nodes: BTreeMap<_, _> = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect();
    let root = metadata
        .packages
        .iter()
        .find(|package| package.name == root_name)
        .unwrap_or_else(|| panic!("package {root_name} is missing from cargo metadata"));
    let mut queue = VecDeque::from([root.id.clone()]);
    let mut visited = BTreeSet::new();

    while let Some(id) = queue.pop_front() {
        if !visited.insert(id.to_owned()) {
            continue;
        }
        let node = nodes
            .get(&id)
            .unwrap_or_else(|| panic!("resolved node for {} is missing", packages[&id]));
        for dependency in &node.deps {
            // An empty dep_kinds list is treated as normal for compatibility
            // with older cargo metadata; otherwise only kind=null is normal.
            let is_normal = dependency.dep_kinds.is_empty()
                || dependency.dep_kinds.iter().any(|kind| kind.kind.is_none());
            if is_normal {
                queue.push_back(dependency.pkg.clone());
            }
        }
    }

    visited.iter().map(|id| packages[id].clone()).collect()
}

fn assert_comm_boundary(root: &str) -> BTreeSet<String> {
    let names = normal_closure(root);
    let allowed_agent24 = ["agent24-os-proto", "agent24-domain", "agent24-protocol"];
    let unexpected: Vec<_> = names
        .iter()
        .filter(|name| {
            name.starts_with("agent24-")
                && name.as_str() != root
                && !allowed_agent24.contains(&name.as_str())
        })
        .cloned()
        .collect();
    assert!(
        unexpected.is_empty(),
        "{root} normal dependency graph crossed the COMM allowlist: {unexpected:?}"
    );

    let forbidden = ["reqwest", "ureq", "isahc", "surf", "attohttpc"];
    let clients: Vec<_> = names
        .iter()
        .filter(|name| forbidden.contains(&name.as_str()))
        .cloned()
        .collect();
    assert!(clients.is_empty(), "{root} pulls HTTP clients: {clients:?}");

    // cargo metadata's resolved feature list is workspace-unified, so an
    // unrelated binary target can activate hyper/client globally. Ask
    // cargo tree with this selected root package to scope feature activation
    // to the comm graph while retaining metadata for the complete graph walk.
    let tree = Command::new("cargo")
        .args(["tree", "-p", root, "-e", "normal,features"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree must be available to COMM-5b tests");
    assert!(
        tree.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&tree.stderr)
    );
    let tree = String::from_utf8_lossy(&tree.stdout);
    for forbidden_feature in [
        "hyper feature \"client\"",
        "hyper-util feature \"client\"",
        "hyper-util feature \"client-legacy\"",
    ] {
        assert!(
            !tree.contains(forbidden_feature),
            "{root} enables HTTP client feature {forbidden_feature}"
        );
    }
    names
}

#[test]
fn comm_normal_dependency_closure_stays_within_allowlist() {
    assert_comm_boundary("agent24-comm");
}

#[test]
fn agent24d_is_a_positive_control_for_the_run_stack() {
    let names = normal_closure("agent24d");
    assert!(
        names.contains("agent24-agent"),
        "positive control failed: agent24d graph must contain agent24-agent"
    );
}

#[test]
fn comm_state_does_not_hold_execution_or_persistence_handles() {
    let source = include_str!("../src/router.rs");
    let state_start = source
        .find("pub struct CommState {")
        .expect("CommState definition exists");
    let state_fields = source[state_start..]
        .split_once("\n}")
        .expect("CommState definition closes")
        .0;
    for forbidden in ["Store", "ModelRouter", "ToolRegistry", "AppState"] {
        assert!(
            !state_fields.contains(forbidden),
            "CommState must not contain {forbidden}"
        );
    }
}
