//! COMM-5b: resolved, transitive normal edges, with package-scoped features.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::Command;

use quote::ToTokens;
use serde::Deserialize;

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    resolve: Resolve,
}
#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
}
#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
struct Node {
    id: String,
    deps: Vec<Dependency>,
}
#[derive(Deserialize)]
struct Dependency {
    pkg: String,
    dep_kinds: Vec<DepKind>,
}
#[derive(Deserialize)]
struct DepKind {
    kind: Option<String>,
}

fn cargo(args: &[&str]) -> String {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo available");
    assert!(
        output.status.success(),
        "cargo {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn normal_closure(metadata: &Metadata, root: &str) -> BTreeSet<String> {
    let packages: BTreeMap<_, _> = metadata.packages.iter().map(|p| (&p.id, &p.name)).collect();
    let nodes: BTreeMap<_, _> = metadata.resolve.nodes.iter().map(|n| (&n.id, n)).collect();
    let root = metadata
        .packages
        .iter()
        .find(|p| p.name == root)
        .expect("root package exists");
    let mut queue = VecDeque::from([&root.id]);
    let mut seen = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        for dep in &nodes[id].deps {
            // Cargo reports normal as null; never traverse build/dev edges.
            if dep.dep_kinds.iter().any(|k| k.kind.is_none()) {
                queue.push_back(&dep.pkg);
            }
        }
    }
    seen.into_iter()
        .filter(|id| **id != root.id)
        .map(|id| packages[id].clone())
        .collect()
}

fn violations(names: &BTreeSet<String>, feature_tree: &str) -> BTreeSet<String> {
    let mut errors = BTreeSet::new();
    for name in names {
        if (name.starts_with("agent24-")
            && !["agent24-os-proto", "agent24-domain", "agent24-protocol"].contains(&name.as_str()))
            || ["reqwest", "ureq", "isahc", "surf", "attohttpc"].contains(&name.as_str())
        {
            errors.insert(name.clone());
        }
    }
    // {f} includes implicitly enabled features, unlike searching feature edges.
    // A workspace metadata node's features are unified with agent24d, so use
    // cargo tree's selected-package normal graph for feature activation.
    for row in feature_tree.lines() {
        let (package, features) = row
            .split_once('|')
            .expect("tree format is package|features");
        let name = package.split_whitespace().next().expect("package name");
        if ["hyper", "hyper-util"].contains(&name)
            && features
                .split(',')
                .any(|f| f == "client" || f.starts_with("client-"))
        {
            errors.insert(format!("{name}/client"));
        }
    }
    errors
}

#[test]
fn comm_normal_dependency_closure_stays_within_allowlist_and_control_detects_agent() {
    let metadata: Metadata =
        serde_json::from_str(&cargo(&["metadata", "--locked", "--format-version", "1"])).unwrap();
    let tree = cargo(&[
        "tree",
        "--locked",
        "--offline",
        "--target",
        "all",
        "-p",
        "agent24-comm",
        "-e",
        "normal",
        "--prefix",
        "none",
        "--format",
        "{p}|{f}",
    ]);
    let errors = violations(&normal_closure(&metadata, "agent24-comm"), &tree);
    assert!(
        errors.is_empty(),
        "COMM normal dependency boundary violated: {errors:?}"
    );
    let control = violations(&normal_closure(&metadata, "agent24d"), "");
    assert!(
        control.contains("agent24-agent"),
        "same checker must reject agent24d: {control:?}"
    );
}

#[test]
fn synthetic_transitive_mutations_reject_every_forbidden_package_and_feature() {
    for forbidden in [
        "agent24-agent",
        "agent24-store",
        "agent24-models",
        "reqwest",
        "ureq",
        "isahc",
        "surf",
        "attohttpc",
    ] {
        let json = serde_json::json!({"packages":[{"id":"root","name":"agent24-comm"},{"id":"allowed","name":"agent24-domain"},{"id":"bad","name":forbidden},{"id":"dev","name":"agent24-tools"}],"resolve":{"nodes":[{"id":"root","deps":[{"pkg":"allowed","dep_kinds":[{"kind":null}]},{"pkg":"dev","dep_kinds":[{"kind":"dev"},{"kind":"build"}]}]},{"id":"allowed","deps":[{"pkg":"root","dep_kinds":[{"kind":null}]},{"pkg":"bad","dep_kinds":[{"kind":null}]}]},{"id":"bad","deps":[]},{"id":"dev","deps":[]}]}});
        let metadata: Metadata = serde_json::from_value(json).unwrap();
        let names = normal_closure(&metadata, "agent24-comm");
        assert!(
            !names.contains("agent24-tools"),
            "dev/build edges are excluded"
        );
        assert_eq!(
            violations(&names, ""),
            BTreeSet::from([forbidden.to_owned()])
        );
    }
    for (name, feature) in [
        ("hyper", "client"),
        ("hyper-util", "client"),
        ("hyper-util", "client-legacy"),
    ] {
        assert_eq!(
            violations(
                &BTreeSet::new(),
                &format!("{name} v1.0|http1,{feature},server")
            ),
            BTreeSet::from([format!("{name}/client")])
        );
    }
    assert!(
        violations(
            &BTreeSet::from([
                "agent24-os-proto".into(),
                "agent24-protocol".into(),
                "agent24-domain".into()
            ]),
            "hyper v1.0|http1,server"
        )
        .is_empty()
    );
}

fn state_boundary(source: &str) -> BTreeMap<String, Vec<String>> {
    let file = syn::parse_file(source).expect("valid state source");
    let mut layout = BTreeMap::new();
    let fields = |fields: &syn::Fields| {
        fields
            .iter()
            .map(|f| {
                format!(
                    "{}:{}",
                    f.ident.as_ref().expect("named field"),
                    f.ty.to_token_stream()
                )
            })
            .collect()
    };
    for item in file.items {
        match item {
            syn::Item::Struct(s) if s.ident == "CommState" => {
                layout.insert("CommState".into(), fields(&s.fields));
            }
            syn::Item::Enum(e) if e.ident == "Backend" => {
                for v in e.variants {
                    layout.insert(format!("Backend::{}", v.ident), fields(&v.fields));
                }
            }
            _ => {}
        }
    }
    layout
}

const ALLOWED_STATE: &str = r#"
struct CommState { backend: Backend, daemon: Option<Arc<crate::daemon::HyphaeDaemonSupervisor>> }
enum Backend {
 Ready { runner: Arc<HyphaeRunner>, password_store: Arc<dyn PasswordStore>, home: Arc<PathBuf> },
 Unconfigured { reason: Arc<String> }, BinaryRejected { reason: Arc<String> }
}
"#;

#[test]
fn comm_state_and_backend_hold_only_passive_handles() {
    // Exact AST field/type allowlist also rejects disguised callback fields and
    // execution handles nested inside Backend, which a CommState substring misses.
    assert_eq!(
        state_boundary(include_str!("../src/router.rs")),
        state_boundary(ALLOWED_STATE)
    );
}

#[test]
fn synthetic_state_mutations_reject_execution_handles_aliases_and_callbacks() {
    for ty in [
        "Store",
        "ModelRouter",
        "ToolRegistry",
        "AppState",
        "HiddenAlias",
        "Arc<dyn Fn(String)>",
    ] {
        for anchor in ["backend: Backend", "runner: Arc<HyphaeRunner>"] {
            let mutation = ALLOWED_STATE.replace(anchor, &format!("{anchor}, inbound: {ty}"));
            assert_ne!(
                state_boundary(&mutation),
                state_boundary(ALLOWED_STATE),
                "{ty} in {anchor} must fail"
            );
        }
    }
}
