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

use agent24_protocol::{RiskClass, ToolInfo};
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolContext, ToolError, truncate};
use agent24_domain::{EgressDestination, EgressPurpose};

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

/// Open the parent directory before the gate. The returned handle is what the
/// later write uses. Relative paths and the filesystem root have none.
pub(crate) fn approved_parent(input: &Map<String, Value>) -> Option<Arc<crate::PinnedOutsideDir>> {
    let raw = str_arg(input, "path")?;
    let path = Path::new(raw);
    if !path.is_absolute() {
        return None;
    }
    let parent = path.parent()?.canonicalize().ok()?;
    pin_outside_dir(parent).ok()
}

pub(crate) fn pin_outside_dir(
    parent: PathBuf,
) -> Result<Arc<crate::PinnedOutsideDir>, std::io::Error> {
    // `parent` was canonical when it was resolved, but opening it resolves it
    // again: a component swapped for a symlink in between would leave the
    // handle on another directory under the approved name. Opened without
    // following a symlink at any level, the handle is the directory that is
    // at this very name, or the open fails.
    // Directory open fails immediately on a FIFO or regular file. `File::open`
    // would block on a FIFO and stall the caller before any approval exists.
    if !parent.is_absolute()
        || parent
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a canonical absolute path",
        ));
    }
    let dir = open_dir_through_no_symlink(&parent)?;
    let meta = dir.dir_metadata()?;
    Ok(Arc::new(crate::PinnedOutsideDir {
        path: parent,
        dir: Arc::new(dir),
        dev: cap_fs_ext::MetadataExt::dev(&meta),
        ino: cap_fs_ext::MetadataExt::ino(&meta),
    }))
}

/// macOS 11 (Darwin 20) and later: one open of the whole path that fails on
/// a symlink at any level (`O_NOFOLLOW_ANY`). Like a plain open, ancestors
/// need only search permission. Older kernels ignore the flag without an
/// error, so there the path is walked as elsewhere (each level then needs
/// read permission: an approved write under an unreadable ancestor fails).
#[cfg(target_os = "macos")]
fn open_dir_through_no_symlink(path: &Path) -> Result<cap_std::fs::Dir, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt;
    if !nofollow_any_is_honoured() {
        return walk_without_following(path);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW_ANY)
        .open(path)?;
    Ok(cap_std::fs::Dir::from_std_file(file))
}

/// Whether this kernel is Darwin 20 (macOS 11) or later, read once. An
/// unreadable release counts as older: the walk is the closed direction.
#[cfg(target_os = "macos")]
fn nofollow_any_is_honoured() -> bool {
    static HONOURED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HONOURED.get_or_init(|| darwin_major().is_some_and(|major| major >= 20))
}

#[cfg(target_os = "macos")]
fn darwin_major() -> Option<u32> {
    let name = rustix::system::uname();
    name.release()
        .to_str()
        .ok()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Elsewhere: walked from the root, no level followed. cap-std opens each
/// with `O_PATH` where there is one (Linux), so ancestors need only search
/// permission there too.
#[cfg(not(target_os = "macos"))]
fn open_dir_through_no_symlink(path: &Path) -> Result<cap_std::fs::Dir, std::io::Error> {
    walk_without_following(path)
}

/// The canonical path opened level by level from the root, following none.
fn walk_without_following(path: &Path) -> Result<cap_std::fs::Dir, std::io::Error> {
    let mut dir = cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for component in path.components() {
        if let Component::Normal(name) = component {
            dir = cap_fs_ext::DirExt::open_dir_nofollow(&dir, name)?;
        }
    }
    Ok(dir)
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
    fn requires_outbound_policy(&self) -> bool {
        false
    }

    fn info(&self) -> ToolInfo {
        ToolInfo::new(
            "fs_read",
            "builtin",
            "Read a UTF-8 text file inside the agent workspace (256 KiB cap).",
            RiskClass::Read,
        )
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
        ctx: &ToolContext,
        input: &Map<String, Value>,
        _cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let raw =
            str_arg(input, "path").ok_or_else(|| ToolError::Invalid("path is required".into()))?;
        if let Some(authority) = ctx.workspace_authority() {
            let bytes = authority
                .read_file(raw.to_owned(), MAX_READ_BYTES)
                .await
                .map_err(|e| ToolError::Denied(format!("workspace authority unavailable: {e}")))?;
            return Ok(truncate(&String::from_utf8_lossy(&bytes), MAX_READ_BYTES));
        }
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
    fn requires_outbound_policy(&self) -> bool {
        false
    }

    fn info(&self) -> ToolInfo {
        ToolInfo::new(
            "fs_write",
            "builtin",
            "Write a UTF-8 text file inside the agent workspace (overwrites).",
            RiskClass::WriteLocal,
        )
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

    /// An outside write's approval is for the directory captured before the
    /// gate (L-APPR-8). The durable-resume path captured none, and which one
    /// the human saw is not recorded, so the call is asked again
    /// (`ApprovalStale`, as for a module call), not refused by the whitelist.
    async fn call_with_invocation(
        &self,
        ctx: &ToolContext,
        invocation: &crate::ToolInvocation,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let outside_unpinned = || {
            str_arg(input, "path").is_some_and(|raw| {
                Path::new(raw).is_absolute() && resolve_in_roots(raw, &self.roots).is_err()
            })
        };
        if matches!(invocation.approval, crate::ApprovalBinding::Unbound)
            && ctx.workspace_authority().is_none()
            && ctx.pinned_outside_directory().is_none()
            && outside_unpinned()
        {
            return Err(ToolError::ApprovalStale);
        }
        self.call(ctx, input, cancel).await
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        _cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let raw =
            str_arg(input, "path").ok_or_else(|| ToolError::Invalid("path is required".into()))?;
        let content = str_arg(input, "content")
            .ok_or_else(|| ToolError::Invalid("content is required".into()))?;
        if let Some(authority) = ctx.workspace_authority() {
            let written = authority
                .write_file(raw.to_owned(), content.as_bytes().to_vec())
                .await
                .map_err(|e| ToolError::Denied(format!("workspace authority unavailable: {e}")))?;
            return Ok(format!("wrote {written} bytes to {raw}"));
        }
        if let Err(err) = resolve_in_roots(raw, &self.roots) {
            let Some(pinned) = ctx.pinned_outside_directory().cloned() else {
                return Err(err);
            };
            let bytes = content.as_bytes().to_vec();
            let raw_owned = raw.to_owned();
            let written = tokio::task::spawn_blocking(move || {
                write_approved_outside(&raw_owned, &pinned, &bytes)
            })
            .await
            .map_err(|e| ToolError::Failed(format!("write task: {e}")))??;
            return Ok(format!("wrote {written} bytes to {raw}"));
        }
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

fn write_approved_outside(
    raw: &str,
    pinned: &crate::PinnedOutsideDir,
    bytes: &[u8],
) -> Result<usize, ToolError> {
    use std::io::Write as _;
    let path = Path::new(raw);
    let parent = path
        .parent()
        .ok_or_else(|| ToolError::Denied(format!("path {raw} has no parent directory")))?;
    let name = path
        .file_name()
        .ok_or_else(|| ToolError::Invalid(format!("path {raw} has no file name")))?;
    let parent_meta = parent
        .metadata()
        .map_err(|err| ToolError::Denied(format!("path {raw} parent is not a directory: {err}")))?;
    if std::os::unix::fs::MetadataExt::dev(&parent_meta) != pinned.dev()
        || std::os::unix::fs::MetadataExt::ino(&parent_meta) != pinned.ino()
    {
        return Err(ToolError::Denied(format!(
            "path {raw} is outside the approved directory"
        )));
    }
    let live = pinned
        .dir()
        .dir_metadata()
        .map_err(|err| ToolError::Denied(format!("approved directory is unavailable: {err}")))?;
    if cap_fs_ext::MetadataExt::dev(&live) != pinned.dev()
        || cap_fs_ext::MetadataExt::ino(&live) != pinned.ino()
    {
        return Err(ToolError::Denied(format!(
            "path {raw} is outside the approved directory"
        )));
    }
    let mut opts = cap_std::fs::OpenOptions::new();
    opts.write(true).create(true);
    cap_fs_ext::OpenOptionsFollowExt::follow(&mut opts, cap_fs_ext::FollowSymlinks::No);
    let mut file = pinned
        .dir()
        .open_with(name, &opts)
        .map_err(|err| map_fs_err(raw, &err))?;
    let meta = file
        .metadata()
        .map_err(|err| ToolError::Failed(format!("write {raw}: {err}")))?;
    if cap_fs_ext::MetadataExt::nlink(&meta) > 1 {
        return Err(ToolError::Denied(format!(
            "path {raw} is a hard link and is not an approved target"
        )));
    }
    file.set_len(0)
        .map_err(|err| ToolError::Failed(format!("write {raw}: {err}")))?;
    file.write_all(bytes)
        .map_err(|err| ToolError::Failed(format!("write {raw}: {err}")))?;
    Ok(bytes.len())
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
    fn requires_outbound_policy(&self) -> bool {
        // The check is per-run: ordinary runs retain the approval flow, while
        // restricted runs are rejected at call time because the destination
        // of an arbitrary child process cannot be bounded.
        false
    }

    fn info(&self) -> ToolInfo {
        ToolInfo::new(
            "shell_exec",
            "builtin",
            "Execute a command as an argv array (no shell interpretation), \
             cwd inside the agent workspace.",
            RiskClass::Exec,
        )
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
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        // Child processes are not network-isolated by the host. Restricted
        // source policies therefore fail closed before spawn (including the
        // already-approved resume path). Ordinary runs have no source egress
        // resources and retain the pre-gate approval behavior.
        ctx.authorize_egress_if_restricted(
            EgressPurpose::ProcessExecution,
            EgressDestination::unknown(),
        )
        .await?;
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
        // FU-103/J-6: never hand the LLM's shell command the daemon's full
        // environment (every *_KEY/*_TOKEN/*_SECRET and A24_*/OMLX_* var it
        // holds) — only the fixed, minimal set a process needs to actually
        // run. See `crate::env_whitelist` for the full rationale.
        crate::env_whitelist::clear_and_whitelist(&mut cmd);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // kill_on_drop: a timeout or cancellation drops the child future —
            // the process must die with it, never linger
            .kill_on_drop(true);
        if let Some(authority) = ctx.workspace_authority() {
            authority
                .configure_command_cwd(&mut cmd)
                .await
                .map_err(|e| ToolError::Denied(format!("workspace authority unavailable: {e}")))?;
        } else {
            cmd.current_dir(&self.workdir);
        }
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
        ToolContext::legacy("run_test", None, None, "tc_test")
    }

    fn sinput(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
            .collect()
    }

    #[tokio::test]
    async fn an_approved_outside_directory_can_be_written_and_a_sibling_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("ws");
        let outside = dir.path().join("outside");
        let sibling = dir.path().join("sibling");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let tool = FsWriteTool::new(vec![workspace]);
        let file = outside.join("a.txt");
        let denied = tool
            .call(
                &ctx(),
                &sinput(&[("path", file.to_str().unwrap()), ("content", "x")]),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(denied, ToolError::Denied(_)), "{denied:?}");

        let approved = ctx().with_approved_outside_directory(
            pin_outside_dir(outside.canonicalize().unwrap()).unwrap(),
        );
        let out = tool
            .call(
                &approved,
                &sinput(&[("path", file.to_str().unwrap()), ("content", "ok")]),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(out.contains("wrote"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ok");

        let other = sibling.join("b.txt");
        let wrong = tool
            .call(
                &approved,
                &sinput(&[("path", other.to_str().unwrap()), ("content", "no")]),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(wrong, ToolError::Denied(_)), "{wrong:?}");
        assert!(!other.exists());
    }

    /// The name `approved_parent` resolved is opened again without following
    /// any symlink: a level swapped for one after it was resolved, last or
    /// in between, is not pinned (#1254 Codex C1); a real directory is.
    #[cfg(unix)]
    #[test]
    fn a_name_that_now_goes_through_a_symlink_is_not_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (safe, secret) = (root.join("safe"), root.join("secret"));
        std::fs::create_dir_all(safe.join("sub")).unwrap();
        std::fs::create_dir_all(secret.join("sub")).unwrap();
        let pinned = pin_outside_dir(safe.clone()).unwrap();
        let meta = std::fs::metadata(&safe).unwrap();
        assert_eq!(
            (pinned.dev(), pinned.ino()),
            (
                std::os::unix::fs::MetadataExt::dev(&meta),
                std::os::unix::fs::MetadataExt::ino(&meta)
            )
        );
        // `safe` resolved, then swapped for a symlink to `secret`.
        std::fs::rename(&safe, root.join("safe-moved")).unwrap();
        std::os::unix::fs::symlink(&secret, &safe).unwrap();
        assert!(pin_outside_dir(safe.clone()).is_err(), "the last level");
        assert!(
            pin_outside_dir(safe.join("sub")).is_err(),
            "a level in between"
        );
        assert!(pin_outside_dir(PathBuf::from("relative")).is_err());
    }

    /// The walk used where `O_NOFOLLOW_ANY` is not honoured refuses a
    /// symlink at any level as well (#1254 Codex round 3).
    #[cfg(unix)]
    #[test]
    fn the_walk_refuses_a_symlink_at_any_level() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (real, link) = (root.join("real"), root.join("link"));
        std::fs::create_dir_all(real.join("sub")).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(walk_without_following(&real.join("sub")).is_ok());
        assert!(walk_without_following(&link).is_err(), "the last level");
        assert!(
            walk_without_following(&link.join("sub")).is_err(),
            "a level in between"
        );
    }

    /// An ancestor the user may search but not read does not stop the pin,
    /// as it did not stop the plain open before (#1254 Codex round 2).
    #[cfg(unix)]
    #[test]
    fn an_ancestor_that_can_only_be_searched_still_pins() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (shared, target) = (root.join("shared"), root.join("shared/target"));
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o311)).unwrap();
        let pinned = pin_outside_dir(target.clone());
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        pinned.unwrap();
    }

    /// The durable-resume path captured no outside directory: the call is
    /// asked again, not run and not refused (#1254 Codex C2). Inside the
    /// workspace it runs as approved.
    #[tokio::test]
    async fn a_resumed_outside_write_is_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let (ws, outside) = (dir.path().join("ws"), dir.path().join("outside"));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let reg = ToolRegistry::builtin(ws.clone());
        let file = outside.join("a.txt");
        let input = sinput(&[("path", file.to_str().unwrap()), ("content", "x")]);
        let err = reg
            .execute_preapproved("fs_write", &ctx(), &input, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ApprovalStale), "{err:?}");
        assert!(!file.exists());
        let inside = ws.join("b.txt");
        let input = sinput(&[("path", inside.to_str().unwrap()), ("content", "y")]);
        reg.execute_preapproved("fs_write", &ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&inside).unwrap(), "y");
    }

    struct SwapGate {
        link: PathBuf,
        secret: PathBuf,
    }

    #[async_trait::async_trait]
    impl crate::ApprovalGate for SwapGate {
        async fn check(
            &self,
            _: &agent24_protocol::ToolInfo,
            _: &ToolContext,
            _: &Map<String, Value>,
            _: Option<&str>,
            _: &CancellationToken,
        ) -> crate::GateDecision {
            let _ = std::fs::remove_file(&self.link);
            std::os::unix::fs::symlink(&self.secret, &self.link).unwrap();
            crate::GateDecision::Allow
        }
    }

    #[tokio::test]
    async fn replacing_the_approved_directory_name_does_not_redirect_the_write() {
        let dir = tempfile::tempdir().unwrap();
        let safe = dir.path().join("safe");
        let secret = dir.path().join("secret");
        std::fs::create_dir_all(&safe).unwrap();
        std::fs::create_dir_all(&secret).unwrap();
        let pinned = pin_outside_dir(safe.canonicalize().unwrap()).unwrap();
        let moved = dir.path().join("safe-moved");
        std::fs::rename(&safe, &moved).unwrap();
        std::os::unix::fs::symlink(&secret, &safe).unwrap();
        let tool = FsWriteTool::new(vec![dir.path().join("ws")]);
        let file = safe.join("a.txt");
        let err = tool
            .call(
                &ctx().with_approved_outside_directory(pinned),
                &sinput(&[("path", file.to_str().unwrap()), ("content", "ok")]),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err:?}");
        assert!(!secret.join("a.txt").exists());
        assert!(!moved.join("a.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_swap_during_the_gate_does_not_write_the_new_target() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        let safe = dir.path().join("safe");
        let secret = dir.path().join("secret");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&safe).unwrap();
        std::fs::create_dir_all(&secret).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&safe, &link).unwrap();
        let file = link.join("a.txt");
        let reg = ToolRegistry::builtin(ws).with_gate(Arc::new(SwapGate {
            link: link.clone(),
            secret: secret.clone(),
        }));
        let err = reg
            .dispatch(
                "fs_write",
                &ctx(),
                &sinput(&[("path", file.to_str().unwrap()), ("content", "ok")]),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err:?}");
        assert!(!secret.join("a.txt").exists());
        assert!(!safe.join("a.txt").exists());
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
    async fn shell_exec_fails_closed_when_child_network_use_cannot_be_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ShellExecTool::new(dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert("argv".to_owned(), serde_json::json!(["/bin/echo", "safe"]));
        let restricted = ctx().with_egress_policy(
            <dyn agent24_domain::EgressGate>::deny_all(),
            vec![agent24_domain::EgressResource::local_only(
                "selected_material:private",
                "rev-1",
            )],
            0,
        );
        let err = tool
            .call(&restricted, &input, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");
    }

    #[tokio::test]
    async fn shell_exec_runs_for_ordinary_run_without_egress_policy() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child-ran");
        let tool = ShellExecTool::new(dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert(
            "argv".to_owned(),
            serde_json::json!(["/usr/bin/touch", marker]),
        );
        let out = tool
            .call(&ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert!(out.contains("\"exit_code\":0"), "{out}");
        assert!(marker.exists(), "ordinary shell command should spawn");
    }

    #[tokio::test]
    async fn shell_exec_runs_argv_without_shell_interpretation_for_ordinary_run() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ShellExecTool::new(dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert(
            "argv".to_owned(),
            serde_json::json!(["/usr/bin/printf", "[%s]", "$HOME", "two words", "a;b"]),
        );
        let out = tool
            .call(&ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["exit_code"], 0);
        // Each argv entry arrives as exactly one argument, unexpanded. Output
        // alone cannot tell direct exec from a shell with perfect quoting;
        // direct exec is `Command::new(program).args(args)` in `call`.
        assert_eq!(parsed["stdout"], "[$HOME][two words][a;b]");
    }

    /// FU-103/J-6, end-to-end through `ShellExecTool::call` (not just the
    /// `env_whitelist` unit — this proves the tool actually wires it in).
    /// Re-execs the test binary with a real ambient `secret_test_key` set on
    /// the CHILD process (via `Command::env` on the re-exec, which becomes
    /// genuine environment for that process — not just a Command-builder
    /// override), then asserts from the outside that the re-exec'd probe
    /// (which runs `shell_exec` against `printenv`) passed. `std::env::set_var`
    /// is deliberately avoided: edition 2024 forbids it outside `unsafe`
    /// (workspace-wide policy — see `agent24-models::router` tests), and it
    /// would mutate this whole test binary's process env, racing every other
    /// test in the binary.
    #[tokio::test]
    async fn shell_exec_does_not_leak_ambient_daemon_secrets() {
        let exe = std::env::current_exe().unwrap();
        let status = std::process::Command::new(exe)
            .args([
                "--exact",
                "local::tests::shell_exec_probe_ambient_secret_is_invisible_to_child",
                "--ignored",
                "--nocapture",
            ])
            .env("secret_test_key", "leaked-if-this-appears-in-child-stdout")
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Only runs re-exec'd by the test above, where `secret_test_key` is real
    /// ambient environment for THIS process.
    #[tokio::test]
    #[ignore]
    async fn shell_exec_probe_ambient_secret_is_invisible_to_child() {
        assert_eq!(
            std::env::var("secret_test_key").as_deref(),
            Ok("leaked-if-this-appears-in-child-stdout"),
            "test setup broken: the ambient var must actually be set here"
        );
        let dir = tempfile::tempdir().unwrap();
        let tool = ShellExecTool::new(dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert("argv".to_owned(), serde_json::json!(["printenv"]));
        let output = tool
            .call(&ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert!(!output.contains("leaked-if-this-appears-in-child-stdout"));
        // An empty child environment would also pass the line above.
        assert!(
            output.contains("PATH="),
            "PATH must reach the child: {output}"
        );
    }

    #[tokio::test]
    async fn shell_exec_reports_nonzero_exit_for_ordinary_run() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ShellExecTool::new(dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert("argv".to_owned(), serde_json::json!(["/usr/bin/false"]));
        let output = tool
            .call(&ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert!(output.contains("\"exit_code\":1"), "{output}");
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
            fn requires_outbound_policy(&self) -> bool {
                false
            }

            fn info(&self) -> ToolInfo {
                // Re-declared as Read purely to skip the gate for this budget
                // test; the real ShellExecTool is Exec and always gated.
                let base = self.0.info();
                ToolInfo::new(base.name, base.source, base.description, RiskClass::Read)
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

    #[tokio::test]
    async fn ordinary_shell_exec_runs_after_approval_without_egress_gate() {
        struct AllowApproval;
        #[async_trait]
        impl crate::ApprovalGate for AllowApproval {
            async fn check(
                &self,
                _info: &ToolInfo,
                _ctx: &ToolContext,
                _input: &Map<String, Value>,
                _standing_target: Option<&str>,
                _cancel: &CancellationToken,
            ) -> crate::GateDecision {
                crate::GateDecision::Allow
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new()
            .with_gate(Arc::new(AllowApproval))
            .with(Arc::new(ShellExecTool::new(dir.path().to_path_buf())));
        let input = serde_json::json!({"argv":["/bin/echo", "ordinary"]})
            .as_object()
            .cloned()
            .unwrap();
        let output = registry
            .dispatch("shell_exec", &ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert!(output.contains("ordinary"));
    }

    #[tokio::test]
    async fn local_only_shell_exec_is_denied_even_by_permissive_egress_gate() {
        struct AllowEgress;
        #[async_trait]
        impl agent24_domain::EgressGate for AllowEgress {
            async fn check(
                &self,
                _request: &agent24_domain::EgressRequest,
            ) -> Result<(), agent24_domain::EgressDecision> {
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let tool = ShellExecTool::new(dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert(
            "argv".to_owned(),
            serde_json::json!(["/bin/echo", "sensitive"]),
        );
        let restricted = ctx().with_egress_policy(
            Arc::new(AllowEgress),
            vec![agent24_domain::EgressResource::local_only(
                "selected_material:secret",
                "rev-1",
            )],
            0,
        );
        let err = tool
            .call(&restricted, &input, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");
    }

    #[tokio::test]
    async fn shell_exec_is_denied_when_source_policy_lookup_failed() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ShellExecTool::new(dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert(
            "argv".to_owned(),
            serde_json::json!(["/bin/echo", "unknown"]),
        );
        let unavailable = ctx().with_egress_policy_unavailable();
        let err = tool
            .call(&unavailable, &input, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");
    }
}
