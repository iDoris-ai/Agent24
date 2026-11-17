//! D1 Office → PDF (ADR-DOC-03 D3/D4): DOCX, XLSX and PPTX documents are
//! stored as they are (no text layer: no engine here reads them) and can be
//! converted to a new PDF document with LibreOffice headless.
//!
//! LibreOffice is not shipped in the package: the desktop downloads a pinned
//! build into this OS's data directory (`engines/libreoffice/<version>/`,
//! readable inside the SBX-1 sandbox), with the Noto CJK fonts next to it,
//! and writes `READY.json` last. Each conversion runs `soffice` in a process
//! of its own with a fresh profile under `engines/libreoffice/profiles/`,
//! whose `user/fonts` links to those fonts and whose
//! `registrymodifications.xcu` holds the font replacement table and turns
//! macros off. Without the bundled fonts the Chinese comes out blank: inside
//! the sandbox LibreOffice sees no system CJK font.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use serde::Deserialize;

use crate::error::ApiError;
use crate::jobs::Job;
use crate::state::AppState;

/// The LibreOffice build the desktop installs (pinned with its sha256 there).
pub const LIBREOFFICE_VERSION: &str = "26.8.3";
/// One conversion, from start to exit.
pub const CONVERT_TIMEOUT: Duration = Duration::from_secs(120);

pub const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
pub const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
pub const PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

/// The replacement table (ADR-DOC-03 D4): the fonts Chinese Office documents
/// name, by the bundled font that stands in for each.
pub const FONT_PAIRS: &[(&str, &str)] = &[
    ("宋体", "Noto Serif SC"),
    ("SimSun", "Noto Serif SC"),
    ("NSimSun", "Noto Serif SC"),
    ("新宋体", "Noto Serif SC"),
    ("仿宋", "Noto Serif SC"),
    ("FangSong", "Noto Serif SC"),
    ("仿宋_GB2312", "Noto Serif SC"),
    ("楷体", "Noto Serif SC"),
    ("KaiTi", "Noto Serif SC"),
    ("楷体_GB2312", "Noto Serif SC"),
    ("Songti SC", "Noto Serif SC"),
    ("STSong", "Noto Serif SC"),
    ("黑体", "Noto Sans SC"),
    ("SimHei", "Noto Sans SC"),
    ("微软雅黑", "Noto Sans SC"),
    ("Microsoft YaHei", "Noto Sans SC"),
    ("等线", "Noto Sans SC"),
    ("DengXian", "Noto Sans SC"),
    ("PingFang SC", "Noto Sans SC"),
    ("STHeiti", "Noto Sans SC"),
];

/// The Office type of a stored file: a ZIP (`PK\x03\x04`) whose upload
/// filename ends in `.docx`, `.xlsx` or `.pptx` (case-insensitive). Anything
/// else is `None`.
#[must_use]
pub fn office_media_type(head: &[u8], filename: Option<&str>) -> Option<&'static str> {
    let _ = (head, filename);
    None
}

/// The file extension LibreOffice reads a media type by.
#[must_use]
pub fn extension(media_type: &str) -> Option<&'static str> {
    let _ = media_type;
    None
}

/// `engines/libreoffice/<LIBREOFFICE_VERSION>` under the OS's data directory.
#[must_use]
pub fn engine_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join("engines")
        .join("libreoffice")
        .join(LIBREOFFICE_VERSION)
}

/// The `soffice` program, when the desktop finished installing it
/// (`READY.json` is there, and so is
/// `LibreOffice.app/Contents/MacOS/soffice`).
#[must_use]
pub fn soffice(data_dir: &Path) -> Option<PathBuf> {
    let _ = data_dir;
    None
}

/// The profile's `registrymodifications.xcu`: [`FONT_PAIRS`] as an always-on
/// replacement table, and macros never run.
#[must_use]
pub fn profile_xcu() -> String {
    String::new()
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ConvertError {
    #[error("the conversion took too long")]
    TooSlow,
    #[error("{0}")]
    Failed(String),
}

/// Converts `input` (named with its extension) to PDF and returns the PDF's
/// path, in `out_dir`. Runs `soffice` with a fresh profile under
/// `<data_dir>/engines/libreoffice/profiles/<random>/` (removed afterwards),
/// kills it at [`CONVERT_TIMEOUT`].
pub fn convert_to_pdf(
    data_dir: &Path,
    soffice: &Path,
    input: &Path,
    out_dir: &Path,
) -> Result<PathBuf, ConvertError> {
    let _ = (data_dir, soffice, input, out_dir);
    Err(ConvertError::Failed("not implemented".into()))
}

/// `DocumentsConvertRequest`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConvertRequest {
    #[serde(default)]
    title: Option<String>,
}

/// `POST /documents/{document_id}/conversions`: the head revision of an
/// Office document as a new PDF document. 202 with the import job.
pub async fn convert(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<ConvertRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let _ = (state, path, body);
    Err(ApiError::internal("not implemented"))
}
