//! Documenting domain OS (`documents`), ADR-DOC-01 / ADR-DOC-02.
//!
//! This crate is the out-of-process module the kernel spawns from
//! `domain-os.yml`. The router built here is nested under
//! `/api/v1/documents` by `agent24_os_sdk::Module::serve`, so every path
//! below is relative to that namespace.
//!
//! Slice 1 lands in small PRs. `GET /capabilities` reports storage as it
//! is now, and every operation without a route yet as
//! unavailable with a typed reason (ADR-DOC-02 §8).

pub mod blob;
pub mod db;
pub mod error;
pub mod id;
pub mod idem;
pub mod state;
pub mod timestamp;
pub mod uploads;

use axum::extract::{DefaultBodyLimit, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::state::AppState;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Needs {
    Storage,
    Engine,
}

/// Slice-1 operations (ADR-DOC-02 §4): what each needs, and whether this
/// build has its route yet. An operation is available only when both hold;
/// until its route lands it reports the code of what it needs, so a client
/// never sees `available: true` for a 404.
const SLICE1_OPERATIONS: &[(&str, Needs, bool)] = &[
    ("upload", Needs::Storage, true),
    ("import", Needs::Storage, false),
    ("get", Needs::Storage, false),
    ("list", Needs::Storage, false),
    ("job", Needs::Storage, false),
    ("render", Needs::Engine, false),
    ("read_range", Needs::Engine, false),
    ("find", Needs::Engine, false),
    ("extract", Needs::Engine, false),
];

/// What this build can do right now. Engines are not wired yet, so engine
/// operations are always unavailable.
#[must_use]
pub async fn capabilities(state: &AppState) -> Capabilities {
    // Asking also retries storage that failed to open (see `state`).
    let storage_ready = state.storage().await.is_ok();
    Capabilities {
        os_version: env!("CARGO_PKG_VERSION"),
        storage: StorageStatus {
            state: if storage_ready {
                "ready"
            } else {
                "unavailable"
            },
        },
        engines: Vec::new(),
        knowledge: KnowledgeStatus {
            setting: "on",
            state: "unavailable",
        },
        operations: SLICE1_OPERATIONS
            .iter()
            .map(|&(op, needs, routed)| {
                let (ready, code) = match needs {
                    Needs::Storage => (storage_ready, "storage_unavailable"),
                    Needs::Engine => (false, "engine_unavailable"),
                };
                let available = routed && ready;
                OperationStatus {
                    op,
                    available,
                    reason: (!available).then_some(code),
                }
            })
            .collect(),
    }
}

async fn get_capabilities(State(state): State<AppState>) -> Json<Capabilities> {
    Json(capabilities(&state).await)
}

/// The module's HTTP surface, relative to `/api/v1/documents`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/capabilities", get(get_capabilities))
        .route("/uploads", post(uploads::create_upload))
        .route(
            "/uploads/{upload_id}/chunks",
            post(uploads::chunks::append_chunk)
                .layer(DefaultBodyLimit::max(uploads::chunks::MAX_CHUNK)),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::error::StorageCause;
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

    async fn get_capabilities_json(state: AppState) -> serde_json::Value {
        let res = router(state)
            .oneshot(Request::get("/capabilities").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Every operation, once: those in `available` available with no
    /// reason, the rest unavailable with the expected reason.
    fn assert_operations(body: &serde_json::Value, available: &[&str]) {
        let mut got: Vec<(String, Option<String>)> = body["operations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|op| {
                let name = op["op"].as_str().unwrap().to_owned();
                let on = available.contains(&name.as_str());
                assert_eq!(op["available"], on, "{op}");
                (name, op["reason"].as_str().map(str::to_owned))
            })
            .collect();
        let reported = got.len();
        got.sort();
        got.dedup_by(|a, b| a.0 == b.0);
        assert_eq!(got.len(), reported, "an operation is listed twice");
        let expected: Vec<(String, Option<String>)> = EXPECTED
            .iter()
            .map(|&(op, reason)| {
                let reason = (!available.contains(&op)).then(|| reason.to_owned());
                (op.to_owned(), reason)
            })
            .collect();
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn capabilities_report_storage_that_failed_to_open_as_unavailable() {
        let body = get_capabilities_json(AppState::unavailable(StorageCause::Corrupt)).await;
        assert_eq!(body["os_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            body["storage"],
            serde_json::json!({ "state": "unavailable" })
        );
        assert_eq!(body["engines"], serde_json::json!([]));
        assert_eq!(body["knowledge"]["setting"], "on");
        assert_eq!(body["knowledge"]["state"], "unavailable");
        assert_operations(&body, &[]);
    }

    #[tokio::test]
    async fn capabilities_report_opened_storage_as_ready() {
        let dir = tempfile::tempdir().unwrap();
        let body = get_capabilities_json(AppState::open(dir.path()).await).await;
        assert_eq!(body["storage"], serde_json::json!({ "state": "ready" }));
        // Ready storage makes only the operations with routes available.
        assert_operations(&body, &["upload"]);
    }

    #[tokio::test]
    async fn capabilities_report_storage_once_it_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let first = AppState::open(dir.path()).await;
        let second = AppState::open_with(dir.path(), std::time::Duration::ZERO).await;
        let body = get_capabilities_json(second.clone()).await;
        assert_eq!(body["storage"]["state"], "unavailable");
        drop(first);
        let body = get_capabilities_json(second).await;
        assert_eq!(body["storage"]["state"], "ready");
    }

    #[tokio::test]
    async fn unknown_paths_are_404_not_a_catch_all() {
        let res = router(AppState::unavailable(StorageCause::Busy))
            .oneshot(Request::get("/documents").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
