//! The slice-1 read engine (ADR-DOC-01 D6 amendment, ADR-DOC-02 §3.1): the
//! `agent24-documents-pdfkit` helper, shipped next to this binary, run once
//! per file. Its report becomes a text layer through `text_layer::build`.
//! The helper is macOS only; elsewhere there is no engine (D10).

use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::{Engine, EngineError, MIN_SCALE, Parse, Render, RenderAsk, RenderError, Rendered};
use crate::text_layer::build::{self, Read};
use crate::text_layer::{EngineRef, MAX_BLOCK_BYTES, MAX_LINES};

pub const HELPER: &str = "agent24-documents-pdfkit";
pub const ENGINE_ID: &str = "apple-pdfkit";
pub const FORMATS: [&str; 3] = ["application/pdf", "image/jpeg", "image/png"];

/// One parse, from start to exit (§3.1).
const TIMEOUT: Duration = Duration::from_secs(120);
/// One render: within the kernel proxy's 10 s for a response's head (§4).
const RENDER_TIMEOUT: Duration = Duration::from_secs(8);
/// A render's header line, at most.
const MAX_HEAD: usize = 1 << 10;
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
/// The report, at most; and what is kept of its error output.
const MAX_OUTPUT: usize = 64 << 20;
const MAX_STDERR: usize = 4 << 10;

pub struct PdfKit {
    path: PathBuf,
    /// The macOS version: PDFKit reads differently from one to the next.
    os: String,
    timeout: Duration,
    render_timeout: Duration,
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
            render_timeout: RENDER_TIMEOUT,
            max_output: MAX_OUTPUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn limited(mut self, timeout: Duration, max_output: usize) -> Self {
        (self.timeout, self.render_timeout, self.max_output) = (timeout, timeout, max_output);
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
        let (content, os) = (content_sha256.to_owned(), self.os.clone());
        let args = [
            OsStr::new("parse"),
            path.as_os_str(),
            OsStr::new(media_type),
        ];
        let run = self.run(&args, self.timeout, self.max_output);
        Box::pin(async move {
            let failed = EngineError::Failed;
            let ran = run.await.map_err(|e| failed(e.to_string()))?;
            match ran.code {
                Some(0) => {
                    let read: Read = serde_json::from_slice(&ran.out)
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
                    "the engine could not read the file ({code}): {}",
                    ran.said
                ))),
                None => Err(failed("the engine stopped".into())),
            }
        })
    }

    fn render(&self, media_type: &str, path: &Path, ask: RenderAsk) -> Render {
        let mut args: Vec<OsString> = vec![
            "render".into(),
            path.into(),
            media_type.into(),
            ask.page.to_string().into(),
            ask.scale.to_string().into(),
            ask.max_bytes.to_string().into(),
        ];
        if let Some([x0, y0, x1, y1]) = ask.region {
            args.push(format!("{x0},{y0},{x1},{y1}").into());
        }
        let run = self.run(&args, self.render_timeout, ask.max_bytes + MAX_HEAD);
        Box::pin(async move {
            let failed = RenderError::Failed;
            let ran = run.await.map_err(|e| match e {
                RunError::TooLong => RenderError::TooSlow,
                RunError::Other(why) => failed(why),
            })?;
            match ran.code {
                Some(0) => rendered(&ran.out, &ask).map_err(failed),
                Some(2) => Err(RenderError::Unsupported),
                Some(4) => Err(RenderError::NoPage),
                Some(5) => Err(RenderError::OffPage),
                Some(6) => Err(RenderError::TooLarge),
                Some(code) => Err(failed(format!(
                    "the engine could not render the file ({code}): {}",
                    ran.said
                ))),
                None => Err(failed("the engine stopped".into())),
            }
        })
    }
}

/// What `render` wrote, checked against what was asked: a line saying the
/// scale it used, then a PNG within the size asked for.
fn rendered(out: &[u8], ask: &RenderAsk) -> Result<Rendered, String> {
    #[derive(Deserialize)]
    struct Head {
        protocol: u32,
        scale: f64,
    }
    let at = out
        .iter()
        .position(|b| *b == b'\n')
        .ok_or("the engine's render has no header")?;
    let head: Head = serde_json::from_slice(&out[..at])
        .map_err(|e| format!("the engine's render header is not one: {e}"))?;
    let png = &out[at + 1..];
    if head.protocol != 1 {
        return Err(format!("the engine speaks protocol {}", head.protocol));
    }
    if !(MIN_SCALE..=ask.scale).contains(&head.scale) {
        return Err(format!("the engine rendered at scale {}", head.scale));
    }
    if png.len() > ask.max_bytes || !png.starts_with(PNG) {
        return Err("the engine's render is not a PNG within the size asked for".into());
    }
    Ok(Rendered {
        scale: head.scale,
        png: png.to_vec(),
    })
}

/// Why a run of the helper left nothing to read.
#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("the engine took too long")]
    TooLong,
    #[error("{0}")]
    Other(String),
}

/// A run of the helper: its output, its exit code (none if a signal stopped
/// it) and the first line of its error output.
struct Ran {
    out: Vec<u8>,
    code: Option<i32>,
    said: String,
}

impl PdfKit {
    /// Runs the helper with `args`. Running past `timeout`, or writing more
    /// than `max` bytes, fails the run and stops the helper at once.
    fn run(
        &self,
        args: &[impl AsRef<OsStr>],
        timeout: Duration,
        max: usize,
    ) -> impl Future<Output = Result<Ran, RunError>> + Send + 'static {
        let mut command = Command::new(&self.path);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        async move {
            let mut child = command
                .spawn()
                .map_err(|e| RunError::Other(format!("the engine did not start: {e}")))?;
            let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
                return Err(RunError::Other("the engine's output is not piped".into()));
            };
            // Output over the cap stops the helper at once; its error
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
                .map_err(|_| RunError::TooLong)?
                .map_err(|e| {
                    RunError::Other(format!("the engine's output could not be read: {e}"))
                })?;
            if over {
                return Err(RunError::Other("the engine's output is too large".into()));
            }
            let said = String::from_utf8_lossy(&err)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            Ok(Ran {
                out,
                code: status.code(),
                said,
            })
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
