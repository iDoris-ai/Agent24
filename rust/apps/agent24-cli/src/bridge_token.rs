//! `agent24 dev bridge-token` writes the dev-bridge credential.
//!
//! The file lives under the shared data directory (`data_dir()/dev/`), the
//! same root the daemon and `@agent24/data-dir` use. It is not under the
//! component install root. The bearer is only in that file: not in the
//! process arguments, the environment, or this command's stdout.

use std::io::Write;
use std::path::{Path, PathBuf};

/// `dev/wechat-bridge.token` under the data directory.
pub fn bridge_token_path(root: &Path) -> PathBuf {
    root.join("dev").join("wechat-bridge.token")
}

/// Two lines, and nothing else. The bearer is not one of them.
pub fn stdout_lines(path: &Path, expires_at: u64) -> String {
    format!("{}\nexpires_at: {expires_at}\n", path.display())
}

pub fn capability_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// `dev/` must be a real directory, not a symlink. It is opened
/// `O_NOFOLLOW` and set to 0700 through that handle.
fn ensure_private_dev(dev: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dev) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!("{} 是符号链接，拒绝写入", dev.display()));
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(format!("{} 不是目录，拒绝写入", dev.display()));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(dev).map_err(|e| format!("creating {}: {e}", dev.display()))?;
        }
        Err(err) => return Err(format!("reading {}: {err}", dev.display())),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let flags = rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC;
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags.bits().cast_signed())
            .open(dev)
            .map_err(|e| format!("{} 无法以 O_NOFOLLOW 打开: {e}", dev.display()))?;
        handle
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("securing {}: {e}", dev.display()))?;
    }
    Ok(())
}

/// Create `dev/` at 0700 and replace the token file at 0600.
///
/// The bytes land in a temporary file in that directory, opened
/// `O_NOFOLLOW`, then `rename` publishes them. A symlink `dev/` is refused
/// and nothing is written through it. A failure deletes the temporary file
/// and does not include the bearer. An empty bearer writes nothing,
/// including the directory.
pub fn write_bridge_token_at(
    root: &Path,
    token: &str,
    expires_at: u64,
    capability_id: &str,
) -> Result<PathBuf, String> {
    if token.is_empty() {
        return Err("bridge token is empty".to_owned());
    }
    if !capability_id_ok(capability_id) {
        return Err("bridge capability id is empty".to_owned());
    }
    let dev = root.join("dev");
    ensure_private_dev(&dev)?;
    let dest = bridge_token_path(root);
    let tmp = dev.join(format!(".wechat-bridge.token.{}.tmp", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_file(&tmp);
    let body = serde_json::to_vec(&serde_json::json!({
        "token": token,
        "expires_at": expires_at,
        "capability_id": capability_id,
    }))
    .map_err(|_| "encoding bridge token file failed".to_owned())?;
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC;
            options.mode(0o600);
            options.custom_flags(flags.bits().cast_signed());
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.sync_all()?;
        std::fs::rename(&tmp, &dest)?;
        Ok::<(), std::io::Error>(())
    })();
    // The rename is already visible. A directory fsync failure is logged
    // and does not fail the write.
    if written.is_ok()
        && let Err(err) = std::fs::File::open(&dev).and_then(|dir| dir.sync_all())
    {
        eprintln!("微信桥令牌目录 fsync 失败（{}）: {err}", dev.display());
    }
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("writing {}: {e}", dest.display()));
    }
    Ok(dest)
}

/// Read the token file without following a symlink.
fn read_token_text(path: &Path) -> Result<Option<String>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let flags = rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK;
        options.custom_flags(flags.bits().cast_signed());
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("reading {}: {err}", path.display())),
    };
    // fstat the fd we opened. A FIFO opened O_NONBLOCK is not a regular file.
    let meta = file
        .metadata()
        .map_err(|err| format!("reading {}: {err}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} 不是普通文件，拒绝读取", path.display()));
    }
    use std::io::Read;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|err| format!("reading {}: {err}", path.display()))?;
    Ok(Some(text))
}

fn capability_id_in(path: &Path, text: &str) -> Result<String, String> {
    let parsed: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| format!("{} 不是 JSON，无法撤销", path.display()))?;
    let id = parsed["capability_id"].as_str().unwrap_or("");
    if !capability_id_ok(id) {
        return Err(format!("{} 没有 capability_id，无法撤销", path.display()));
    }
    Ok(id.to_owned())
}

/// A capability id still inside its recorded lifetime.
///
/// Missing file, unreadable file, bad id, or `expires_at` already past:
/// nothing to revoke. A file with a well-formed id and no `expires_at` is
/// treated as still live, so a re-issue still tries to revoke it.
pub fn live_bridge_id(root: &Path, now: u64) -> Option<String> {
    let path = bridge_token_path(root);
    let text = read_token_text(&path).ok().flatten()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    let id = parsed["capability_id"].as_str().unwrap_or("");
    if !capability_id_ok(id) {
        return None;
    }
    if parsed["expires_at"].as_u64().is_some_and(|exp| exp <= now) {
        return None;
    }
    Some(id.to_owned())
}

/// The id recorded when the file was written. Missing file or missing id
/// is an error and does not contact the daemon. A symlink is not followed.
pub fn read_bridge_capability_id(root: &Path) -> Result<String, String> {
    let path = bridge_token_path(root);
    let text =
        read_token_text(&path)?.ok_or_else(|| format!("reading {}: not found", path.display()))?;
    capability_id_in(&path, &text)
}

pub fn remove_bridge_token_at(root: &Path) -> Result<(), String> {
    let path = bridge_token_path(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("removing {}: {err}", path.display())),
    }
}

/// Delete the token file only when it still names `capability_id`.
///
/// The file is read again immediately before the delete. A different id is
/// left in place. A missing file is already gone.
pub fn remove_bridge_token_if_id(root: &Path, capability_id: &str) -> Result<(), String> {
    let path = bridge_token_path(root);
    let Some(text) = read_token_text(&path)? else {
        return Ok(());
    };
    let current = capability_id_in(&path, &text)?;
    if current != capability_id {
        return Err(format!(
            "{} 的 capability_id 是 {current}，与本次撤销的 {capability_id} 不一致，文件保留。",
            path.display()
        ));
    }
    remove_bridge_token_at(root)
}

/// Shell revoke of an id this daemon process does not have (it restarted).
pub const DAEMON_RESTART_REVOKED: &str = "令牌已随 daemon 重启失效，已删除本地文件";

/// `true` when the daemon answered that it has no such capability id.
pub fn daemon_lost_bridge_token(status: u16, body: &serde_json::Value) -> bool {
    status == 404 && body["error"]["code"].as_str() == Some("bridge_token_unknown")
}

/// Delete the local file after the daemon reported the id is gone.
/// The `Err` is the sentence the command shows. Other statuses return `None`
/// and leave the file in place.
pub fn revoke_reply_when_daemon_lost_the_id(
    root: &Path,
    capability_id: &str,
    status: u16,
    body: &serde_json::Value,
) -> Option<Result<(), String>> {
    if !daemon_lost_bridge_token(status, body) {
        return None;
    }
    match remove_bridge_token_if_id(root, capability_id) {
        Ok(()) => Some(Err(DAEMON_RESTART_REVOKED.to_owned())),
        Err(err) => Some(Err(format!(
            "令牌已随 daemon 重启失效。请删除 {}。{err}",
            bridge_token_path(root).display()
        ))),
    }
}

/// HTTP failure text for a bridge issue or revoke that is not the closed gate.
pub fn http_failure(status: u16, body: &serde_json::Value) -> String {
    let code = body["error"]["code"].as_str().unwrap_or("request_failed");
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or("bridge token request failed");
    format!("{code}: {message} (HTTP {status})")
}

/// `A24_DEV_TOOL=1`, the same switch the wechat bridge and the daemon use.
pub fn dev_tool_enabled() -> bool {
    std::env::var("A24_DEV_TOOL").ok().as_deref() == Some("1")
}

pub const DEV_TOOL_REQUIRED: &str =
    "微信桥令牌只在开发时签发。请设置 A24_DEV_TOOL=1 后再运行 agent24 dev bridge-token。";

/// Daemon answered 404 `not_found` on the issue route. No elevation was asked.
pub const DAEMON_DEV_TOOL_REQUIRED: &str = "运行中的 agent24d 没有设置 A24_DEV_TOOL=1，开发签发路由不存在。请先带上这个变量启动 daemon，再运行 agent24 dev bridge-token。这次没有请求桌面确认。";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    #[cfg(unix)]
    fn writes_0600_file_under_0700_dev_and_stdout_omits_the_bearer() {
        let root = tempfile::tempdir().unwrap();
        let secret = "bridge-secret-not-for-stdout";
        let path = write_bridge_token_at(root.path(), secret, 1_700_000_000, "abc123").unwrap();
        assert_eq!(path, bridge_token_path(root.path()));
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(mode(&path), 0o600);
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["token"], secret);
        assert_eq!(parsed["expires_at"], 1_700_000_000);
        assert_eq!(parsed["capability_id"], "abc123");
        assert_eq!(read_bridge_capability_id(root.path()).unwrap(), "abc123");
        assert_eq!(live_bridge_id(root.path(), 1).as_deref(), Some("abc123"));
        assert_eq!(live_bridge_id(root.path(), 1_700_000_000).as_deref(), None);
        let printed = stdout_lines(&path, 1_700_000_000);
        assert!(!printed.contains(secret));
        assert!(printed.contains(&path.display().to_string()));
        assert!(printed.contains("expires_at: 1700000000"));
        // A second issue replaces the file and keeps the mode.
        let again =
            write_bridge_token_at(root.path(), "next-secret", 1_700_000_100, "def456").unwrap();
        assert_eq!(again, path);
        assert_eq!(mode(&again), 0o600);
        assert_eq!(mode(again.parent().unwrap()), 0o700);
        assert_eq!(read_bridge_capability_id(root.path()).unwrap(), "def456");
    }

    /// An empty bearer is not written. Deleting the empty check in
    /// `write_bridge_token_at` turns this red.
    #[test]
    fn empty_token_is_not_written() {
        let root = tempfile::tempdir().unwrap();
        let err = write_bridge_token_at(root.path(), "", 1, "abc123").unwrap_err();
        assert!(err.contains("empty"), "{err}");
        assert!(!bridge_token_path(root.path()).exists());
        assert!(!root.path().join("dev").exists());
    }

    /// `dev` as a symlink is not a place to write. Following it would drop
    /// the bearer in the link target. Dropping the symlink check turns this red.
    #[test]
    #[cfg(unix)]
    fn refuses_to_write_when_dev_is_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root.path().join("dev")).unwrap();
        let err = write_bridge_token_at(root.path(), "secret", 1, "abc123").unwrap_err();
        assert!(err.contains("符号链接"), "{err}");
        assert!(elsewhere.path().read_dir().unwrap().next().is_none());
        assert!(!bridge_token_path(root.path()).exists());
    }

    /// The file is reread before delete. A replaced id is kept.
    /// Skipping the compare deletes the new file and turns this red.
    #[test]
    fn revoke_delete_keeps_a_file_whose_id_changed() {
        let root = tempfile::tempdir().unwrap();
        write_bridge_token_at(root.path(), "secret", 1, "abc123").unwrap();
        write_bridge_token_at(root.path(), "next", 2, "def456").unwrap();
        let err = remove_bridge_token_if_id(root.path(), "abc123").unwrap_err();
        assert!(err.contains("不一致"), "{err}");
        assert!(bridge_token_path(root.path()).exists());
        assert_eq!(read_bridge_capability_id(root.path()).unwrap(), "def456");
        remove_bridge_token_if_id(root.path(), "def456").unwrap();
        assert!(!bridge_token_path(root.path()).exists());
    }

    /// A FIFO must not block the read, and must not be parsed as a token.
    /// Dropping `O_NONBLOCK` or the `is_file` check turns this red: the read
    /// either waits past the timeout or reports a JSON error.
    #[test]
    #[cfg(unix)]
    fn read_token_text_rejects_a_fifo_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("dev")).unwrap();
        let path = bridge_token_path(root.path());
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let root_buf = root.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_bridge_capability_id(&root_buf));
        });
        let err = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap_err();
        assert!(err.contains("普通文件"), "{err}");
        assert!(path.exists());
    }

    /// 404 `bridge_token_unknown` deletes the file and names the restart.
    /// Treating that status as a generic HTTP failure leaves the file and
    /// turns the assertion red.
    #[test]
    fn revoke_of_an_id_the_daemon_lost_deletes_the_local_file() {
        let root = tempfile::tempdir().unwrap();
        write_bridge_token_at(root.path(), "secret", 1, "abc123").unwrap();
        let body = serde_json::json!({"error": {"code": "bridge_token_unknown", "message": "x"}});
        let outcome = revoke_reply_when_daemon_lost_the_id(root.path(), "abc123", 404, &body)
            .unwrap()
            .unwrap_err();
        assert_eq!(outcome, DAEMON_RESTART_REVOKED);
        assert!(!bridge_token_path(root.path()).exists());
        write_bridge_token_at(root.path(), "secret", 1, "abc123").unwrap();
        let forbidden = serde_json::json!({"error": {"code": "not_elevated_operation"}});
        assert!(
            revoke_reply_when_daemon_lost_the_id(root.path(), "abc123", 403, &forbidden).is_none()
        );
        assert!(bridge_token_path(root.path()).exists());
    }
}
