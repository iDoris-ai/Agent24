//! Documenting domain OS (`documents`), ADR-DOC-01 / ADR-DOC-02.
//!
//! This crate is the out-of-process module the kernel spawns from
//! `domain-os.yml`. The router built here is nested under
//! `/api/v1/documents` by `agent24_os_sdk::Module::serve`, so every path
//! below is relative to that namespace.
//!
//! Slice 1 lands in small PRs. Until storage and engines exist, the only
//! route is `GET /capabilities`, and it reports every operation as
//! unavailable with a typed reason (ADR-DOC-02 §8).

pub mod blob;
pub mod db;
pub mod error;
pub mod id;

use axum::{Json, Router, routing::get};
use serde::Serialize;

/// The manifest the kernel digests; `main` hands these exact bytes to the SDK.
pub const MANIFEST_YAML: &str = include_str!("../domain-os.yml");

/// `GET /capabilities` body (ADR-DOC-02 §8).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Capabilities {
    pub os_version: &'static str,
    pub storage: StorageStatus,
    pub engines: Vec<EngineStatus>,
    pub knowledge: KnowledgeStatus,
    pub operations: Vec<OperationStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StorageStatus {
    /// `ready | degraded | unavailable`
    pub state: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EngineStatus {
    pub id: &'static str,
    /// `parse | ocr | render | export`
    pub kind: &'static str,
    pub formats: Vec<&'static str>,
    /// `absent | installing | ready | failed`
    pub state: &'static str,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct KnowledgeStatus {
    /// `on | off` — README §9: on by default.
    pub setting: &'static str,
    /// README §9.2 states. DOC-1 has no knowledge-required operation, so this
    /// is reported, never used to block.
    pub state: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OperationStatus {
    pub op: &'static str,
    pub available: bool,
    /// A code from the closed set in ADR-DOC-02 §6; `None` when available.
    pub reason: Option<&'static str>,
}

/// Slice-1 operations (ADR-DOC-02 §4) and what each one waits on today.
const SLICE1_OPERATIONS: &[(&str, &str)] = &[
    ("upload", "storage_unavailable"),
    ("import", "storage_unavailable"),
    ("get", "storage_unavailable"),
    ("list", "storage_unavailable"),
    ("job", "storage_unavailable"),
    ("render", "engine_unavailable"),
    ("read_range", "engine_unavailable"),
    ("find", "engine_unavailable"),
    ("extract", "engine_unavailable"),
];

/// What this build can do right now. Later slice-1 PRs replace the fixed
/// states with live checks of storage and engines.
#[must_use]
pub fn capabilities() -> Capabilities {
    Capabilities {
        os_version: env!("CARGO_PKG_VERSION"),
        storage: StorageStatus {
            state: "unavailable",
        },
        engines: Vec::new(),
        knowledge: KnowledgeStatus {
            setting: "on",
            state: "unavailable",
        },
        operations: SLICE1_OPERATIONS
            .iter()
            .map(|&(op, reason)| OperationStatus {
                op,
                available: false,
                reason: Some(reason),
            })
            .collect(),
    }
}

async fn get_capabilities() -> Json<Capabilities> {
    Json(capabilities())
}

/// The module's HTTP surface, relative to `/api/v1/documents`.
pub fn router() -> Router {
    Router::new().route("/capabilities", get(get_capabilities))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_domain::{Capability, DomainOsManifest, ImplKind, ModelAccess};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// The daemon validates `domain-os.yml` with this exact function before it
    /// mounts a package; if it rejects ours, the OS never starts.
    #[test]
    fn manifest_passes_the_kernel_validator_with_the_adr_identity() {
        let m =
            DomainOsManifest::from_yaml(MANIFEST_YAML).expect("kernel must accept the manifest");
        assert_eq!(m.name(), "documents");
        // The kernel registers this version; /capabilities reports Cargo's.
        assert_eq!(m.version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(m.route_namespace(), "/api/v1/documents");
        // `from_yaml` also refuses an event_module or data_dir that differs from
        // the name-derived value (agent24-domain/src/lib.rs, the checks on
        // `raw.event_module != raw.name` and `raw.data_dir != expected_dir`).
        assert_eq!(m.event_module(), "documents");
        assert_eq!(m.impl_kind(), ImplKind::OutOfProcessProvider);
        assert_eq!(m.model_access(), ModelAccess::LocalOnly);
        assert_eq!(
            m.kernel_capabilities(),
            &[Capability::Events, Capability::Models]
        );
        let spawn = m.spawn().expect("out-of-process module needs spawn");
        assert_eq!(spawn.command, "bin/agent24-documents");
    }

    #[test]
    fn manifest_requests_no_memory_capability() {
        // ADR-DOC-02 §2.1 / README M1: the OS never writes user memory.
        let m = DomainOsManifest::from_yaml(MANIFEST_YAML).unwrap();
        assert!(!m.kernel_capabilities().contains(&Capability::Memory));
    }

    /// Written out independently of `SLICE1_OPERATIONS`, so dropping, renaming
    /// or duplicating an operation in the production table fails this test.
    const EXPECTED: &[(&str, &str)] = &[
        ("extract", "engine_unavailable"),
        ("find", "engine_unavailable"),
        ("get", "storage_unavailable"),
        ("import", "storage_unavailable"),
        ("job", "storage_unavailable"),
        ("list", "storage_unavailable"),
        ("read_range", "engine_unavailable"),
        ("render", "engine_unavailable"),
        ("upload", "storage_unavailable"),
    ];

    #[tokio::test]
    async fn capabilities_route_reports_every_slice1_operation_with_a_typed_reason() {
        let res = router()
            .oneshot(Request::get("/capabilities").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["os_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["storage"]["state"], "unavailable");
        assert_eq!(body["engines"], serde_json::json!([]));
        assert_eq!(body["knowledge"]["setting"], "on");
        assert_eq!(body["knowledge"]["state"], "unavailable");

        let mut got: Vec<(String, String)> = body["operations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|op| {
                assert_eq!(op["available"], false, "nothing is available yet: {op}");
                (
                    op["op"].as_str().unwrap().to_owned(),
                    op["reason"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let reported = got.len();
        got.sort();
        got.dedup_by(|a, b| a.0 == b.0);
        assert_eq!(got.len(), reported, "an operation is listed twice");
        let expected: Vec<(String, String)> = EXPECTED
            .iter()
            .map(|&(op, reason)| (op.to_owned(), reason.to_owned()))
            .collect();
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn unknown_paths_are_404_not_a_catch_all() {
        let res = router()
            .oneshot(Request::get("/documents").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
