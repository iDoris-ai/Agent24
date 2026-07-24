//! Local builtins: `fs_read` / `fs_write` (path-whitelisted) and `shell_exec`
//! (argv-array execution, never a shell string).
//!
//! Path whitelist semantics: a target is allowed only if its CANONICAL path
//! (symlinks resolved) sits under one of the canonicalized roots. For writes
//! the parent directory is canonicalized (the file itself may not exist yet)
//! and an existing target must not be a symlink pointing outside.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agent24_protocol::ToolInfo;
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolContext, ToolError, truncate};

const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024;

fn canonical_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots
        .into_iter()
        .filter_map(|r| match r.canonicalize() {
            Ok(c) => Some(c),
            Err(err) => {
                tracing::warn!("fs whitelist root {} dropped: {err}", r.display());
                None
            }
        })
        .collect()
}

fn under_roots(candidate: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| candidate.starts_with(r))
}

fn str_arg<'a>(input: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

/// Canonicalize `path` for reading and enforce the whitelist.
fn checked_read_path(raw: &str, roots: &[PathBuf]) -> Result<PathBuf, ToolError> {
    let canonical = Path::new(raw)
        .canonicalize()
        .map_err(|e| ToolError::Invalid(format!("path {raw}: {e}")))?;
    if !under_roots(&canonical, roots) {
        return Err(ToolError::Denied(format!(
            "path {raw} is outside the allowed workspace"
        )));
    }
    Ok(canonical)
}

/// Resolve `path` for writing: canonicalize the parent (must exist), then
/// re-attach the final component. An existing symlink target is rejected —
/// writing through it could escape the whitelist.
fn checked_write_path(raw: &str, roots: &[PathBuf]) -> Result<PathBuf, ToolError> {
    let path = Path::new(raw);
    let name = path
        .file_name()
        .ok_or_else(|| ToolError::Invalid(format!("path {raw} has no file name")))?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let parent = parent.ok_or_else(|| ToolError::Invalid(format!("path {raw} has no parent")))?;
    let parent = parent
        .canonicalize()
        .map_err(|e| ToolError::Invalid(format!("parent of {raw}: {e}")))?;
    if !under_roots(&parent, roots) {
        return Err(ToolError::Denied(format!(
            "path {raw} is outside the allowed workspace"
        )));
    }
    let target = parent.join(name);
    if target.is_symlink() {
        return Err(ToolError::Denied(format!(
            "path {raw} is a symlink — refusing to write through it"
        )));
    }
    Ok(target)
}

// ── fs_read ──────────────────────────────────────────────────────────────────

pub struct FsReadTool {
    roots: Vec<PathBuf>,
}

impl FsReadTool {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: canonical_roots(roots),
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
        let path = checked_read_path(raw, &self.roots)?;
        if !path.is_file() {
            return Err(ToolError::Invalid(format!("{raw} is not a regular file")));
        }
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| ToolError::Failed(format!("read {raw}: {e}")))?;
        Ok(truncate(&String::from_utf8_lossy(&bytes), MAX_READ_BYTES))
    }
}

// ── fs_write ─────────────────────────────────────────────────────────────────

pub struct FsWriteTool {
    roots: Vec<PathBuf>,
}

impl FsWriteTool {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: canonical_roots(roots),
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
        let path = checked_write_path(raw, &self.roots)?;
        // O_NOFOLLOW closes the check-then-write race: even if the target is
        // swapped for a symlink after checked_write_path, the open fails
        // instead of following it out of the workspace.
        let bytes = content.as_bytes().to_vec();
        let raw_owned = raw.to_owned();
        let written = tokio::task::spawn_blocking(move || -> std::io::Result<usize> {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            file.write_all(&bytes)?;
            Ok(bytes.len())
        })
        .await
        .map_err(|e| ToolError::Failed(format!("write task: {e}")))?
        .map_err(|e| ToolError::Failed(format!("write {raw_owned}: {e}")))?;
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
            // kill_on_drop: a timeout or cancellation drops the child future —
            // the process must die with it, never linger
            .kill_on_drop(true);
        let fut = cmd.output();
        let output = tokio::select! {
            r = fut => r.map_err(|e| ToolError::Failed(format!("spawn {program}: {e}")))?,
            () = cancel.cancelled() => return Err(ToolError::Cancelled),
        };

        let out = serde_json::json!({
            "exit_code": output.status.code(),
            "stdout": truncate(&String::from_utf8_lossy(&output.stdout), MAX_STREAM_BYTES),
            "stderr": truncate(&String::from_utf8_lossy(&output.stderr), MAX_STREAM_BYTES),
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
