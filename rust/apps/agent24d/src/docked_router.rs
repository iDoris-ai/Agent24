//! Docked router (CORE-5 design D6; 5f-2). Built on its own, NOT the business
//! router with routes switched off: only `GET /api/v1/health` and the three
//! `DiagnosticRead` routes exist. Data comes from values handed in (marker,
//! manifest, cached probes); nothing here opens a database. Not wired into
//! startup (`migration_coordinator::ENABLED=false`; wiring is 5f-3).
//!
//! `DiagnosticRead` is daily-level and needs a live first-party (or host)
//! bearer from the capability store. With no capability store or in legacy
//! single-token mode the routes answer 503 `diagnostics_unavailable`; the
//! legacy bearer never opens them. Creative tokens never reach them. There is
//! no retry/repair route (Q2): `retry_allowed` is always false.

use agent24_protocol::state_file::AuthMode;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::capabilities::{
    Audience, CapabilityError, CapabilityStore, Operation, Resource, RouteDecision, decide,
    unix_now,
};
use crate::migration_guard::dock::{
    DockCode, DockPhase, DockedHealth, FailedDb, MigrationMarker, RollbackState,
};
use crate::migration_guard::manifest::{DbRecord, Manifest};
use agent24_domain::http::error_response;

/// `GET /api/v1/diagnostics/migration`. Scrubbed: no paths, no raw errors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationDiagnostics {
    pub attempt_id: String,
    pub snapshot_id: Option<String>,
    pub failed_version: String,
    pub database: Option<FailedDb>,
    pub migration_version: Option<i64>,
    pub phase: DockPhase,
    pub code: DockCode,
    pub rollback: RollbackState,
    pub retry_allowed: bool,
    pub error_summary: String,
}

impl MigrationDiagnostics {
    #[must_use]
    pub fn from_marker(m: &MigrationMarker) -> Self {
        Self {
            attempt_id: m.attempt_id.clone(),
            snapshot_id: m.snapshot_id.clone(),
            failed_version: m.failed_binary.version.clone(),
            database: m.database,
            migration_version: m.migration_version,
            phase: m.phase,
            code: m.error_code,
            rollback: m.rollback,
            retry_allowed: false,
            error_summary: m.error_summary.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub database: FailedDb,
    pub existed: bool,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
}

/// `GET /api/v1/diagnostics/migration/snapshots`: metadata only, never contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDiagnostics {
    pub snapshot_id: Option<String>,
    pub restore: RollbackState,
    pub files: Vec<SnapshotFile>,
}

impl SnapshotDiagnostics {
    #[must_use]
    pub fn from_manifest(snapshot_id: &str, m: &Manifest, restore: RollbackState) -> Self {
        let file = |database, r: &DbRecord| SnapshotFile {
            database,
            existed: r.existed,
            bytes: r.len,
            sha256: r.sha256.clone(),
        };
        Self {
            snapshot_id: Some(snapshot_id.to_owned()),
            restore,
            files: vec![
                file(FailedDb::Store, &m.store_db),
                file(FailedDb::Memory, &m.memory_db),
            ],
        }
    }
}

/// `GET /api/v1/diagnostics/migration/space`: the last probe, as cached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceDiagnostics {
    pub available_bytes: Option<u64>,
    pub budget_bytes: Option<u64>,
    pub failures: Vec<DockCode>,
}

#[derive(Clone)]
pub struct DockedState {
    pub health: DockedHealth,
    pub migration: MigrationDiagnostics,
    pub snapshots: SnapshotDiagnostics,
    pub space: SpaceDiagnostics,
    /// `None`: no capability authority, diagnostics are unavailable.
    pub capabilities: Option<CapabilityStore>,
    pub auth_mode: AuthMode,
}

pub fn docked_router(state: DockedState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/diagnostics/migration", get(migration))
        .route("/api/v1/diagnostics/migration/snapshots", get(snapshots))
        .route("/api/v1/diagnostics/migration/space", get(space))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state)
}

async fn health(State(s): State<DockedState>) -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, Json(s.health)).into_response()
}

async fn migration(State(s): State<DockedState>) -> Json<MigrationDiagnostics> {
    Json(s.migration)
}

async fn snapshots(State(s): State<DockedState>) -> Json<SnapshotDiagnostics> {
    Json(s.snapshots)
}

async fn space(State(s): State<DockedState>) -> Json<SpaceDiagnostics> {
    Json(s.space)
}

async fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route")
}

/// Route catalog first (same table as the business router), then the
/// `DiagnosticRead` check. Unknown path and wrong method fall through to the
/// router's own 404 / 405; anything else is refused.
async fn gate(State(s): State<DockedState>, req: Request<Body>, next: Next) -> Response {
    let matched = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|m| m.as_str().to_owned());
    match decide(
        req.method(),
        matched.as_deref(),
        req.uri().path(),
        &BTreeSet::new(),
    ) {
        RouteDecision::UnknownPath | RouteDecision::MethodNotAllowed => next.run(req).await,
        RouteDecision::Mapped(p) if p.operation == Operation::HealthRead => next.run(req).await,
        RouteDecision::Mapped(p) if p.operation == Operation::DiagnosticRead => {
            match diagnostic_denied(&s, &req) {
                Some(denied) => denied,
                None => next.run(req).await,
            }
        }
        _ => not_found().await,
    }
}

fn diagnostic_denied(s: &DockedState, req: &Request<Body>) -> Option<Response> {
    let store = match (&s.auth_mode, &s.capabilities) {
        (AuthMode::Capabilities, Some(store)) => store,
        _ => {
            return Some(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "diagnostics_unavailable",
                "Diagnostics are unavailable",
            ));
        }
    };
    let unauthorized = || {
        error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing or invalid bearer token",
        )
    };
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(bearer) = bearer else {
        return Some(unauthorized());
    };
    match store.validate_bearer(
        bearer,
        Operation::DiagnosticRead,
        &Resource::global(),
        unix_now(),
    ) {
        Ok(a) if matches!(a.claims.audience, Audience::CreativeRuntime) => Some(error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Operation not allowed",
        )),
        Ok(_) => None,
        Err(CapabilityError::OperationDenied | CapabilityError::ResourceDenied) => Some(
            error_response(StatusCode::FORBIDDEN, "forbidden", "Operation not allowed"),
        ),
        Err(_) => Some(unauthorized()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::capabilities::CreativeMintRequest;
    use crate::capabilities::first_party::{FirstPartyAuthority, test_peer};
    use crate::enroll_api::EnrollAuthority;
    use crate::migration_guard::dock::{DbMark, DockReason, MigrationsState, PairMark};
    use crate::migration_guard::manifest::BinaryIdentity;
    use agent24_protocol::HealthMigrations;
    use std::time::Duration;
    use tower::ServiceExt;

    const SECRET: &str = "RAW-ERR /Users/jason/.agent24/agent24.db";

    struct Fx {
        router: Router,
        state: DockedState,
        first_party: String,
        host: String,
        creative: String,
        creative_with_diag: String,
    }

    fn marker() -> MigrationMarker {
        let mark = DbMark {
            existed: true,
            version: Some(16),
        };
        MigrationMarker {
            format_version: 1,
            attempt_id: "att-1".into(),
            snapshot_id: Some("snap-1".into()),
            created_at: "2026-11-01T00:00:00Z".into(),
            failed_binary: BinaryIdentity {
                version: "0.7.0".into(),
                sha256: "ab".repeat(32),
            },
            previous_binary: None,
            database: Some(FailedDb::Memory),
            migration_version: Some(17),
            phase: DockPhase::Migrating,
            error_code: DockCode::MigrationFailed,
            error_summary: "migration 17 failed".into(),
            rollback: RollbackState::Restored,
            before_migrations: PairMark {
                store: mark,
                memory: mark,
            },
            observed_migrations: None,
        }
    }

    fn state(mode: AuthMode, caps: Option<CapabilityStore>) -> DockedState {
        let reason = DockReason {
            code: DockCode::MigrationFailed,
            phase: DockPhase::Migrating,
            rollback: RollbackState::Restored,
            retry_allowed: false,
            migrations: MigrationsState::Verified,
        };
        let mut migration = MigrationDiagnostics::from_marker(&marker());
        // Hostile content in the marker must never reach the public health body.
        migration.error_summary = SECRET.into();
        DockedState {
            health: DockedHealth::new(
                reason,
                "0.7.0".into(),
                "rust".into(),
                3,
                HealthMigrations {
                    store: 30,
                    memory: 16,
                },
            ),
            migration,
            snapshots: SnapshotDiagnostics {
                snapshot_id: Some("snap-1".into()),
                restore: RollbackState::Restored,
                files: vec![SnapshotFile {
                    database: FailedDb::Store,
                    existed: true,
                    bytes: Some(4096),
                    sha256: Some("cd".repeat(32)),
                }],
            },
            space: SpaceDiagnostics {
                available_bytes: Some(1 << 30),
                budget_bytes: Some(1 << 20),
                failures: vec![],
            },
            capabilities: caps,
            auth_mode: mode,
        }
    }

    fn fx() -> Fx {
        let store = CapabilityStore::new("g");
        let now = unix_now();
        let host = store
            .mint_product_host("h", Duration::from_secs(600), now)
            .unwrap();
        let (host_bearer, _, _) = host.into_bearer_parts();
        let host_auth = store
            .validate_bearer(
                &host_bearer,
                Operation::ModelsRead,
                &Resource::global(),
                now,
            )
            .unwrap();
        let mint = |ops: Option<Vec<Operation>>| {
            let mut req =
                CreativeMintRequest::new("w", "a", "p", "s", Duration::from_secs(60), now);
            if let Some(ops) = ops {
                req = req.with_allowed_operations(ops);
            }
            store
                .mint_creative_authorized(&host_auth, req)
                .unwrap()
                .into_bearer_parts()
                .0
        };
        let creative = mint(None);
        let creative_with_diag = mint(Some(vec![Operation::DiagnosticRead, Operation::ModelsRead]));
        let first_party = FirstPartyAuthority::new(store.clone())
            .issue(&test_peer("g"))
            .unwrap()
            .secret
            .to_string();
        let state = state(AuthMode::Capabilities, Some(store));
        Fx {
            router: docked_router(state.clone()),
            state,
            first_party,
            host: host_bearer,
            creative,
            creative_with_diag,
        }
    }

    async fn call(
        r: &Router,
        method: &str,
        path: &str,
        bearer: Option<&str>,
    ) -> (StatusCode, String) {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = bearer {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = r
            .clone()
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    fn code(body: &str) -> String {
        serde_json::from_str::<serde_json::Value>(body).unwrap()["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    const DIAG: [&str; 3] = [
        "/api/v1/diagnostics/migration",
        "/api/v1/diagnostics/migration/snapshots",
        "/api/v1/diagnostics/migration/space",
    ];

    /// T4: only the D6 set exists. Even the strongest token reaches no business route.
    #[tokio::test]
    async fn exposes_only_the_d6_routes() {
        let f = fx();
        for (m, p) in [
            ("POST", "/api/v1/chat"),
            ("GET", "/api/v1/events"),
            ("GET", "/api/v1/models"),
            ("GET", "/api/v1/os"),
            ("GET", "/api/v1/attached"),
            ("GET", "/api/v1/schedules"),
            ("POST", "/api/v1/schedules"),
            ("GET", "/api/v1/sessions"),
            ("POST", "/api/v1/runs"),
            ("GET", "/api/v1/memory/assertions"),
            ("GET", "/api/v1/comm/identity"),
            ("GET", "/api/v1/idoris/status"),
            ("POST", "/api/v1/shutdown"),
            ("POST", "/api/v1/capabilities/creative"),
            ("GET", "/api/v1/presence/challenges/next"),
            ("GET", "/api/v1/diagnostics"),
            ("GET", "/api/v1/diagnostics/migration/log"),
            ("POST", "/api/v1/diagnostics/migration/retry"),
            ("POST", "/api/v1/diagnostics/migration/repair"),
            ("GET", "/api/v1/sin90/directions"),
            ("GET", "/"),
        ] {
            for bearer in [None, Some(f.host.as_str()), Some(f.first_party.as_str())] {
                let (status, body) = call(&f.router, m, p, bearer).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{m} {p}");
                assert_eq!(code(&body), "not_found", "{m} {p}");
            }
        }
    }

    /// T4/T14: the 503 body is exactly `DockedHealth`; no path or raw error text.
    #[tokio::test]
    async fn health_is_503_with_the_scrubbed_body() {
        let f = fx();
        let (status, body) = call(&f.router, "GET", "/api/v1/health", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v, serde_json::to_value(&f.state.health).unwrap());
        assert_eq!(v["status"], "migration_failed");
        assert_eq!(v["reason"]["retry_allowed"], false);
        assert_eq!(
            v["migrations"],
            serde_json::json!({"store": 30, "memory": 16})
        );
        let keys: BTreeSet<_> = v["reason"].as_object().unwrap().keys().cloned().collect();
        let want = ["code", "phase", "rollback", "retry_allowed", "migrations"];
        assert_eq!(keys, want.iter().map(|k| (*k).to_owned()).collect());
        for needle in [
            "RAW-ERR",
            "/Users",
            "agent24.db",
            "att-1",
            "snap-1",
            "error_summary",
        ] {
            assert!(!body.contains(needle), "health leaks {needle}: {body}");
        }
    }

    /// Health is public in every auth mode; HEAD inherits it.
    #[tokio::test]
    async fn health_is_public_in_legacy_and_capability_mode() {
        let legacy = docked_router(state(AuthMode::LegacySingleToken, None));
        for r in [legacy, fx().router] {
            let (s, _) = call(&r, "GET", "/api/v1/health", None).await;
            assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
            let (s, b) = call(&r, "HEAD", "/api/v1/health", None).await;
            assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
            assert!(b.is_empty());
        }
    }

    /// Default deny: no token, junk token, creative (even one that asks for the
    /// operation) are refused; first-party and host read all three.
    #[tokio::test]
    async fn diagnostics_default_deny() {
        let f = fx();
        for p in DIAG {
            for (bearer, want) in [
                (None, StatusCode::UNAUTHORIZED),
                (Some("not-a-token"), StatusCode::UNAUTHORIZED),
                (Some(f.creative.as_str()), StatusCode::FORBIDDEN),
                (Some(f.creative_with_diag.as_str()), StatusCode::FORBIDDEN),
                (Some(f.first_party.as_str()), StatusCode::OK),
                (Some(f.host.as_str()), StatusCode::OK),
            ] {
                let (status, body) = call(&f.router, "GET", p, bearer).await;
                assert_eq!(status, want, "{p} {bearer:?}");
                if status != StatusCode::OK {
                    assert!(!body.contains("snap-1") && !body.contains("RAW-ERR"), "{p}");
                }
            }
        }
        let t = Some(f.first_party.as_str());
        let (_, b) = call(&f.router, "GET", DIAG[0], t).await;
        assert_eq!(
            serde_json::from_str::<MigrationDiagnostics>(&b).unwrap(),
            f.state.migration
        );
        let (_, b) = call(&f.router, "GET", DIAG[1], t).await;
        assert_eq!(
            serde_json::from_str::<SnapshotDiagnostics>(&b).unwrap(),
            f.state.snapshots
        );
        let (_, b) = call(&f.router, "GET", DIAG[2], t).await;
        assert_eq!(
            serde_json::from_str::<SpaceDiagnostics>(&b).unwrap(),
            f.state.space
        );
    }

    /// Without a capability authority the routes are `diagnostics_unavailable`;
    /// a legacy bearer (or a capability token under legacy mode) never opens them.
    #[tokio::test]
    async fn diagnostics_unavailable_without_capability_authority() {
        let f = fx();
        let store = f.state.capabilities.clone();
        let cases = [
            docked_router(state(AuthMode::LegacySingleToken, None)),
            docked_router(state(AuthMode::Capabilities, None)),
            docked_router(state(AuthMode::LegacySingleToken, store)),
        ];
        for r in cases {
            for p in DIAG {
                for bearer in [None, Some("legacy-bearer"), Some(f.first_party.as_str())] {
                    let (status, body) = call(&r, "GET", p, bearer).await;
                    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{p}");
                    assert_eq!(code(&body), "diagnostics_unavailable", "{p}");
                }
            }
        }
    }

    #[tokio::test]
    async fn head_inherits_get_and_wrong_methods_are_405() {
        let f = fx();
        for p in DIAG {
            let (s, _) = call(&f.router, "HEAD", p, None).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "HEAD {p} without token");
            let (s, _) = call(&f.router, "HEAD", p, Some(&f.creative)).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "HEAD {p} creative");
            let (s, b) = call(&f.router, "HEAD", p, Some(&f.first_party)).await;
            assert_eq!(s, StatusCode::OK, "HEAD {p}");
            assert!(b.is_empty());
            for m in ["POST", "PUT", "PATCH", "DELETE"] {
                let (s, _) = call(&f.router, m, p, Some(&f.first_party)).await;
                assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "{m} {p}");
            }
        }
        for m in ["POST", "PUT", "DELETE"] {
            let (s, _) = call(&f.router, m, "/api/v1/health", None).await;
            assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "{m} health");
        }
    }

    /// The business router keeps answering 200/ok; docking is a separate router.
    #[tokio::test]
    async fn normal_health_is_unchanged() {
        let app = crate::server::build_router(crate::server::tests::state().await);
        let (s, b) = call(&app, "GET", "/api/v1/health", None).await;
        assert_eq!(s, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(v.get("reason").is_none() && v.get("startup_state").is_none());
        let (s, _) = call(&app, "GET", DIAG[0], None).await;
        assert_ne!(
            s,
            StatusCode::OK,
            "diagnostics must not exist on the business router"
        );
    }

    fn keys_of(v: &serde_yaml::Value) -> BTreeSet<String> {
        v.as_mapping()
            .unwrap()
            .keys()
            .map(|k| k.as_str().unwrap().to_owned())
            .collect()
    }

    fn strings(v: &serde_yaml::Value) -> BTreeSet<String> {
        v.as_sequence()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect()
    }

    fn json_keys(v: &serde_json::Value) -> BTreeSet<String> {
        v.as_object().unwrap().keys().cloned().collect()
    }

    fn variants<T: Serialize>(items: &[T]) -> BTreeSet<String> {
        items
            .iter()
            .map(|i| {
                serde_json::to_value(i)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    /// T14: Rust types, their serialized shape and OpenAPI agree (keys, required, enums).
    #[test]
    fn rust_types_match_openapi() {
        let doc: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../../protocol/openapi.yaml")).unwrap();
        let schemas = &doc["components"]["schemas"];
        let s = state(AuthMode::Capabilities, None);
        let health = serde_json::to_value(&s.health).unwrap();
        let hs = &schemas["Health"];
        assert!(json_keys(&health).is_subset(&keys_of(&hs["properties"])));
        for r in strings(&hs["required"]) {
            assert!(health.get(&r).is_some(), "{r} missing from docked health");
        }
        assert_eq!(
            strings(&hs["properties"]["status"]["enum"]),
            BTreeSet::from(["ok".to_owned(), "migration_failed".to_owned()])
        );
        let files = &schemas["SnapshotDiagnostics"]["properties"]["files"]["items"];
        for (sc, value) in [
            (&schemas["HealthReason"], health["reason"].clone()),
            (
                &schemas["MigrationDiagnostics"],
                serde_json::to_value(&s.migration).unwrap(),
            ),
            (
                &schemas["SnapshotDiagnostics"],
                serde_json::to_value(&s.snapshots).unwrap(),
            ),
            (
                &schemas["SpaceDiagnostics"],
                serde_json::to_value(&s.space).unwrap(),
            ),
            (files, serde_json::to_value(&s.snapshots.files[0]).unwrap()),
        ] {
            assert_eq!(keys_of(&sc["properties"]), json_keys(&value));
            assert_eq!(strings(&sc["required"]), json_keys(&value));
        }
        let all_codes = [
            DockCode::Interrupted,
            DockCode::MigrationFailed,
            DockCode::RollbackFailed,
            DockCode::GuardInvalid,
            DockCode::PersistFailed,
            DockCode::MarkerInvalid,
            DockCode::SpaceInsufficient,
            DockCode::SpaceBudgetUnknown,
        ];
        assert_eq!(variants(&all_codes), strings(&schemas["DockCode"]["enum"]));
        let phases = [
            DockPhase::Preflight,
            DockPhase::Migrating,
            DockPhase::FtsRebuild,
            DockPhase::Restoring,
        ];
        assert_eq!(variants(&phases), strings(&schemas["DockPhase"]["enum"]));
        let rollbacks = [
            RollbackState::NotNeeded,
            RollbackState::InProgress,
            RollbackState::Restored,
            RollbackState::Failed,
        ];
        assert_eq!(
            variants(&rollbacks),
            strings(&schemas["RollbackState"]["enum"])
        );
        assert_eq!(
            variants(&[FailedDb::Store, FailedDb::Memory]),
            strings(&schemas["DockDatabase"]["enum"])
        );
        assert_eq!(
            variants(&[MigrationsState::Verified, MigrationsState::Unknown]),
            strings(&schemas["HealthReason"]["properties"]["migrations"]["enum"])
        );
    }
}
