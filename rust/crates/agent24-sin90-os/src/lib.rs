//! Sin90 as a DOMAIN OS — the first [`DomainModule`] (ME-1b-b; ADR-029).
//!
//! Before this, Sin90 lived inside the daemon: `AppState.sin90` held its store,
//! seven `/api/v1/sin90/*` routes were spelled out in `build_router`, and its
//! handlers reached into the kernel crate for the error envelope. The kernel knew
//! Sin90 by name, which meant "swappable domain OS" was a description of an
//! intention rather than of the code.
//!
//! Now the kernel's ROUTING, state and mounter know only [`agent24_domain`]. Its
//! composition root (`serve`) still names this crate, because someone has to say
//! which OS is installed — that is a wiring decision, not the coupling ADR-029
//! objects to. This crate owns:
//! - its **identity** — the embedded `domain-os.yml`, validated at construction,
//!   from which the kernel derives the namespace, the event module and the data
//!   directory;
//! - its **store** — its own `sin90.db` inside the directory the kernel assigns,
//!   never the kernel's `agent24.db`;
//! - its **routes** — relative (`/directions`, not `/api/v1/sin90/directions`),
//!   so the module cannot mount outside its namespace even if it tried;
//! - its **events** — through the [`KernelCtx`] sink, which stamps `sin90` itself.
//!
//! What it deliberately does NOT own: authentication (the kernel's layer wraps
//! the mounted routes), the 503 its namespace serves when its store will not open
//! (the kernel serves that — a module with no store is the last thing that should
//! be answering), and the error-envelope shape (shared, in
//! [`agent24_domain::http`], so a client cannot tell kernel from module).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use agent24_domain::http::{error_response, module_unavailable, read_body_or_response};
use agent24_domain::{DomainError, DomainModule, DomainOsManifest, KernelCtx, Result};
use agent24_sin90::{ScheduleBlockStatus, Sin90Proposal};
use agent24_sin90_store::{Sin90Store, StoreError};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use serde::Deserialize;

/// The manifest, compiled in. A domain OS that shipped its identity as a loose
/// file could have it edited to point at another module's routes or data; here
/// the only `domain-os.yml` that can describe this code is the one built with it.
const MANIFEST: &str = include_str!("../domain-os.yml");

/// This module's identity, available WITHOUT constructing it.
///
/// The kernel builds its catalogue of installed OSes before it decides what to
/// construct, because a module whose constructor fails must still have a name for
/// `agent24 os disable` to act on — that is the case where a user most needs it.
/// (A constructor that PANICS is a different matter: nothing in-process contains
/// that.) Constants rather than a parse: the catalogue must not be able to fail
/// either.
///
/// `identity_matches_the_manifest` keeps them honest.
pub const MANIFEST_NAME: &str = "sin90";
pub const MANIFEST_VERSION: &str = "0.2.1";

/// Where this module's data lives — a CONSTRUCTOR choice, not a trait parameter.
///
/// [`DomainModule::open_store`] takes only a `&Path`, and widening it with a mode
/// flag would push every module's storage policy into the shared contract. An
/// ephemeral daemon instead builds the module in [`StorageMode::Memory`], which
/// legitimately ignores the assigned path — exactly the case the contract's
/// `open_store` doc calls out.
#[derive(Debug, Clone)]
pub enum StorageMode {
    /// `sin90.db` inside the kernel-assigned directory. `legacy`, when set, is a
    /// pre-ME-1b database (`~/.agent24/sin90.db`) to migrate from on first start;
    /// see [`Sin90Store::open_migrating_from`] for why that is one SQLite
    /// snapshot rather than three file moves.
    Persistent { legacy: Option<PathBuf> },
    /// In-memory, for `--ephemeral` daemons and tests.
    Memory,
}

/// Sin90, packaged as a domain OS.
pub struct Sin90Module {
    manifest: DomainOsManifest,
    mode: StorageMode,
    /// Set exactly once by [`DomainModule::open_store`]. `OnceLock` rather than a
    /// mutex because it is written once at mount and read by every handler after;
    /// if it is still empty when [`DomainModule::routes`] runs, the module serves
    /// its own unavailable response instead of unwrapping — the mounter should
    /// never call routes after a failed open, but a defensive 503 beats a panic
    /// that takes the daemon down.
    store: OnceLock<Sin90Store>,
}

impl Sin90Module {
    /// Build the module. Fails only if the compiled-in manifest is invalid, which
    /// would be a build-time mistake in this crate, not a runtime condition.
    pub fn new(mode: StorageMode) -> Result<Self> {
        Ok(Self {
            manifest: DomainOsManifest::from_yaml(MANIFEST)?,
            mode,
            store: OnceLock::new(),
        })
    }

    fn store(&self) -> Option<&Sin90Store> {
        self.store.get()
    }
}

#[async_trait::async_trait]
impl DomainModule for Sin90Module {
    fn manifest(&self) -> &DomainOsManifest {
        &self.manifest
    }

    async fn open_store(&self, dir: &Path) -> Result<()> {
        let store = match &self.mode {
            StorageMode::Memory => Sin90Store::open_memory().await,
            StorageMode::Persistent { legacy } => {
                let path = dir.join("sin90.db");
                match legacy {
                    Some(l) => Sin90Store::open_migrating_from(&path, l).await,
                    None => Sin90Store::open(&path).await,
                }
            }
        }
        .map_err(|e| DomainError::Store(e.to_string()))?;

        self.store
            .set(store)
            .map_err(|_| DomainError::Store("open_store called more than once".into()))
    }

    fn routes(&self, ctx: Arc<dyn KernelCtx>) -> axum::Router {
        let Some(store) = self.store() else {
            // Unreachable through the mounter, which only asks a module for routes
            // after open_store succeeded — but the trait cannot promise that, and
            // a panic here would kill the whole daemon over one module.
            return axum::Router::new().fallback(|| async { module_unavailable("sin90") });
        };
        let state = Sin90State {
            store: store.clone(),
            ctx,
        };
        axum::Router::new()
            .route("/directions", post(create_direction).get(list_directions))
            .route("/schedule-blocks", post(create_block).get(list_blocks))
            .route("/schedule-blocks/{id}", patch(transition_block))
            .route("/proposals", post(submit_proposal).get(list_proposals))
            .route("/proposals/{id}", get(get_proposal))
            .route("/proposals/{id}/accept", post(accept_proposal))
            .route("/attention", get(attention))
            .with_state(state)
    }
}

/// Handler state: the module's OWN store plus the kernel context. Nothing from
/// `AppState` — that is the point of the move.
#[derive(Clone)]
struct Sin90State {
    store: Sin90Store,
    ctx: Arc<dyn KernelCtx>,
}

impl Sin90State {
    /// Emit `sin90.<kind>`. The sink stamps the module name from the manifest, so
    /// there is no parameter here through which an event could be misattributed;
    /// `None` means the kernel did not grant events, which is a degraded-but-fine
    /// state, not an error to fail a mutation over.
    fn emit(&self, kind: &str, payload: serde_json::Value) {
        let Some(sink) = self.ctx.events() else {
            return;
        };
        // The envelope requires an object. A non-object here would be a bug in
        // THIS file (every call site below passes `json!({...})`), so it fails LOUD
        // in debug and drops the event in release — never the old kernel-side
        // helper's silent coercion to `{}`, which turned a mistake into missing
        // data nobody could see. Debug-only because a malformed event must not take
        // down a user's daemon over a payload.
        let serde_json::Value::Object(map) = payload else {
            debug_assert!(false, "sin90 event payload must be an object");
            return;
        };
        if let Err(e) = sink.emit(kind, map) {
            debug_assert!(false, "sin90 emitted an invalid event kind: {e}");
        }
    }
}

/// Map a store error to an HTTP response, mirroring the kernel's envelope shape.
/// A FOREIGN KEY violation is a client mistake (referenced a nonexistent entity)
/// → 404, not the 500 a raw sqlx error would otherwise become.
fn map_err(err: StoreError) -> Response {
    if err.is_fk_violation() {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "a referenced entity does not exist",
        );
    }
    match err {
        StoreError::NotFound(m) => error_response(StatusCode::NOT_FOUND, "not_found", &m),
        StoreError::Transition(e) => {
            error_response(StatusCode::CONFLICT, "conflict", &e.to_string())
        }
        // The accept has no body — a validation failure means the STORED ops are
        // stale against current state, a state conflict, not a malformed request.
        StoreError::Proposal(e) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unprocessable",
            &e.to_string(),
        ),
        StoreError::Conflict(m) => error_response(StatusCode::CONFLICT, "conflict", &m),
        StoreError::WeekNotOpen(m) => error_response(
            StatusCode::CONFLICT,
            "conflict",
            &format!("task {m}'s week is not open"),
        ),
        StoreError::SameWeekCarry(m) => error_response(
            StatusCode::CONFLICT,
            "conflict",
            &format!("cannot carry task {m} into its own week"),
        ),
        StoreError::Internal(_)
        | StoreError::Sqlx(_)
        | StoreError::Migrate(_)
        | StoreError::Serde(_) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", "store error")
        }
    }
}

// Err is the shared v1-envelope Response; the lint only fires because this
// helper is sync, not because it is avoidable.
#[allow(clippy::result_large_err)]
fn parse<T: for<'de> Deserialize<'de>>(
    bytes: &Bytes,
    what: &str,
) -> std::result::Result<T, Response> {
    serde_json::from_slice(bytes).map_err(|e| {
        error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("invalid {what}: {e}"),
        )
    })
}

/// A fixed-width `YYYY-MM-DDThh:mm:ssZ` (20 chars) — the exact shape
/// `agent24_core::util::now_iso8601` stamps events with, so a lexical window
/// compare is chronological. Deliberately not a full RFC3339 parser: we only
/// need to reject shapes (like a bare date) that would compare wrong.
fn is_fixed_iso8601(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || c.is_ascii_digit())
}

// ---- request/query bodies (deny_unknown_fields: reject model typos loudly) --

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewDirectionReq {
    title: String,
    target_window: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewBlockReq {
    #[serde(default)]
    direction_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    planned_minutes: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionReq {
    to: ScheduleBlockStatus,
}

/// Half-open window `[start, end)` — ISO-8601 bounds, fixed-width so a lexical
/// compare is chronological.
#[derive(Deserialize)]
struct AttentionQuery {
    start: String,
    end: String,
}

// ---- handlers -------------------------------------------------------------

async fn create_direction(State(state): State<Sin90State>, req: Request<Body>) -> Response {
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let body: NewDirectionReq = match parse(&bytes, "direction") {
        Ok(b) => b,
        Err(r) => return r,
    };
    match state
        .store
        .create_direction(&body.title, &body.target_window)
        .await
    {
        Ok(d) => {
            state.emit(
                "direction.created",
                serde_json::json!({ "id": d.id, "title": d.title }),
            );
            (StatusCode::CREATED, Json(d)).into_response()
        }
        Err(e) => map_err(e),
    }
}

async fn create_block(State(state): State<Sin90State>, req: Request<Body>) -> Response {
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let body: NewBlockReq = match parse(&bytes, "block") {
        Ok(b) => b,
        Err(r) => return r,
    };
    match state
        .store
        .create_block(
            body.direction_id.as_deref(),
            body.task_id.as_deref(),
            body.planned_minutes,
        )
        .await
    {
        Ok(b) => {
            // Emit the created half too, else `block.transitioned` later carries a
            // block id the stream never announced.
            state.emit(
                "block.created",
                serde_json::json!({ "block_id": b.id, "direction_id": b.direction_id }),
            );
            (StatusCode::CREATED, Json(b)).into_response()
        }
        Err(e) => map_err(e),
    }
}

async fn transition_block(
    State(state): State<Sin90State>,
    AxPath(id): AxPath<String>,
    req: Request<Body>,
) -> Response {
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let body: TransitionReq = match parse(&bytes, "transition") {
        Ok(b) => b,
        Err(r) => return r,
    };
    match state.store.transition_block(&id, body.to).await {
        Ok(b) => {
            state.emit(
                "block.transitioned",
                serde_json::json!({ "block_id": b.id, "to": b.status }),
            );
            Json(b).into_response()
        }
        Err(e) => map_err(e),
    }
}

async fn submit_proposal(State(state): State<Sin90State>, req: Request<Body>) -> Response {
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let proposal: Sin90Proposal = match parse(&bytes, "proposal") {
        Ok(b) => b,
        Err(r) => return r,
    };
    match state.store.submit_proposal(&proposal).await {
        Ok(()) => {
            state.emit(
                "proposal.submitted",
                serde_json::json!({ "id": proposal.id }),
            );
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "id": proposal.id, "status": "pending" })),
            )
                .into_response()
        }
        Err(e) => map_err(e),
    }
}

async fn accept_proposal(State(state): State<Sin90State>, AxPath(id): AxPath<String>) -> Response {
    match state.store.apply_proposal(&id).await {
        Ok(outcome) => {
            // Only broadcast when this call actually applied — a retried accept
            // that merely replays the stored receipt must NOT re-emit.
            if outcome.applied_now {
                state.emit(
                    "proposal.applied",
                    serde_json::json!({ "proposal_id": outcome.receipt.proposal_id }),
                );
            }
            Json(outcome.receipt).into_response()
        }
        Err(e) => map_err(e),
    }
}

// ---- reads (list + detail) ------------------------------------------------
// Plain "what exists now" projections, newest first. The mutation stream emits
// created/transitioned events carrying ids; these GETs are the reconcile side.

async fn list_directions(State(state): State<Sin90State>) -> Response {
    match state.store.list_directions().await {
        Ok(v) => Json(serde_json::json!({ "directions": v })).into_response(),
        Err(e) => map_err(e),
    }
}

async fn list_blocks(State(state): State<Sin90State>) -> Response {
    match state.store.list_blocks().await {
        Ok(v) => Json(serde_json::json!({ "blocks": v })).into_response(),
        Err(e) => map_err(e),
    }
}

async fn list_proposals(State(state): State<Sin90State>) -> Response {
    match state.store.list_proposals().await {
        Ok(v) => Json(serde_json::json!({ "proposals": v })).into_response(),
        Err(e) => map_err(e),
    }
}

async fn get_proposal(State(state): State<Sin90State>, AxPath(id): AxPath<String>) -> Response {
    match state.store.get_proposal(&id).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => map_err(e),
    }
}

async fn attention(
    State(state): State<Sin90State>,
    q: std::result::Result<Query<AttentionQuery>, QueryRejection>,
) -> Response {
    // Keep the v1 error envelope for a bad query string (axum's default rejection
    // is plain text).
    let Query(q) = match q {
        Ok(q) => q,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid query: {e}"),
            );
        }
    };
    // Both bounds must be the fixed-width ISO-8601 that events stamp `at` with
    // (`YYYY-MM-DDThh:mm:ssZ`). A date-only `2026-08-11` compares lexically BELOW
    // `2026-08-11T00:00:00Z`, so it would silently drop that whole day — worse
    // than a 400. Reject anything that isn't the exact 20-char shape.
    if !is_fixed_iso8601(&q.start) || !is_fixed_iso8601(&q.end) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "start and end must be fixed-width ISO-8601 (YYYY-MM-DDThh:mm:ssZ)",
        );
    }
    // start >= end would silently return an empty report indistinguishable from
    // "did nothing" — reject it rather than lie.
    if q.start >= q.end {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "start must be strictly before end",
        );
    }
    match state.store.attention(&q.start, &q.end).await {
        Ok(rows) => Json(serde_json::json!({ "attention": rows })).into_response(),
        Err(e) => map_err(e),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn identity_matches_the_manifest() {
        // The kernel's catalogue uses the CONSTANTS (so it cannot fail), while
        // everything downstream uses the parsed MANIFEST. If they ever disagreed,
        // `agent24 os enable sin90` would write an entry for a name the mounter
        // never sees — and nothing else would notice.
        let m = DomainOsManifest::from_yaml(MANIFEST).expect("the compiled-in manifest is valid");
        assert_eq!(m.name(), MANIFEST_NAME);
        assert_eq!(m.version(), MANIFEST_VERSION);
    }

    #[test]
    fn a_module_that_never_opened_its_store_serves_unavailable_rather_than_panicking() {
        // The mounter never asks a module for routes after a failed open, but the
        // trait cannot promise that, and a panic here would take the daemon down
        // over one module.
        struct NoCtx;
        impl KernelCtx for NoCtx {
            fn events(&self) -> Option<&agent24_domain::EventSink> {
                None
            }
        }
        let m = Sin90Module::new(StorageMode::Memory).unwrap();
        // `routes` is called WITHOUT `open_store` having run.
        let _router = m.routes(Arc::new(NoCtx));
    }
}
