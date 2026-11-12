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

/// Create `dev/` at 0700 and replace the token file at 0600.
///
/// The bytes land in a temporary file in that directory, then `rename`
/// publishes them. A failure deletes the temporary file and does not include
/// the bearer. An empty bearer writes nothing, including the directory.
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
    std::fs::create_dir_all(&dev).map_err(|e| format!("creating {}: {e}", dev.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dev, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("securing {}: {e}", dev.display()))?;
    }
    let dest = bridge_token_path(root);
    let tmp = dev.join(format!(".wechat-bridge.token.{}.tmp", std::process::id()));
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
            options.mode(0o600);
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
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("writing {}: {e}", dest.display()));
    }
    Ok(dest)
}

/// The id recorded when the file was written. Missing file or missing id
/// is an error and does not contact the daemon.
pub fn read_bridge_capability_id(root: &Path) -> Result<String, String> {
    let path = bridge_token_path(root);
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading {}: {err}", path.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| format!("{} 不是 JSON，无法撤销", path.display()))?;
    let id = parsed["capability_id"].as_str().unwrap_or("");
    if !capability_id_ok(id) {
        return Err(format!("{} 没有 capability_id，无法撤销", path.display()));
    }
    Ok(id.to_owned())
}

pub fn remove_bridge_token_at(root: &Path) -> Result<(), String> {
    let path = bridge_token_path(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("removing {}: {err}", path.display())),
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
}
