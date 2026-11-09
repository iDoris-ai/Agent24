//! Host-owned trust policy. Package contents never select the trust store.
use agent24_os_proc::{DEVELOPER_ID_TEAM_ID, valid_team_id};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const FILE_NAME: &str = "os-trusted-teams.json";
pub fn default_path() -> Option<PathBuf> {
    agent24_protocol::state_file::state_dir().map(|dir| dir.join(FILE_NAME))
}
pub fn builtin() -> BTreeSet<String> {
    BTreeSet::from([DEVELOPER_ID_TEAM_ID.to_owned()])
}
/// Missing or damaged policy never grants trust to an external team.
/// A symlink, a non-regular file, a file owned by someone else, or a file
/// group/other can write is the same as a missing file: built-in team only.
pub fn load(path: &Path) -> BTreeSet<String> {
    let result = (|| -> io::Result<BTreeSet<String>> {
        let bytes = read_trust_file(path)?;
        let teams: BTreeSet<String> = serde_json::from_slice(&bytes)?;
        if !teams.iter().all(|team| valid_team_id(team)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Team ID",
            ));
        }
        Ok(teams)
    })();
    let mut teams = match result {
        Ok(teams) => teams,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), error = %err, "cannot read OS trusted teams; using built-in team only");
            }
            BTreeSet::new()
        }
    };
    teams.extend(builtin());
    teams
}

/// One fd: nofollow open, then fstat, then a bounded read of that same fd.
fn read_trust_file(path: &Path) -> io::Result<Vec<u8>> {
    const MAX_BYTES: u64 = 64 * 1024;
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;
        let file = fs::File::from(rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )?);
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trusted teams file is not a regular file",
            ));
        }
        if meta.uid() != rustix::process::getuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trusted teams file is not owned by the current user",
            ));
        }
        // Write side creates the file mode 0600. Group/other write is enough
        // to ignore the file; owner read can be wider than 0600 (0640 is fine).
        if meta.mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trusted teams file is writable by group or other",
            ));
        }
        if meta.len() > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trusted teams file is too large",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let bytes = fs::read(path)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trusted teams file is too large",
            ));
        }
        Ok(bytes)
    }
}
/// Called by the daemon. Serialize read-modify-write across processes.
pub fn update(path: &Path, team: &str, trusted: bool) -> Result<(), String> {
    if !valid_team_id(team) {
        return Err("Team ID must be 10 uppercase letters or digits".into());
    }
    if !trusted && team == DEVELOPER_ID_TEAM_ID {
        return Err("built-in team cannot be removed".into());
    }
    write_update(path, team, trusted).map_err(|err| err.to_string())
}
fn write_update(path: &Path, team: &str, trusted: bool) -> io::Result<()> {
    use fs2::FileExt;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("missing trust directory"))?;
    fs::create_dir_all(dir)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let lock = options.open(dir.join("os-trusted-teams.lock"))?;
    lock.lock_exclusive()?;
    let mut teams = load(path);
    if trusted {
        teams.insert(team.to_owned());
    } else {
        teams.remove(team);
    }
    let bytes = serde_json::to_vec_pretty(&teams)?;
    let tmp = dir.join(format!(".os-trusted-teams.{}.tmp", std::process::id()));
    if fs::symlink_metadata(&tmp).is_ok() {
        fs::remove_file(&tmp)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn trust_roundtrip_builtin_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(load(&path), builtin());
        update(&path, "ABCDE12345", true).unwrap();
        assert!(load(&path).contains("ABCDE12345"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        update(&path, "ABCDE12345", false).unwrap();
        assert_eq!(load(&path), builtin());
        assert!(update(&path, DEVELOPER_ID_TEAM_ID, false).is_err());
        assert!(update(&path, "../bad", true).is_err());
        fs::write(&path, b"[\"ABCDE12345\", bad]").unwrap();
        assert_eq!(load(&path), builtin());
    }

    /// Replacing the nofollow/owner/mode read with `fs::read` follows the
    /// link and accepts the mode 0666 file, so both assertions go red.
    #[test]
    fn symlink_writable_and_nonregular_trust_files_stay_builtin_only() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        fs::write(&real, br#"["ABCDE12345"]"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let link = dir.path().join("link.json");
            symlink(&real, &link).unwrap();
            assert_eq!(load(&link), builtin(), "a symlink must not be followed");

            let loose = dir.path().join(FILE_NAME);
            update(&loose, "ABCDE12345", true).unwrap();
            assert!(load(&loose).contains("ABCDE12345"));
            let mut perms = fs::metadata(&loose).unwrap().permissions();
            perms.set_mode(0o666);
            fs::set_permissions(&loose, perms).unwrap();
            assert_eq!(
                load(&loose),
                builtin(),
                "group/other write must drop external teams"
            );

            let folder = dir.path().join("not-a-file");
            fs::create_dir(&folder).unwrap();
            assert_eq!(load(&folder), builtin(), "a directory is not a trust file");
        }
    }
}
