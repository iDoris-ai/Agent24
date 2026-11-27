//! L12-2 PDF → Word: a PDF document converted to a new DOCX document with
//! the a24-pdf2docx engine (pdf2docx + our fixes, `tools/engines/pdf2docx/`).
//!
//! The engine is not shipped in the package: the desktop downloads a pinned
//! build into this OS's data directory (`engines/pdf2docx/<version>/`, inside
//! the SBX-1 sandbox's read and write roots) and writes `READY.json` last.
//! Each conversion runs the engine in a process of its own, in a scratch
//! directory under `engines/pdf2docx/work/` (removed afterwards), with
//! `TMPDIR` there, and kills it at [`CONVERT_TIMEOUT`].
//!
//! It is the same route as Office → PDF (`POST /documents/{id}/conversions`,
//! [`crate::office::convert`]): a PDF source takes this path. The engine's
//! contract (`tools/engines/pdf2docx/README.md`): `convert <in.pdf> <out.docx>`,
//! the last stdout line JSON, exit 0 written, 3 a scan (no text layer: refused,
//! `unsupported_format`, no new code in the closed set), anything else failed
//! (`parse_failed`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The engine build the desktop installs (pinned with its sha256 there).
pub const PDF2DOCX_VERSION: &str = "0.5.13-a24.1";
/// One conversion, from start to exit.
pub const CONVERT_TIMEOUT: Duration = Duration::from_secs(300);
pub const PDF: &str = "application/pdf";

/// `engines/pdf2docx/<PDF2DOCX_VERSION>` under the OS's data directory.
#[must_use]
pub fn engine_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join("engines")
        .join("pdf2docx")
        .join(PDF2DOCX_VERSION)
}

/// The engine program, when the desktop finished installing it
/// (`READY.json` is there, and so is `a24-pdf2docx/a24-pdf2docx`).
#[must_use]
pub fn program(data_dir: &Path) -> Option<PathBuf> {
    let dir = engine_dir(data_dir);
    let program = dir.join("a24-pdf2docx/a24-pdf2docx");
    if dir.join("READY.json").is_file() && program.is_file() {
        Some(program)
    } else {
        None
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PdfWordError {
    /// Exit 3: every page is without text (a scan). Not retried.
    #[error("the PDF has no text layer (a scanned document): it cannot be converted to Word")]
    Scanned,
    #[error("the conversion took too long")]
    TooSlow,
    #[error("{0}")]
    Failed(String),
}

fn random_hex() -> Result<String, PdfWordError> {
    crate::office::random_hex().map_err(|error| PdfWordError::Failed(error.to_string()))
}

/// Removes the scratch directory however the conversion ends.
#[cfg(unix)]
struct RemoveOnDrop(PathBuf);

#[cfg(unix)]
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Converts `input` (a PDF) to DOCX at `out` with the engine `program`.
/// Runs it in a scratch directory under `<data_dir>/engines/pdf2docx/work/`
/// (`TMPDIR` points there; removed however it ends), stdin/stdout/stderr not
/// inherited, killed at [`CONVERT_TIMEOUT`]. Exit 0 with no file at `out` is
/// a failure.
pub fn convert_to_docx(
    data_dir: &Path,
    program: &Path,
    input: &Path,
    out: &Path,
) -> Result<(), PdfWordError> {
    #[cfg(not(unix))]
    {
        let _ = (data_dir, program, input, out);
        return Err(PdfWordError::Failed(
            "converting to Word needs a unix process".into(),
        ));
    }
    #[cfg(unix)]
    {
        let work = data_dir
            .join("engines")
            .join("pdf2docx")
            .join("work")
            .join(random_hex()?);
        if let Err(e) = std::fs::create_dir_all(&work) {
            let _ = std::fs::remove_dir_all(&work);
            return Err(PdfWordError::Failed(format!(
                "creating the work directory failed: {e}"
            )));
        }
        let _remove = RemoveOnDrop(work.clone());
        let input_pdf = work.join("input.pdf");
        std::fs::copy(input, &input_pdf)
            .map_err(|e| PdfWordError::Failed(format!("copying the input failed: {e}")))?;
        let output_docx = work.join("output.docx");
        let absolute = std::path::absolute(&work)
            .map_err(|e| PdfWordError::Failed(format!("the work path: {e}")))?;
        let mut child = Command::new(program)
            .arg("convert")
            .arg(&input_pdf)
            .arg(&output_docx)
            .env("TMPDIR", &absolute)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| PdfWordError::Failed(format!("starting a24-pdf2docx failed: {e}")))?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() > CONVERT_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(PdfWordError::TooSlow);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(PdfWordError::Failed(format!(
                        "waiting for a24-pdf2docx failed: {e}"
                    )));
                }
            }
        };
        if status.code() == Some(3) {
            return Err(PdfWordError::Scanned);
        }
        if !status.success() {
            return Err(PdfWordError::Failed(format!(
                "a24-pdf2docx exited with {status}"
            )));
        }
        if !output_docx.is_file() {
            return Err(PdfWordError::Failed(format!(
                "a24-pdf2docx wrote no docx at {}",
                output_docx.display()
            )));
        }
        if std::fs::rename(&output_docx, out).is_err() {
            std::fs::copy(&output_docx, out)
                .map_err(|e| PdfWordError::Failed(format!("writing the docx failed: {e}")))?;
        }
        Ok(())
    }
}

/// The stored title with `.pdf` (any case) replaced by `.docx`; `.docx`
/// appended when it has no `.pdf` ending.
#[must_use]
pub fn docx_title(stored: &str) -> String {
    let ext = ".pdf";
    let stem = match stored.len().checked_sub(ext.len()) {
        Some(cut) if stored.is_char_boundary(cut) && stored[cut..].eq_ignore_ascii_case(ext) => {
            stored[..cut].to_owned()
        }
        _ => stored.to_owned(),
    };
    format!("{stem}.docx")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;
    use std::time::{Duration, Instant};

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::state::AppState;

    fn executable(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn work_entries(data_dir: &Path) -> usize {
        std::fs::read_dir(data_dir.join("engines/pdf2docx/work"))
            .map(|entries| entries.flatten().count())
            .unwrap_or(0)
    }

    /// A fake engine: copies `$SRC_DOCX` (or writes nothing) and exits `$1`-style.
    fn fake_engine(path: &Path, script_tail: &str) {
        executable(
            path,
            &format!(
                "#!/bin/sh\n[ \"$1\" = convert ] || exit 2\nin=\"$2\"\nout=\"$3\"\n[ -f \"$in\" ] || exit 4\n{script_tail}\n"
            ),
        );
    }

    #[test]
    fn the_program_is_present_only_after_install() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(program(dir.path()), None);
        let bin = engine_dir(dir.path()).join("a24-pdf2docx/a24-pdf2docx");
        executable(&bin, "#!/bin/sh\n");
        assert_eq!(program(dir.path()), None, "not ready before READY.json");
        std::fs::write(engine_dir(dir.path()).join("READY.json"), b"{}").unwrap();
        assert_eq!(program(dir.path()), Some(bin));
    }

    #[test]
    fn a_program_that_writes_the_docx_succeeds_and_the_work_dir_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let input = dir.path().join("in.pdf");
        std::fs::write(&input, b"%PDF-1.4").unwrap();
        let out = dir.path().join("out.docx");
        let bin = dir.path().join("engine");
        // The engine sees TMPDIR inside the work directory.
        fake_engine(
            &bin,
            "case \"$TMPDIR\" in */engines/pdf2docx/work/*) ;; *) exit 9 ;; esac\nprintf 'PK\\003\\004' > \"$out\"\necho '[INFO] noise' >&2\necho '{\"ok\": true, \"pages\": 1, \"seconds\": 0.1}'\nexit 0",
        );
        convert_to_docx(&data, &bin, &input, &out).unwrap();
        assert!(out.is_file());
        assert_eq!(work_entries(&data), 0);
    }

    #[test]
    fn exit_3_is_a_scan_and_other_failures_fail() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let input = dir.path().join("in.pdf");
        std::fs::write(&input, b"%PDF-1.4").unwrap();
        let out = dir.path().join("out.docx");
        let bin = dir.path().join("engine");

        fake_engine(
            &bin,
            "echo '{\"ok\": false, \"error\": \"scanned_pdf\", \"pages\": 1}'\nexit 3",
        );
        assert_eq!(
            convert_to_docx(&data, &bin, &input, &out),
            Err(PdfWordError::Scanned)
        );

        fake_engine(
            &bin,
            "echo '{\"ok\": false, \"error\": \"convert_failed\"}'\nexit 4",
        );
        assert!(matches!(
            convert_to_docx(&data, &bin, &input, &out),
            Err(PdfWordError::Failed(_))
        ));

        // Exit 0 but nothing written: a failure, not a success.
        fake_engine(
            &bin,
            "echo '{\"ok\": true, \"pages\": 1, \"seconds\": 0}'\nexit 0",
        );
        assert!(matches!(
            convert_to_docx(&data, &bin, &input, &out),
            Err(PdfWordError::Failed(_))
        ));
        assert!(!out.exists());
        assert_eq!(work_entries(&data), 0);
    }

    #[test]
    fn titles_end_in_docx() {
        assert_eq!(docx_title("合同.pdf"), "合同.docx");
        assert_eq!(docx_title("合同.PDF"), "合同.docx");
        assert_eq!(docx_title("收费目录"), "收费目录.docx");
        assert_eq!(docx_title("a.pdf.pdf"), "a.pdf.docx");
    }

    async fn pdf_document(state: &AppState, title: &str) -> String {
        let storage = state.storage().await.unwrap();
        let doc = "doc_01K74Z3QJ8V5N2W9RTX6Y00002";
        let sha = storage.blobs.put_bytes(b"%PDF-1.4 fake").unwrap().sha256;
        sqlx::raw_sql(&format!(
            "INSERT INTO documents (id, title, media_type, head_revision, created_at, updated_at)
                    VALUES ('{doc}', '{title}', '{PDF}', 1, '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z');
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{doc}', 1, '{sha}', 13, '{PDF}', 'import');"
        ))
        .execute(storage.db.pool())
        .await
        .unwrap();
        doc.to_owned()
    }

    async fn post_conversion(state: &AppState, doc: &str) -> (StatusCode, serde_json::Value) {
        let res = crate::router(state.clone())
            .oneshot(
                Request::post(format!("/documents/{doc}/conversions"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn ended(state: &AppState, job_id: &str) -> crate::jobs::Job {
        let storage = state.storage().await.unwrap();
        let started = Instant::now();
        loop {
            let job = crate::jobs::load(storage.db.pool(), job_id)
                .await
                .unwrap()
                .unwrap();
            if job.status != "running" {
                return job;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the job never ended"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn install_engine(data: &Path, script_tail: &str) {
        fake_engine(
            &engine_dir(data).join("a24-pdf2docx/a24-pdf2docx"),
            script_tail,
        );
        std::fs::write(engine_dir(data).join("READY.json"), b"{}").unwrap();
    }

    /// Without the engine: 503 `engine_unavailable` naming `pdf2docx`, no job.
    #[tokio::test]
    async fn a_pdf_without_the_engine_is_engine_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(dir.path()).await;
        let doc = pdf_document(&state, "合同.pdf").await;
        let (status, body) = post_conversion(&state, &doc).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "engine_unavailable");
        assert_eq!(body["error"]["details"]["engine"], "pdf2docx");
    }

    /// A PDF converts to a new DOCX document titled `<stem>.docx`, the
    /// `convert` job's result.
    #[tokio::test]
    async fn a_pdf_becomes_a_new_docx_document() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(dir.path()).await;
        let data = state.storage().await.unwrap().data_dir().to_path_buf();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/template-lease.docx");
        install_engine(
            &data,
            &format!(
                "cp '{}' \"$out\"\necho '{{\"ok\": true, \"pages\": 1, \"seconds\": 0.1}}'\nexit 0",
                fixture.display()
            ),
        );
        let doc = pdf_document(&state, "合同.pdf").await;
        let (status, job) = post_conversion(&state, &doc).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{job}");
        assert_eq!(job["kind"], "convert");
        let job = ended(&state, job["job_id"].as_str().unwrap()).await;
        assert_eq!(job.status, "succeeded", "{:?}", job.error);
        let new_doc = job.result.expect("the new document")["document_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let storage = state.storage().await.unwrap();
        let (title, media): (String, String) =
            sqlx::query_as("SELECT title, media_type FROM documents WHERE id = ?")
                .bind(&new_doc)
                .fetch_one(storage.db.pool())
                .await
                .unwrap();
        assert_eq!(title, "合同.docx");
        assert_eq!(media, crate::office::DOCX);
        assert_eq!(work_entries(&data), 0);
    }

    /// A scan ends the job failed with `unsupported_format` (no new code);
    /// any other engine failure with `parse_failed`.
    #[tokio::test]
    async fn a_scan_and_a_failure_end_the_job_failed() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(dir.path()).await;
        let data = state.storage().await.unwrap().data_dir().to_path_buf();
        let doc = pdf_document(&state, "扫描件.pdf").await;

        install_engine(
            &data,
            "echo '{\"ok\": false, \"error\": \"scanned_pdf\", \"pages\": 1}'\nexit 3",
        );
        let (status, job) = post_conversion(&state, &doc).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let job = ended(&state, job["job_id"].as_str().unwrap()).await;
        assert_eq!(job.status, "failed");
        assert_eq!(job.error.unwrap()["code"], "unsupported_format");

        install_engine(
            &data,
            "echo '{\"ok\": false, \"error\": \"convert_failed\"}'\nexit 4",
        );
        let (_, job) = post_conversion(&state, &doc).await;
        let job = ended(&state, job["job_id"].as_str().unwrap()).await;
        assert_eq!(job.status, "failed");
        assert_eq!(job.error.unwrap()["code"], "parse_failed");
    }
}
