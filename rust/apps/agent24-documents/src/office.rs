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
use crate::id::{IdKind, is_id};
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

fn random_hex() -> Result<String, ConvertError> {
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
        ConvertError::TooSlow => {
            ApiError::engine_unavailable("libreoffice", "the conversion took too long")
        }
        ConvertError::Failed(message) => ApiError::unprocessable("parse_failed", message),
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
    let Some(ext) = extension(&revision.media_type) else {
        return Err(ApiError::unprocessable(
            "unsupported_format",
            "only DOCX, XLSX and PPTX documents can be converted to PDF",
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
    let data_dir = storage.data_dir().to_path_buf();
    crate::derived::import_derived(&storage, title, "pdf", move |out| {
        let _held = match CONVERTING.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
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
    })
    .await
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
