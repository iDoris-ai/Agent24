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
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use serde::Deserialize;

use crate::error::ApiError;
use crate::id::{IdKind, is_id, new_id};
use crate::jobs::Job;
use crate::state::{AppState, blob_cause, io_cause};

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
    if !head.starts_with(b"PK\x03\x04") {
        return None;
    }
    let name = filename?;
    if ends_with_ignore_ascii_case(name, ".docx") {
        Some(DOCX)
    } else if ends_with_ignore_ascii_case(name, ".xlsx") {
        Some(XLSX)
    } else if ends_with_ignore_ascii_case(name, ".pptx") {
        Some(PPTX)
    } else {
        None
    }
}

fn ends_with_ignore_ascii_case(name: &str, suffix: &str) -> bool {
    let name = name.as_bytes();
    let suffix = suffix.as_bytes();
    name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// The file extension LibreOffice reads a media type by.
#[must_use]
pub fn extension(media_type: &str) -> Option<&'static str> {
    match media_type {
        DOCX => Some("docx"),
        XLSX => Some("xlsx"),
        PPTX => Some("pptx"),
        _ => None,
    }
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
    let dir = engine_dir(data_dir);
    let program = dir.join("LibreOffice.app/Contents/MacOS/soffice");
    if dir.join("READY.json").exists() && program.is_file() {
        Some(program)
    } else {
        None
    }
}

/// Escapes `&`, `<`, `>` and `"` for a LibreOffice registry value.
fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// The profile's `registrymodifications.xcu`: [`FONT_PAIRS`] as an always-on
/// replacement table, and macros never run.
#[must_use]
pub fn profile_xcu() -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<item oor:path="/org.openoffice.Office.Common/Font/Substitution"><prop oor:name="Replacement" oor:op="fuse"><value>true</value></prop></item>
"#,
    );
    for (i, (replace, substitute)) in FONT_PAIRS.iter().enumerate() {
        xml.push_str(&format!(
            "<item oor:path=\"/org.openoffice.Office.Common/Font/Substitution/FontPairs\"><node oor:name=\"_{i}\" oor:op=\"replace\"><prop oor:name=\"Always\" oor:op=\"fuse\"><value>true</value></prop><prop oor:name=\"OnScreenOnly\" oor:op=\"fuse\"><value>false</value></prop><prop oor:name=\"ReplaceFont\" oor:op=\"fuse\"><value>{}</value></prop><prop oor:name=\"SubstituteFont\" oor:op=\"fuse\"><value>{}</value></prop></node></item>\n",
            xml_escape(replace),
            xml_escape(substitute),
        ));
    }
    xml.push_str(
        r#"<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop></item>
</oor:items>
"#,
    );
    xml
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ConvertError {
    #[error("the conversion took too long")]
    TooSlow,
    #[error("{0}")]
    Failed(String),
}

pub(crate) fn random_hex() -> Result<String, ConvertError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| ConvertError::Failed(format!("no randomness: {e}")))?;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        hex.push(char::from(b"0123456789abcdef"[(byte & 0xf) as usize]));
    }
    Ok(hex)
}

/// Removes the temporary profile however the conversion ends.
#[cfg(unix)]
struct RemoveOnDrop(PathBuf);

#[cfg(unix)]
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    #[cfg(not(unix))]
    {
        let _ = (data_dir, soffice, input, out_dir);
        return Err(ConvertError::Failed(
            "converting to PDF needs a unix font symlink".into(),
        ));
    }
    #[cfg(unix)]
    {
        let profile_root = data_dir
            .join("engines")
            .join("libreoffice")
            .join("profiles")
            .join(random_hex()?);
        let user = profile_root.join("user");
        if let Err(e) = std::fs::create_dir_all(&user) {
            let _ = std::fs::remove_dir_all(&profile_root);
            return Err(ConvertError::Failed(format!(
                "creating the profile failed: {e}"
            )));
        }
        let _remove = RemoveOnDrop(profile_root.clone());
        std::fs::write(user.join("registrymodifications.xcu"), profile_xcu())
            .map_err(|e| ConvertError::Failed(format!("writing the profile failed: {e}")))?;
        std::os::unix::fs::symlink(engine_dir(data_dir).join("fonts"), user.join("fonts"))
            .map_err(|e| ConvertError::Failed(format!("linking fonts failed: {e}")))?;
        let absolute = std::path::absolute(&profile_root)
            .map_err(|e| ConvertError::Failed(format!("the profile path: {e}")))?;
        let mut child = Command::new(soffice)
            .arg("--headless")
            .arg("--norestore")
            .arg("--nolockcheck")
            .arg("--nodefault")
            .arg("--nofirststartwizard")
            .arg(format!(
                "-env:UserInstallation=file://{}",
                absolute.display()
            ))
            .arg("--convert-to")
            .arg("pdf")
            .arg("--outdir")
            .arg(out_dir)
            .arg(input)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ConvertError::Failed(format!("starting soffice failed: {e}")))?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() > CONVERT_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ConvertError::TooSlow);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ConvertError::Failed(format!(
                        "waiting for soffice failed: {e}"
                    )));
                }
            }
        };
        if !status.success() {
            return Err(ConvertError::Failed(format!(
                "soffice exited with {status}"
            )));
        }
        let Some(stem) = input.file_stem() else {
            return Err(ConvertError::Failed("the input has no file name".into()));
        };
        let mut pdf_name = stem.to_os_string();
        pdf_name.push(".pdf");
        let pdf = out_dir.join(pdf_name);
        if !pdf.is_file() {
            return Err(ConvertError::Failed(format!(
                "soffice wrote no pdf at {}",
                pdf.display()
            )));
        }
        Ok(pdf)
    }
}

/// `DocumentsConvertRequest`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConvertRequest {
    #[serde(default)]
    title: Option<String>,
}

/// One conversion at a time.
static CONVERTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn convert_failure(error: ConvertError) -> ApiError {
    match error {
        // Not `engine_unavailable`, which is 「not installed」 to every client
        // (PR-Daemon #1909 ③): the converter is there, this file was too much for it.
        ConvertError::TooSlow => ApiError::unprocessable(
            "parse_failed",
            format!(
                "the conversion took longer than {} s: the file may be too large or complex",
                CONVERT_TIMEOUT.as_secs()
            ),
        ),
        ConvertError::Failed(message) => ApiError::unprocessable("parse_failed", message),
    }
}

fn pdf_word_failure(error: crate::pdf_word::PdfWordError) -> ApiError {
    match error {
        crate::pdf_word::PdfWordError::Scanned => ApiError::unprocessable(
            "unsupported_format",
            crate::pdf_word::PdfWordError::Scanned.to_string(),
        ),
        crate::pdf_word::PdfWordError::TooSlow => ApiError::unprocessable(
            "parse_failed",
            format!(
                "the conversion took longer than {} s: the file may be too large or complex",
                crate::pdf_word::CONVERT_TIMEOUT.as_secs()
            ),
        ),
        crate::pdf_word::PdfWordError::Failed(message) => {
            ApiError::unprocessable("parse_failed", message)
        }
    }
}

fn io_failure(error: &std::io::Error) -> ApiError {
    ApiError::storage_unavailable(io_cause(error))
}

/// The stored title plus `.pdf`, when this request did not name one.
async fn default_title(storage: &crate::state::Storage, id: &str) -> Option<String> {
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM documents WHERE id = ?")
        .bind(id)
        .fetch_optional(storage.db.pool())
        .await
        .ok()
        .flatten();
    // `合同.docx` becomes `合同.pdf`.
    title.map(|title| {
        let stem = [".docx", ".xlsx", ".pptx"]
            .iter()
            .find_map(|ext| {
                let cut = title.len().checked_sub(ext.len())?;
                (title.is_char_boundary(cut) && title[cut..].eq_ignore_ascii_case(ext))
                    .then(|| title[..cut].to_owned())
            })
            .unwrap_or_else(|| title.clone());
        format!("{stem}.pdf")
    })
}

/// The stored title as `.docx`, when this request did not name one.
async fn default_docx_title(storage: &crate::state::Storage, id: &str) -> Option<String> {
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM documents WHERE id = ?")
        .bind(id)
        .fetch_optional(storage.db.pool())
        .await
        .ok()
        .flatten();
    title.map(|title| crate::pdf_word::docx_title(&title))
}

/// `POST /documents/{document_id}/conversions`: the head revision of an
/// Office document as a new PDF document. 202 with the import job.
pub async fn convert(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<ConvertRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    if !is_id(IdKind::Document, &id) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    if req
        .title
        .as_ref()
        .is_some_and(|title| !(1..=500).contains(&title.chars().count()))
    {
        return Err(ApiError::invalid_request(
            "title must be 1 to 500 characters",
        ));
    }
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    let head: Option<i64> =
        match sqlx::query_scalar("SELECT head_revision FROM documents WHERE id = ?")
            .bind(&id)
            .fetch_optional(storage.db.pool())
            .await
        {
            Ok(head) => head,
            Err(e) => return Err(storage.db_failure(e).await),
        };
    let head = head.ok_or_else(|| ApiError::not_found("no such document"))?;
    let revision = crate::read::revision(&storage, &id, head).await?;
    if revision.media_type == crate::pdf_word::PDF {
        let Some(program) = crate::pdf_word::program(storage.data_dir()) else {
            return Err(ApiError::engine_unavailable(
                "pdf2docx",
                "the PDF to Word converter is not installed",
            ));
        };
        let source = storage
            .blobs
            .path_of(&revision.content_sha256)
            .map_err(|e| ApiError::storage_unavailable(blob_cause(&e)))?;
        let title = match req.title {
            Some(title) => Some(title),
            None => default_docx_title(&storage, &id).await,
        };
        // PR-Daemon #1909 ③: the conversion can take far longer than the kernel
        // waits for an answer (10 s to the head), so the answer is a `convert`
        // job, running; it ends when the new document is imported, its result
        // that document. A restart ends it `interrupted` (`jobs::recover`).
        let job_id =
            new_id(IdKind::Job).map_err(|_| ApiError::internal("could not generate a job id"))?;
        let inserted = sqlx::query(
            "INSERT INTO jobs (id, kind, document_id, revision, status, origin)
             VALUES (?, 'convert', ?, ?, 'running', json_object('kind', 'page'))",
        )
        .bind(&job_id)
        .bind(&id)
        .bind(head)
        .execute(storage.db.pool())
        .await;
        if let Err(e) = inserted {
            return Err(storage.db_failure(e).await);
        }
        let job = match crate::jobs::load(storage.db.pool(), &job_id).await {
            Ok(Some(job)) => job,
            Ok(None) => return Err(ApiError::internal("the convert job was not written")),
            Err(e) => return Err(storage.db_failure(e).await),
        };
        let data_dir = storage.data_dir().to_path_buf();
        let owned = storage.clone();
        let watched = storage.clone();
        let this_job = job_id.clone();
        let runtime = tokio::runtime::Handle::current();
        let make = move |out: &Path| {
            let _held = match CONVERTING.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            // Cancelled while it waited for the one conversion before it: not
            // started at all (PR-Daemon #1915); `finish_convert` ends it cancelled.
            let status: Option<String> = runtime
                .block_on(
                    sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
                        .bind(&this_job)
                        .fetch_optional(watched.db.pool()),
                )
                .ok()
                .flatten();
            if status.as_deref() == Some("cancelling") {
                return Err(ApiError::invalid_request("the conversion was cancelled"));
            }
            crate::pdf_word::convert_to_docx(&data_dir, &program, &source, out)
                .map_err(pdf_word_failure)
        };
        let made = async move { crate::derived::import_derived(&owned, title, "docx", make).await };
        tokio::spawn(finish_convert(storage, job_id, made));
        return Ok((StatusCode::ACCEPTED, Json(job)));
    }
    let Some(ext) = extension(&revision.media_type) else {
        return Err(ApiError::unprocessable(
            "unsupported_format",
            "only DOCX, XLSX and PPTX documents convert to PDF, and PDF documents to Word",
        ));
    };
    let Some(soffice_bin) = soffice(storage.data_dir()) else {
        return Err(ApiError::engine_unavailable(
            "libreoffice",
            "the Office converter is not installed",
        ));
    };
    let source = storage
        .blobs
        .path_of(&revision.content_sha256)
        .map_err(|e| ApiError::storage_unavailable(blob_cause(&e)))?;
    let title = match req.title {
        Some(title) => Some(title),
        None => default_title(&storage, &id).await,
    };
    // PR-Daemon #1909 ③: the conversion can take far longer than the kernel
    // waits for an answer (10 s to the head), so the answer is a `convert`
    // job, running; it ends when the new PDF is imported, its result that
    // document. A restart ends it `interrupted` (`jobs::recover`).
    let job_id =
        new_id(IdKind::Job).map_err(|_| ApiError::internal("could not generate a job id"))?;
    let inserted = sqlx::query(
        "INSERT INTO jobs (id, kind, document_id, revision, status, origin)
         VALUES (?, 'convert', ?, ?, 'running', json_object('kind', 'page'))",
    )
    .bind(&job_id)
    .bind(&id)
    .bind(head)
    .execute(storage.db.pool())
    .await;
    if let Err(e) = inserted {
        return Err(storage.db_failure(e).await);
    }
    let job = match crate::jobs::load(storage.db.pool(), &job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return Err(ApiError::internal("the convert job was not written")),
        Err(e) => return Err(storage.db_failure(e).await),
    };
    let data_dir = storage.data_dir().to_path_buf();
    let owned = storage.clone();
    let watched = storage.clone();
    let this_job = job_id.clone();
    let runtime = tokio::runtime::Handle::current();
    let make = move |out: &Path| {
        let _held = match CONVERTING.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Cancelled while it waited for the one conversion before it: not
        // started at all (PR-Daemon #1915); `finish_convert` ends it cancelled.
        let status: Option<String> = runtime
            .block_on(
                sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
                    .bind(&this_job)
                    .fetch_optional(watched.db.pool()),
            )
            .ok()
            .flatten();
        if status.as_deref() == Some("cancelling") {
            return Err(ApiError::invalid_request("the conversion was cancelled"));
        }
        let work = data_dir
            .join("engines")
            .join("libreoffice")
            .join("work")
            .join(random_hex().map_err(convert_failure)?);
        if let Err(e) = std::fs::create_dir_all(&work) {
            return Err(io_failure(&e));
        }
        let result = (|| -> Result<(), ApiError> {
            let input = work.join(format!("input.{ext}"));
            std::fs::copy(&source, &input).map_err(|e| io_failure(&e))?;
            let pdf =
                convert_to_pdf(&data_dir, &soffice_bin, &input, &work).map_err(convert_failure)?;
            if std::fs::rename(&pdf, out).is_err() {
                std::fs::copy(&pdf, out).map_err(|e| io_failure(&e))?;
            }
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&work);
        result
    };
    let made = async move { crate::derived::import_derived(&owned, title, "pdf", make).await };
    tokio::spawn(finish_convert(storage, job_id, made));
    Ok((StatusCode::ACCEPTED, Json(job)))
}

/// Runs the conversion and its import, then ends the `convert` job as the
/// import ended: succeeded with the new document as its result, or failed
/// with the conversion's or the import's error.
async fn finish_convert(
    storage: std::sync::Arc<crate::state::Storage>,
    job_id: String,
    made: impl std::future::Future<Output = Result<(StatusCode, Json<Job>), ApiError>> + Send,
) {
    let ended = match made.await {
        Err(e) => Err((e.code().to_owned(), e.message().to_owned())),
        Ok((_, Json(import))) => wait_import(&storage, &import.job_id).await,
    };
    let (status, error, result) = match ended {
        Ok(document) => ("succeeded", None, Some(document)),
        Err((code, message)) => (
            "failed",
            Some(serde_json::json!({ "code": code, "message": message }).to_string()),
            None,
        ),
    };
    let written = sqlx::query(&format!(
        // A job cancelled while it ran (`cancelling`) ends `cancelled`, not
        // left there (PR-Daemon #1915): the conversion is not stopped midway,
        // so a PDF it made may still be in the library, but the job says cancelled.
        "UPDATE jobs SET updated_at = {},
                status = CASE status WHEN 'cancelling' THEN 'cancelled' ELSE ?1 END,
                error = CASE status WHEN 'cancelling' THEN NULL ELSE ?2 END,
                result_ref = CASE status WHEN 'cancelling' THEN NULL ELSE ?3 END
         WHERE id = ?4 AND status IN ('running', 'cancelling')",
        crate::jobs::NOW
    ))
    .bind(status)
    .bind(error)
    .bind(result)
    .bind(&job_id)
    .execute(storage.db.pool())
    .await;
    if let Err(e) = written {
        tracing::warn!(job = %job_id, error = %e, "documents: the convert job could not be ended");
    }
}

/// The import job's end: its document, or its error. Waits at most ten minutes.
async fn wait_import(
    storage: &crate::state::Storage,
    import: &str,
) -> Result<String, (String, String)> {
    for _ in 0..1200 {
        match crate::jobs::load(storage.db.pool(), import).await {
            Ok(Some(job)) => match job.status.as_str() {
                "succeeded" => {
                    return job.document_id.ok_or_else(|| {
                        (
                            "storage_unavailable".to_owned(),
                            "the import named no document".to_owned(),
                        )
                    });
                }
                "failed" | "cancelled" | "interrupted" => {
                    let code = job
                        .error
                        .as_ref()
                        .and_then(|e| e.get("code"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|c| crate::error::CODES.contains(c))
                        .unwrap_or("storage_unavailable")
                        .to_owned();
                    let message = job
                        .error
                        .as_ref()
                        .and_then(|e| e.get("message"))
                        .and_then(serde_json::Value::as_str)
                        .map_or_else(|| format!("the import ended {}", job.status), str::to_owned);
                    return Err((code, message));
                }
                _ => {}
            },
            Ok(None) => {
                return Err((
                    "storage_unavailable".into(),
                    "the import job is gone".into(),
                ));
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err((
        "storage_unavailable".into(),
        "the import did not end in time".into(),
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            for i in 0..1000u32 {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir()
                    .join(format!("a24-office-{}-{nanos}-{i}", std::process::id()));
                if std::fs::create_dir(&path).is_ok() {
                    return Self(path);
                }
            }
            panic!("could not create a unique temp directory");
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn office_type_follows_the_zip_head_and_the_extension() {
        let zip = &b"PK\x03\x04rest"[..];
        assert_eq!(office_media_type(zip, Some("合同.DOCX")), Some(DOCX));
        assert_eq!(office_media_type(zip, Some("a.txt")), None);
        assert_eq!(office_media_type(b"%PDF-1.7", Some("a.docx")), None);
        assert_eq!(office_media_type(zip, None), None);
        for (name, media, ext) in [
            ("a.docx", DOCX, "docx"),
            ("b.xlsx", XLSX, "xlsx"),
            ("c.pptx", PPTX, "pptx"),
        ] {
            let got = office_media_type(zip, Some(name)).unwrap();
            assert_eq!(got, media);
            assert_eq!(extension(got), Some(ext));
        }
        assert_eq!(extension("application/pdf"), None);
    }

    #[test]
    fn profile_xcu_replaces_every_font_and_disables_macros() {
        let xcu = profile_xcu();
        assert!(xcu.contains("<oor:items"));
        assert!(xcu.trim_end().ends_with("</oor:items>"));
        assert!(xcu.contains("DisableMacrosExecution"));
        for (i, (replace, substitute)) in FONT_PAIRS.iter().enumerate() {
            assert!(xcu.contains(&format!("oor:name=\"_{i}\"")));
            assert!(xcu.contains(&format!(
                "<prop oor:name=\"ReplaceFont\" oor:op=\"fuse\"><value>{}</value></prop>",
                xml_escape(replace)
            )));
            assert!(xcu.contains(&format!(
                "<prop oor:name=\"SubstituteFont\" oor:op=\"fuse\"><value>{}</value></prop>",
                xml_escape(substitute)
            )));
        }
        assert_eq!(xml_escape("A&B<C>D\"E"), "A&amp;B&lt;C&gt;D&quot;E");
    }

    /// PR-Daemon #1909 ③: the request answers at once with a running
    /// `convert` job, however long the converter takes; a converter that
    /// fails ends the job failed, never as 「not installed」.
    #[tokio::test]
    async fn a_conversion_answers_at_once_with_a_job_that_ends_later() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(dir.path()).await;
        let storage = state.storage().await.unwrap();
        let data = storage.data_dir().to_path_buf();
        let program = engine_dir(&data).join("LibreOffice.app/Contents/MacOS/soffice");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        executable(&program, "#!/bin/sh\nsleep 2\nexit 3\n");
        std::fs::write(engine_dir(&data).join("READY.json"), b"{}").unwrap();
        let doc = "doc_01K74Z3QJ8V5N2W9RTX6Y00001";
        let sha = storage
            .blobs
            .put_bytes(b"PK not really a docx")
            .unwrap()
            .sha256;
        sqlx::raw_sql(&format!(
            "INSERT INTO documents (id, title, media_type, head_revision, created_at, updated_at)
                    VALUES ('{doc}', 'a.docx', '{DOCX}', 1, '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z');
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{doc}', 1, '{sha}', 1, '{DOCX}', 'import');"
        ))
        .execute(storage.db.pool())
        .await
        .unwrap();

        let started = Instant::now();
        let res = crate::router(state.clone())
            .oneshot(
                Request::post(format!("/documents/{doc}/conversions"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "answered after {:?}",
            started.elapsed()
        );
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let job: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            (job["kind"].as_str(), job["status"].as_str()),
            (Some("convert"), Some("running"))
        );
        let id = job["job_id"].as_str().unwrap().to_owned();
        let mut ended = None;
        for _ in 0..100 {
            let job = crate::jobs::load(storage.db.pool(), &id)
                .await
                .unwrap()
                .unwrap();
            if job.status != "running" {
                ended = Some(job);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let ended = ended.expect("the convert job never ended");
        assert_eq!(ended.status, "failed");
        assert_ne!(ended.error.unwrap()["code"], "engine_unavailable");

        // Cancelled while it ran: it ends cancelled, not stuck cancelling (PR-Daemon #1915).
        let res = crate::router(state.clone())
            .oneshot(
                Request::post(format!("/documents/{doc}/conversions"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let job: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = job["job_id"].as_str().unwrap().to_owned();
        sqlx::query("UPDATE jobs SET status = 'cancelling' WHERE id = ?")
            .bind(&id)
            .execute(storage.db.pool())
            .await
            .unwrap();
        let mut last = String::new();
        for _ in 0..100 {
            last = crate::jobs::load(storage.db.pool(), &id)
                .await
                .unwrap()
                .unwrap()
                .status;
            if last != "cancelling" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(last, "cancelled");
    }

    #[test]
    fn soffice_is_present_only_after_install() {
        let scratch = Scratch::new();
        assert_eq!(soffice(scratch.path()), None);
        let program = engine_dir(scratch.path()).join("LibreOffice.app/Contents/MacOS/soffice");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"").unwrap();
        std::fs::write(engine_dir(scratch.path()).join("READY.json"), b"{}").unwrap();
        assert_eq!(soffice(scratch.path()), Some(program));
    }

    fn executable(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn profile_entries(data_dir: &Path) -> usize {
        let profiles = data_dir.join("engines/libreoffice/profiles");
        std::fs::read_dir(profiles)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0)
    }

    #[test]
    fn a_program_that_writes_the_pdf_is_its_result_and_the_profile_is_removed() {
        let scratch = Scratch::new();
        let data = scratch.path().join("data");
        let out = scratch.path().join("out");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        let input = scratch.path().join("input.docx");
        std::fs::write(&input, b"PK\x03\x04").unwrap();
        let program = scratch.path().join("soffice");
        executable(
            &program,
            r#"#!/bin/sh
out=""
input=""
while [ $# -gt 0 ]; do
  case "$1" in
    --outdir)
      out="$2"
      shift 2
      ;;
    *)
      input="$1"
      shift
      ;;
  esac
done
base=$(basename "$input")
stem=${base%.*}
printf '%s\n' '%PDF-1.4' > "$out/$stem.pdf"
exit 0
"#,
        );
        let pdf = convert_to_pdf(&data, &program, &input, &out).unwrap();
        assert_eq!(pdf, out.join("input.pdf"));
        assert!(pdf.is_file());
        assert_eq!(profile_entries(&data), 0);
    }

    #[test]
    fn a_program_that_exits_with_an_error_fails_the_conversion() {
        let scratch = Scratch::new();
        let data = scratch.path().join("data");
        let out = scratch.path().join("out");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        let input = scratch.path().join("input.docx");
        std::fs::write(&input, b"PK\x03\x04").unwrap();
        let program = scratch.path().join("soffice");
        executable(&program, "#!/bin/sh\nexit 1\n");
        let error = convert_to_pdf(&data, &program, &input, &out).unwrap_err();
        assert!(matches!(error, ConvertError::Failed(_)), "{error:?}");
        assert_eq!(profile_entries(&data), 0);
    }
}
