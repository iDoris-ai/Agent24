//! The slice-1 read engine (ADR-DOC-01 D6 amendment, ADR-DOC-02 §3.1): the
//! `agent24-documents-pdfkit` helper, shipped next to this binary, run once
//! per file. Its report becomes a text layer through `text_layer::build`.
//! The helper is macOS only; elsewhere there is no engine (D10).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::{Engine, EngineError, Parse};
use crate::text_layer::build::{self, Read};
use crate::text_layer::{EngineRef, MAX_BLOCK_BYTES, MAX_LINES};

pub const HELPER: &str = "agent24-documents-pdfkit";
pub const ENGINE_ID: &str = "apple-pdfkit";
pub const FORMATS: [&str; 3] = ["application/pdf", "image/jpeg", "image/png"];

/// One parse, from start to exit (§3.1).
const TIMEOUT: Duration = Duration::from_secs(120);
/// The report, at most; and what is kept of its error output.
const MAX_OUTPUT: usize = 64 << 20;
const MAX_STDERR: usize = 4 << 10;

pub struct PdfKit {
    path: PathBuf,
    /// The macOS version: PDFKit reads differently from one to the next.
    os: String,
    timeout: Duration,
    max_output: usize,
}

impl PdfKit {
    /// The helper next to this executable, if this is macOS and it is there.
    #[must_use]
    pub fn find() -> Option<Self> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let path = std::env::current_exe().ok()?.parent()?.join(HELPER);
        let os = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let os = canonical(String::from_utf8(os.stdout).ok()?.trim())?;
        path.is_file().then(|| Self::at(path, &os))
    }

    /// The helper at `path` on macOS `os`; `os` is taken as [`canonical`]
    /// makes it, or as given if it is not a version.
    #[must_use]
    pub fn at(path: PathBuf, os: &str) -> Self {
        Self {
            path,
            os: canonical(os).unwrap_or_else(|| os.to_owned()),
            timeout: TIMEOUT,
            max_output: MAX_OUTPUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn limited(mut self, timeout: Duration, max_output: usize) -> Self {
        (self.timeout, self.max_output) = (timeout, max_output);
        self
    }
}

/// A macOS version as `major.minor.patch`, as the helper reports it:
/// `sw_vers` leaves out a zero patch ("26.6" is "26.6.0").
fn canonical(v: &str) -> Option<String> {
    let parts: Vec<u32> = v
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    match parts[..] {
        [major, minor] => Some(format!("{major}.{minor}.0")),
        [major, minor, patch] => Some(format!("{major}.{minor}.{patch}")),
        _ => None,
    }
}

/// Up to `max` bytes of `from`, and whether there was more; the rest is
/// left unread (`drain`: read and dropped, so the writer is not blocked).
async fn read_capped(
    mut from: impl AsyncRead + Unpin,
    max: usize,
    drain: bool,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    (&mut from)
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .await?;
    let over = out.len() > max;
    out.truncate(max);
    if drain {
        tokio::io::copy(&mut from, &mut tokio::io::sink()).await?;
    }
    Ok((out, over))
}

impl Engine for PdfKit {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: ENGINE_ID.into(),
            version: self.os.clone(),
        }
    }

    /// Bumped with any change to how the report becomes blocks.
    fn config(&self) -> Value {
        json!({ "helper": 1, "blocks": "rows-v1", "max_block_bytes": MAX_BLOCK_BYTES, "max_lines": MAX_LINES })
    }

    fn parse(&self, content_sha256: &str, media_type: &str, path: &Path) -> Parse {
        let (engine, config) = (self.engine(), self.config());
        let (content, os, max) = (content_sha256.to_owned(), self.os.clone(), self.max_output);
        let mut command = Command::new(&self.path);
        command.arg("parse").arg(path).arg(media_type);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let timeout = self.timeout;
        Box::pin(async move {
            let failed = |why: String| EngineError::Failed(why);
            let mut child = command
                .spawn()
                .map_err(|e| failed(format!("the engine did not start: {e}")))?;
            let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
                return Err(failed("the engine's output is not piped".into()));
            };
            // A report over the cap stops the helper at once; its error
            // output is kept to a few KiB and the rest read away.
            let run = async {
                let report = async {
                    let read = read_capped(out, max, false).await;
                    if matches!(read, Ok((_, true))) {
                        let _ = child.start_kill();
                    }
                    read
                };
                let (out, err) = tokio::join!(report, read_capped(err, MAX_STDERR, true));
                let (out, err) = (out?, err?);
                Ok::<_, std::io::Error>((out, err, child.wait().await?))
            };
            // On timeout the child is dropped, and killed with it.
            let ((out, over), (err, _), status) = tokio::time::timeout(timeout, run)
                .await
                .map_err(|_| failed("the engine took too long".into()))?
                .map_err(|e| failed(format!("the engine's output could not be read: {e}")))?;
            let said = String::from_utf8_lossy(&err)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            if over {
                return Err(failed("the engine's report is too large".into()));
            }
            match status.code() {
                Some(0) => {
                    let read: Read = serde_json::from_slice(&out)
                        .map_err(|e| failed(format!("the engine's report is not one: {e}")))?;
                    if read.os_version != os {
                        return Err(failed(
                            "macOS changed under the engine; restart the OS".into(),
                        ));
                    }
                    build::layer(&content, engine, config, read).map_err(failed)
                }
                Some(2) => Err(EngineError::Unsupported),
                Some(code) => Err(failed(format!(
                    "the engine could not read the file ({code}): {said}"
                ))),
                None => Err(failed("the engine stopped".into())),
            }
        })
    }
}

#[cfg(all(test, unix))]
mod tests;
