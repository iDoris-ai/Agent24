//! Local builtins: `fs_read` / `fs_write` (path-whitelisted) and `shell_exec`
//! (argv-array execution, never a shell string).
//!
//! Path whitelist semantics: every root is opened ONCE as a `cap_std::fs::Dir`
//! (a pinned directory fd). All path resolution then happens INSIDE that fd
//! with openat-style beneath-only traversal — a symlink (or a parent-directory
//! swap) pointing outside the workspace fails at resolution time, not at some
//! earlier check that could race. In-workspace symlinks still work.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent24_protocol::ToolInfo;
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolContext, ToolError, truncate};

const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024;

/// One whitelisted root: the paths it answers for (as given + canonicalized,
/// so `/tmp/...` matches even when the dir really lives under `/private/tmp`)
/// plus the pinned directory handle all I/O goes through.
struct Root {
    prefixes: Vec<PathBuf>,
    dir: Arc<cap_std::fs::Dir>,
}

fn open_roots(roots: Vec<PathBuf>) -> Vec<Root> {
    roots
        .into_iter()
        .filter_map(|r| {
            let canonical = match r.canonicalize() {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!("fs whitelist root {} dropped: {err}", r.display());
                    return None;
                }
            };
            match cap_std::fs::Dir::open_ambient_dir(&canonical, cap_std::ambient_authority()) {
                Ok(dir) => {
                    let mut prefixes = vec![canonical];
                    if !prefixes.contains(&r) {
                        prefixes.push(r);
                    }
                    Some(Root {
                        prefixes,
                        dir: Arc::new(dir),
                    })
                }
                Err(err) => {
                    tracing::warn!("fs whitelist root {} unopenable: {err}", r.display());
                    None
                }
            }
        })
        .collect()
}

fn str_arg<'a>(input: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

/// Lexically fold `.` / `..` (no filesystem access). `None` when `..` would
/// climb above the root — those can never be workspace paths.
fn lexical_normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other),
        }
    }
    Some(out)
}

/// Map an absolute input path onto (pinned root dir, path relative to it).
fn resolve_in_roots<'a>(raw: &str, roots: &'a [Root]) -> Result<(&'a Root, PathBuf), ToolError> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(ToolError::Invalid(format!(
            "path {raw} must be absolute (inside the workspace)"
        )));
    }
    let norm = lexical_normalize(path)
        .ok_or_else(|| ToolError::Denied(format!("path {raw} escapes the filesystem root")))?;
    for root in roots {
        for prefix in &root.prefixes {
            if let Ok(rel) = norm.strip_prefix(prefix) {
                if rel.as_os_str().is_empty() {
                    return Err(ToolError::Invalid(format!(
                        "path {raw} is the workspace root, not a file"
                    )));
                }
                return Ok((root, rel.to_path_buf()));
            }
        }
    }
    Err(ToolError::Denied(format!(
        "path {raw} is outside the allowed workspace"
    )))
}

/// cap-std reports beneath-escapes as distinct I/O errors; surface them as
/// policy denials, everything else as plain failures.
fn map_fs_err(raw: &str, err: &std::io::Error) -> ToolError {
    let msg = err.to_string();
    if err.kind() == std::io::ErrorKind::PermissionDenied
        || msg.contains("outside of the filesystem")
    {
        ToolError::Denied(format!("path {raw} resolves outside the workspace: {msg}"))
    } else {
        ToolError::Failed(format!("{raw}: {msg}"))
    }
}

// ── fs_read ──────────────────────────────────────────────────────────────────

pub struct FsReadTool {
    roots: Vec<Root>,
}

impl FsReadTool {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: open_roots(roots),
        }
    }
}

#[async_trait]
impl Tool for FsReadTool {
    fn info(&self) -> ToolInfo {
        ToolInfo {
            name: "fs_read".to_owned(),
            source: "builtin".to_owned(),
            description: "Read a UTF-8 text file inside the agent workspace (256 KiB cap)."
                .to_owned(),
            requires_approval: false,
        }
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path inside the workspace" }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        input: &Map<String, Value>,
        _cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let raw =
            str_arg(input, "path").ok_or_else(|| ToolError::Invalid("path is required".into()))?;
        let (root, rel) = resolve_in_roots(raw, &self.roots)?;
        // Everything below happens through the pinned dirfd: beneath-only
        // traversal (parent swaps and escaping symlinks fail at open), type
        // check from the fd, bounded read — never more than cap+1 in memory.
        let dir = Arc::clone(&root.dir);
        let raw_owned = raw.to_owned();
        let out = tokio::task::spawn_blocking(move || -> Result<String, ToolError> {
            use std::io::Read as _;
            let file = dir.open(&rel).map_err(|e| map_fs_err(&raw_owned, &e))?;
            let meta = file
                .metadata()
                .map_err(|e| ToolError::Failed(format!("stat {raw_owned}: {e}")))?;
            if !meta.is_file() {
                return Err(ToolError::Invalid(format!(
                    "{raw_owned} is not a regular file"
                )));
            }
            let mut bytes = Vec::new();
            file.take(MAX_READ_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| ToolError::Failed(format!("read {raw_owned}: {e}")))?;
            Ok(truncate(&String::from_utf8_lossy(&bytes), MAX_READ_BYTES))
        })
        .await
        .map_err(|e| ToolError::Failed(format!("read task: {e}")))??;
        Ok(out)
    }
}

// ── fs_write ─────────────────────────────────────────────────────────────────

pub struct FsWriteTool {
    roots: Vec<Root>,
}

impl FsWriteTool {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: open_roots(roots),
        }
    }
}

#[async_trait]
impl Tool for FsWriteTool {
    fn info(&self) -> ToolInfo {
        ToolInfo {
            name: "fs_write".to_owned(),
            source: "builtin".to_owned(),
            description: "Write a UTF-8 text file inside the agent workspace (overwrites)."
                .to_owned(),
            requires_approval: true,
        }
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path inside the workspace" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        input: &Map<String, Value>,
        _cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let raw =
            str_arg(input, "path").ok_or_else(|| ToolError::Invalid("path is required".into()))?;
        let content = str_arg(input, "content")
            .ok_or_else(|| ToolError::Invalid("content is required".into()))?;
        let (root, rel) = resolve_in_roots(raw, &self.roots)?;
        // Beneath-only traversal from the pinned dirfd — parent swaps and
        // symlinks escaping the workspace fail at open, atomically with it.
        let dir = Arc::clone(&root.dir);
        let bytes = content.as_bytes().to_vec();
        let raw_owned = raw.to_owned();
        let written = tokio::task::spawn_blocking(move || -> Result<usize, ToolError> {
            use std::io::Write as _;
            let mut opts = cap_std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            let mut file = dir
                .open_with(&rel, &opts)
                .map_err(|e| map_fs_err(&raw_owned, &e))?;
            file.write_all(&bytes)
                .map_err(|e| ToolError::Failed(format!("write {raw_owned}: {e}")))?;
            Ok(bytes.len())
        })
        .await
        .map_err(|e| ToolError::Failed(format!("write task: {e}")))??;
        Ok(format!("wrote {written} bytes to {raw}"))
    }
}

// ── shell_exec ───────────────────────────────────────────────────────────────

pub struct ShellExecTool {
    /// Default (and only allowed) working directory root
    workdir: PathBuf,
    budget: Duration,
}

impl ShellExecTool {
    pub fn new(workdir: PathBuf) -> Self {
        Self {
            workdir,
            budget: Duration::from_secs(30),
        }
    }

    /// Tests use tiny budgets to exercise the timeout kill path.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }
}

#[async_trait]
impl Tool for ShellExecTool {
    fn info(&self) -> ToolInfo {
        ToolInfo {
            name: "shell_exec".to_owned(),
            source: "builtin".to_owned(),
            description: "Execute a command as an argv array (no shell interpretation), \
                          cwd inside the agent workspace."
                .to_owned(),
            requires_approval: true,
        }
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "argv": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Command and arguments, executed directly (no shell)"
                }
            },
            "required": ["argv"],
            "additionalProperties": false
        })
    }

    fn timeout(&self) -> Duration {
        self.budget
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let argv: Vec<String> = input
            .get("argv")
            .and_then(Value::as_array)
            .ok_or_else(|| ToolError::Invalid("argv (array of strings) is required".into()))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ToolError::Invalid("argv entries must be strings".into()))
            })
            .collect::<Result<_, _>>()?;
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| ToolError::Invalid("argv must not be empty".into()))?;

        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(&self.workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // kill_on_drop: a timeout or cancellation drops the child future —
            // the process must die with it, never linger
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Failed(format!("spawn {program}: {e}")))?;

        // Stream both pipes with a hard cap: keep the first 16 KiB, then keep
        // DRAINING (discarding) so the child never blocks on a full pipe —
        // but never buffer more than the cap in memory.
        async fn capped_drain(
            mut src: impl tokio::io::AsyncRead + Unpin,
        ) -> std::io::Result<(Vec<u8>, bool)> {
            use tokio::io::AsyncReadExt as _;
            let mut kept = Vec::new();
            let mut dropped = false;
            let mut buf = [0u8; 8192];
            loop {
                let n = src.read(&mut buf).await?;
                if n == 0 {
                    return Ok((kept, dropped));
                }
                let room = MAX_STREAM_BYTES.saturating_sub(kept.len());
                let take = n.min(room);
                kept.extend_from_slice(&buf[..take]);
                if take < n {
                    dropped = true;
                }
            }
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let io = async {
            // Both pipes drained CONCURRENTLY — sequential draining deadlocks
            // when the child fills the un-drained pipe while the other stays open
            let out_fut = async {
                match stdout {
                    Some(s) => capped_drain(s).await.unwrap_or((Vec::new(), false)),
                    None => (Vec::new(), false),
                }
            };
            let err_fut = async {
                match stderr {
                    Some(s) => capped_drain(s).await.unwrap_or((Vec::new(), false)),
                    None => (Vec::new(), false),
                }
            };
            let (out, err) = tokio::join!(out_fut, err_fut);
            let status = child.wait().await;
            (out, err, status)
        };
        let ((stdout, out_dropped), (stderr, err_dropped), status) = tokio::select! {
            r = io => r,
            () = cancel.cancelled() => return Err(ToolError::Cancelled),
        };
        let status = status.map_err(|e| ToolError::Failed(format!("wait {program}: {e}")))?;

        let render = |bytes: &[u8], dropped: bool| {
            let s = String::from_utf8_lossy(bytes).into_owned();
            if dropped {
                format!("{s}… [output capped at {MAX_STREAM_BYTES} bytes]")
            } else {
                s
            }
        };
        let out = serde_json::json!({
            "exit_code": status.code(),
            "stdout": render(&stdout, out_dropped),
            "stderr": render(&stderr, err_dropped),
        });
        Ok(out.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::ToolRegistry;
    use std::sync::Arc;

    fn ctx() -> ToolContext {
        ToolContext {
            run_id: "run_test".to_owned(),
        }
    }

    fn sinput(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
            .collect()
    }

    #[tokio::test]
    async fn fs_read_and_write_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let file = root.join("note.txt");
        let write = FsWriteTool::new(vec![root.clone()]);
        let cancel = CancellationToken::new();
        let out = write
            .call(
                &ctx(),
                &sinput(&[("path", file.to_str().unwrap()), ("content", "hello")]),
                &cancel,
            )
            .await
            .unwrap();
        assert!(out.contains("5 bytes"));

        let read = FsReadTool::new(vec![root]);
        let out = read
            .call(
                &ctx(),
                &sinput(&[("path", file.to_str().unwrap())]),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn path_escape_attempts_are_denied() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let cancel = CancellationToken::new();
        let read = FsReadTool::new(vec![root.clone()]);
        let write = FsWriteTool::new(vec![root.clone()]);

        // dot-dot traversal out of the root
        let escape = root.join("../escape.txt");
        let err = write
            .call(
                &ctx(),
                &sinput(&[("path", escape.to_str().unwrap()), ("content", "x")]),
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");

        // absolute path outside
        let err = read
            .call(&ctx(), &sinput(&[("path", "/etc/hosts")]), &cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");
    }

    #[tokio::test]
    async fn symlink_escapes_are_denied() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let link = root.join("link.txt");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let cancel = CancellationToken::new();

        // read through the symlink: canonical target is outside → denied
        let read = FsReadTool::new(vec![root.clone()]);
        let err = read
            .call(
                &ctx(),
                &sinput(&[("path", link.to_str().unwrap())]),
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");

        // write through the symlink: rejected as symlink
        let write = FsWriteTool::new(vec![root]);
        let err = write
            .call(
                &ctx(),
                &sinput(&[("path", link.to_str().unwrap()), ("content", "pwn")]),
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "secret");
    }

    #[tokio::test]
    async fn symlinked_parent_directory_cannot_escape() {
        // The dirfd-anchored traversal must also stop escapes via an
        // intermediate PATH COMPONENT that is a symlink out of the workspace
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("dirlink")).unwrap();
        let cancel = CancellationToken::new();

        let read = FsReadTool::new(vec![root.clone()]);
        let target = root.join("dirlink/secret.txt");
        let err = read
            .call(
                &ctx(),
                &sinput(&[("path", target.to_str().unwrap())]),
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");

        let write = FsWriteTool::new(vec![root]);
        let err = write
            .call(
                &ctx(),
                &sinput(&[("path", target.to_str().unwrap()), ("content", "pwn")]),
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
            "secret"
        );
    }

    #[tokio::test]
    async fn shell_exec_runs_argv_without_shell_interpretation() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ShellExecTool::new(dir.path().to_path_buf());
        let cancel = CancellationToken::new();
        let mut input = Map::new();
        // `$HOME` must NOT be expanded — argv goes straight to exec
        input.insert(
            "argv".to_owned(),
            serde_json::json!(["/bin/echo", "$HOME", "two words"]),
        );
        let out = tool.call(&ctx(), &input, &cancel).await.unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["exit_code"], 0);
        assert_eq!(parsed["stdout"], "$HOME two words\n");
    }

    #[tokio::test]
    async fn shell_exec_reports_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ShellExecTool::new(dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert("argv".to_owned(), serde_json::json!(["/usr/bin/false"]));
        let out = tool
            .call(&ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["exit_code"], 1);
    }

    #[tokio::test]
    async fn shell_exec_times_out_through_the_registry_budget() {
        // Registry-level timeout uses tool.timeout(); the whitelist+approval
        // gates are bypassed here by whitelisting a non-approval wrapper —
        // instead we test the budget directly through dispatch on a
        // no-approval clone of the tool.
        struct NoApproval(ShellExecTool);
        #[async_trait]
        impl Tool for NoApproval {
            fn info(&self) -> ToolInfo {
                ToolInfo {
                    requires_approval: false,
                    ..self.0.info()
                }
            }
            fn parameters(&self) -> Value {
                self.0.parameters()
            }
            fn timeout(&self) -> Duration {
                self.0.timeout()
            }
            async fn call(
                &self,
                ctx: &ToolContext,
                input: &Map<String, Value>,
                cancel: &CancellationToken,
            ) -> Result<String, ToolError> {
                self.0.call(ctx, input, cancel).await
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let tool =
            ShellExecTool::new(dir.path().to_path_buf()).with_budget(Duration::from_millis(100));
        let reg = ToolRegistry::new().with(Arc::new(NoApproval(tool)));
        let mut input = Map::new();
        input.insert("argv".to_owned(), serde_json::json!(["/bin/sleep", "30"]));
        let started = std::time::Instant::now();
        let err = reg
            .dispatch("shell_exec", &ctx(), &input, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Timeout(_)), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
