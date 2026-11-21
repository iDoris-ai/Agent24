//! Launcher discovery and its private on-disk instance proof.
use super::*;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

#[derive(serde::Serialize)]
struct LauncherFrame<'a> {
    v: u8,
    admin_token: &'a str,
    discovery_path: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    register_socket: Option<&'a Path>,
    orphan_timeout_s: u32,
}

pub(super) fn frame(
    token: &str,
    path: &Path,
    register_socket: Option<&Path>,
) -> Result<Zeroizing<String>, SupervisorError> {
    let frame = LauncherFrame {
        v: 1,
        admin_token: token,
        discovery_path: path,
        register_socket,
        orphan_timeout_s: if register_socket.is_some() { 900 } else { 0 },
    };
    let value =
        Zeroizing::new(serde_json::to_string(&frame).map_err(|_| SupervisorError::Configuration)?);
    if value.len() + 1 > 4096 {
        return Err(SupervisorError::Configuration);
    }
    Ok(value)
}

pub(super) fn path(data: &DataPaths) -> Result<PathBuf, SupervisorError> {
    let run = data
        .state
        .parent()
        .ok_or(SupervisorError::Configuration)?
        .join("run");
    fs::create_dir_all(&run)?;
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700))?;
    Ok(fs::canonicalize(run)?.join("idoris.json"))
}

#[derive(Deserialize)]
pub(super) struct Discovery {
    pub schema: u32,
    pub service: String,
    pub pid: u32,
    pub instance_nonce: Zeroizing<String>,
    pub instance_id: String,
    pub data_port: u16,
    pub admin_port: u16,
    #[serde(default)]
    pub started_at_ms: u64,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub contract_version: String,
    #[serde(default)]
    pub launch: Option<serde_json::Value>,
}

#[cfg(test)]
pub(super) fn read(path: &Path, pid: u32, nonce: &str) -> Result<Discovery, SupervisorError> {
    let record = read_any(path)?;
    if record.pid != pid || record.instance_nonce.as_str() != nonce {
        return Err(SupervisorError::StaleDiscovery);
    }
    Ok(record)
}

pub(super) fn read_any(path: &Path) -> Result<Discovery, SupervisorError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != nix::unistd::geteuid().as_raw()
    {
        return Err(SupervisorError::Contract);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(SupervisorError::Contract);
    }
    let record: Discovery =
        serde_json::from_slice(&bytes).map_err(|_| SupervisorError::Contract)?;
    if record.schema != 1
        || record.service != "idoris"
        || record.instance_id.is_empty()
        || record.data_port == 0
        || record.admin_port == 0
        || record.data_port == record.admin_port
    {
        return Err(SupervisorError::Contract);
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn frame_and_discovery_identity_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(temp.path()).unwrap().join("idoris.json");
        let token = "a".repeat(64);
        let nonce = "b".repeat(64);
        let value: serde_json::Value =
            serde_json::from_str(&frame(&token, &path, Some(Path::new("/register.sock"))).unwrap())
                .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"v":1,"admin_token":token,"discovery_path":path,"register_socket":"/register.sock","orphan_timeout_s":900})
        );
        let fallback: serde_json::Value =
            serde_json::from_str(&frame(&token, &path, None).unwrap()).unwrap();
        assert!(fallback.get("register_socket").is_none());
        assert_eq!(fallback["orphan_timeout_s"], 0);
        fs::write(&path, serde_json::json!({"schema":1,"service":"idoris","pid":42,"instance_nonce":nonce,"instance_id":"s1","data_port":1234,"admin_port":1235}).to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read(&path, 42, &nonce).is_ok());
        assert!(read(&path, 43, &nonce).is_err());
        assert!(read(&path, 42, &token).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&path, 42, &nonce).is_err());
    }
}
