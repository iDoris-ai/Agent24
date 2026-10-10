//! `GET /documents/{id}/revisions/{rev}/pages/{page}` (ADR-DOC-02 §4;
//! `documentsRenderPage`): a page, or a region of it, as a PNG of at most
//! 1 MiB, at the largest scale up to the one asked for that fits.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::revision;
use crate::engine::{MAX_SCALE, MIN_SCALE, RenderAsk, RenderError, WAIT, pdfkit};
use crate::error::ApiError;
use crate::state::{AppState, blob_cause};

/// The PNG, at most: what the kernel proxy passes on (§4).
pub const MAX_PNG: usize = 1 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderParams {
    scale: Option<f64>,
    region: Option<String>,
}

/// `x0,y0,x1,y1`: four plain decimals, `x0 < x1` and `y0 < y1`.
fn region(s: &str) -> Option<[f64; 4]> {
    let digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
    let plain = |f: &str| match f.split_once('.') {
        Some((whole, part)) => digits(whole) && digits(part),
        None => digits(f),
    };
    let fields: Vec<&str> = s.split(',').collect();
    if fields.len() != 4 || !fields.iter().all(|f| plain(f)) {
        return None;
    }
    let v: Vec<f64> = fields
        .iter()
        .map(|f| f.parse().ok())
        .collect::<Option<_>>()?;
    let corners = [v[0], v[1], v[2], v[3]];
    (corners.iter().all(|c| c.is_finite()) && v[0] < v[2] && v[1] < v[3]).then_some(corners)
}

const ENGINE: &str = pdfkit::ENGINE_ID;

fn too_long() -> ApiError {
    ApiError::engine_unavailable(ENGINE, "the render took too long")
}

pub async fn render_page(
    State(state): State<AppState>,
    path: Result<UrlPath<(String, i64, i64)>, PathRejection>,
    params: Result<Query<RenderParams>, QueryRejection>,
) -> Result<Response, ApiError> {
    // One deadline for all of it, from here, storage (which may be reopened)
    // included: inside the kernel proxy's 10 s for a response's head, so a
    // client hears 503 rather than 504.
    let deadline = tokio::time::Instant::now() + WAIT;
    let UrlPath((id, rev, page)) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Query(p) = params.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let scale = p.scale.unwrap_or(1.0);
    let region = match p.region.as_deref() {
        None => None,
        Some(r) => Some(region(r).ok_or_else(|| {
            ApiError::invalid_request("region is x0,y0,x1,y1 with x0 < x1 and y0 < y1")
        })?),
    };
    if page < 1 || !(MIN_SCALE..=MAX_SCALE).contains(&scale) {
        return Err(ApiError::invalid_request(
            "page is 1 or more; scale is 0.25 to 4",
        ));
    }
    let render = async {
        let storage = state
            .storage()
            .await
            .map_err(ApiError::storage_unavailable)?;
        let r = revision(&storage, &id, rev).await?;
        if !pdfkit::FORMATS.contains(&r.media_type.as_str()) {
            return Err(ApiError::unprocessable(
                "unsupported_format",
                "slice 1 renders PDF, JPEG and PNG",
            ));
        }
        // A page past any the file could have is past its last.
        let page = u32::try_from(page).map_err(|_| ApiError::not_found("no such page"))?;
        let layers = state.layers();
        let renderer = layers
            .engine()
            .ok_or_else(|| {
                ApiError::engine_unavailable(ENGINE, "no render engine on this platform")
            })?
            .clone();
        let file = storage
            .blobs
            .path_of(&r.content_sha256)
            .map_err(|e| ApiError::storage_unavailable(blob_cause(&e)))?;
        let _slot = layers
            .render_slot(deadline)
            .await
            .ok_or_else(|| ApiError::engine_unavailable(ENGINE, "the render engine is busy"))?;
        let ask = RenderAsk {
            page,
            scale,
            region,
            max_bytes: MAX_PNG,
        };
        renderer
            .render(&r.media_type, &file, ask)
            .await
            .map_err(|e| match e {
                RenderError::Unsupported => ApiError::unprocessable(
                    "unsupported_format",
                    "the render engine does not read this file",
                ),
                RenderError::NoPage => ApiError::not_found("no such page"),
                RenderError::OffPage => ApiError::invalid_request("the region is off the page"),
                RenderError::TooLarge => ApiError::render_too_large(),
                RenderError::TooSlow => too_long(),
                e @ RenderError::Failed(_) => {
                    ApiError::unprocessable("parse_failed", e.to_string())
                }
            })
    };
    // Given up at the deadline, the render is dropped and its helper killed.
    let rendered = tokio::time::timeout_at(deadline, render)
        .await
        .map_err(|_| too_long())??;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png".to_owned()),
            (
                header::HeaderName::from_static("documents-render-scale"),
                rendered.scale.to_string(),
            ),
        ],
        rendered.png,
    )
        .into_response())
}

#[cfg(test)]
mod tests;
