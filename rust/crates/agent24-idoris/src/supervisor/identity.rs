//! Temporary local process identity adapter until agent24-os-proc is merged.
//! libproc provides macOS kernel start time (including microseconds), executable
//! and UID without ps or argv matching. Linux uses /proc with start ticks.

use super::{SupervisorError, Timings, signal_group};
use fs2::FileExt;
use nix::{
    sys::signal::Signal,
    unistd::{Pid, getpgid, getuid},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProcessRecord {
    pub pid: u32,
    pub start_time: u64,
    pub executable: PathBuf,
    pub uid: u32,
}
impl ProcessRecord {
    pub fn in_group(&self, group: Pid) -> bool {
        Self::capture(self.pid).is_ok_and(|actual| self.verify(&actual).is_ok())
            && getpgid(Some(Pid::from_raw(self.pid as i32))) == Ok(group)
    }

    pub fn group_members(group: Pid) -> io::Result<Vec<Self>> {
        #[cfg(target_os = "macos")]
        let pids = {
            use libproc::processes::{ProcFilter, pids_by_type};
            pids_by_type(ProcFilter::All)?
        };
        #[cfg(target_os = "linux")]
        let pids: Vec<u32> = fs::read_dir("/proc")?
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let pids: Vec<u32> = Vec::new();
        Ok(pids
            .into_iter()
            .filter_map(|pid| {
                if getpgid(Some(Pid::from_raw(pid as i32))) != Ok(group) {
                    return None;
                }
                let record = Self::capture(pid).ok()?;
                record.in_group(group).then_some(record)
            })
            .collect())
    }

    #[cfg(target_os = "macos")]
    pub fn capture(pid: u32) -> io::Result<Self> {
        use libproc::libproc::{
            bsd_info::BSDInfo,
            proc_pid::{pidinfo, pidpath},
        };
        let pid_i32 = i32::try_from(pid).map_err(|_| io::Error::other("invalid PID"))?;
        let info = pidinfo::<BSDInfo>(pid_i32, 0).map_err(io::Error::other)?;
        let executable = fs::canonicalize(pidpath(pid_i32).map_err(io::Error::other)?)?;
        let again = pidinfo::<BSDInfo>(pid_i32, 0).map_err(io::Error::other)?;
        if info.pbi_start_tvsec != again.pbi_start_tvsec
            || info.pbi_start_tvusec != again.pbi_start_tvusec
        {
            return Err(io::Error::other("process identity changed"));
        }
        Ok(Self {
            pid,
            start_time: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
            executable,
            uid: info.pbi_uid,
        })
    }
    #[cfg(target_os = "linux")]
    pub fn capture(pid: u32) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let root = PathBuf::from(format!("/proc/{pid}"));
        let stat = fs::read_to_string(root.join("stat"))?;
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .ok_or_else(|| io::Error::other("invalid proc stat"))?
            .1
            .split_whitespace()
            .collect();
        let start_time = fields
            .get(19)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| io::Error::other("invalid start time"))?;
        Ok(Self {
            pid,
            start_time,
            executable: fs::canonicalize(root.join("exe"))?,
            uid: fs::metadata(&root)?.uid(),
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn capture(_pid: u32) -> io::Result<Self> {
        Err(io::Error::other("unsupported process identity platform"))
    }

    pub fn verify(&self, actual: &Self) -> Result<(), SupervisorError> {
        if self == actual && self.uid == getuid().as_raw() && self.pid > 1 {
            Ok(())
        } else {
            Err(SupervisorError::InstanceConflict)
        }
    }
    fn verify_current(&self) -> Result<(), SupervisorError> {
        self.verify(&Self::capture(self.pid).map_err(|_| SupervisorError::InstanceConflict)?)?;
        let pid =
            Pid::from_raw(i32::try_from(self.pid).map_err(|_| SupervisorError::InstanceConflict)?);
        if getpgid(Some(pid)).map_err(|_| SupervisorError::InstanceConflict)? != pid {
            return Err(SupervisorError::InstanceConflict);
        }
        Ok(())
    }
    fn signal(&self, signal: Signal) -> Result<(), SupervisorError> {
        self.verify_current()?;
        signal_group(Pid::from_raw(self.pid as i32), signal)
            .map_err(|_| SupervisorError::InstanceConflict)?;
        Ok(())
    }
    async fn wait_for_exit(&self, deadline: Instant) -> Result<bool, SupervisorError> {
        loop {
            let alive = self.alive();
            if matches!(alive, Ok(false)) {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return alive.map(|_| false);
            }
            // macOS can return ESRCH from pidinfo for an unreaped zombie while
            // kill(pid, 0) still succeeds. Retry uncertainty without signalling;
            // only a verified live identity may authorize escalation.
            sleep(Duration::from_millis(20)).await;
        }
    }
    pub(super) fn alive(&self) -> Result<bool, SupervisorError> {
        if self.pid <= 1 || i32::try_from(self.pid).is_err() {
            return Err(SupervisorError::InstanceConflict);
        }
        #[cfg(target_os = "macos")]
        {
            use libproc::libproc::{bsd_info::BSDInfo, proc_pid::pidinfo};
            match pidinfo::<BSDInfo>(self.pid as i32, 0) {
                Ok(info) => {
                    if info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec != self.start_time {
                        return Ok(false);
                    }
                    if info.pbi_uid != self.uid {
                        return Err(SupervisorError::InstanceConflict);
                    }
                    Ok(info.pbi_status != 5)
                }
                Err(_) => match nix::sys::signal::kill(Pid::from_raw(self.pid as i32), None) {
                    Err(nix::errno::Errno::ESRCH) => Ok(false),
                    _ => Err(SupervisorError::InstanceConflict),
                },
            }
        }
        #[cfg(target_os = "linux")]
        {
            match fs::read_to_string(format!("/proc/{}/stat", self.pid)) {
                Ok(stat) => {
                    let fields: Vec<_> = stat
                        .rsplit_once(')')
                        .ok_or(SupervisorError::InstanceConflict)?
                        .1
                        .split_whitespace()
                        .collect();
                    let start: u64 = fields
                        .get(19)
                        .and_then(|v| v.parse().ok())
                        .ok_or(SupervisorError::InstanceConflict)?;
                    if start != self.start_time {
                        return Ok(false);
                    }
                    use std::os::unix::fs::MetadataExt;
                    if fs::metadata(format!("/proc/{}", self.pid))?.uid() != self.uid {
                        return Err(SupervisorError::InstanceConflict);
                    }
                    Ok(fields.first().is_some_and(|state| *state != "Z"))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(_) => Err(SupervisorError::InstanceConflict),
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(SupervisorError::InstanceConflict)
        }
    }
}

pub(super) fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::other("private directory is a symlink"));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}
pub(super) fn private_open(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

pub(super) struct Lease {
    _lock: File,
    record: PathBuf,
}
impl Drop for Lease {
    fn drop(&mut self) {
        // A concurrent fork may retain the open file description until exec.
        // Release ownership now instead of waiting for its CLOEXEC close.
        let _ = fs2::FileExt::unlock(&self._lock);
    }
}
impl Lease {
    pub fn acquire(state: &Path) -> Result<Self, SupervisorError> {
        private_dir(state)?;
        let lock = private_open(&state.join("managed-process.lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| SupervisorError::InstanceConflict)?;
        Ok(Self {
            _lock: lock,
            record: state.join("managed-process.json"),
        })
    }
    pub fn write(&self, record: &ProcessRecord) -> io::Result<()> {
        let parent = self
            .record
            .parent()
            .ok_or_else(|| io::Error::other("no record directory"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer(&mut temporary, record)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&self.record)
            .map_err(|error| error.error)?;
        Ok(())
    }
    pub fn clear(&self) -> io::Result<()> {
        match fs::remove_file(&self.record) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    pub async fn recover(
        &self,
        timings: &Timings,
        shutdown: &CancellationToken,
        executable: &Path,
    ) -> Result<(), SupervisorError> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&self.record)
        {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            other => other.map_err(|_| SupervisorError::InstanceConflict)?,
        };
        let mut body = Vec::new();
        file.take(4097)
            .read_to_end(&mut body)
            .map_err(|_| SupervisorError::InstanceConflict)?;
        if body.len() > 4096 {
            return Err(SupervisorError::InstanceConflict);
        }
        let record: ProcessRecord =
            serde_json::from_slice(&body).map_err(|_| SupervisorError::InstanceConflict)?;
        // ESRCH or a different start time means the recorded process is gone.
        // Never signal the new owner of a recycled PID, even if its path matches.
        if !record.alive()? {
            self.clear()?;
            return Ok(());
        }
        if record.executable != fs::canonicalize(executable)? {
            return Err(SupervisorError::InstanceConflict);
        }
        if shutdown.is_cancelled() {
            return Err(SupervisorError::Stopped);
        }
        record.signal(Signal::SIGTERM)?;
        let deadline = Instant::now() + timings.terminate;
        if !record.wait_for_exit(deadline).await? {
            record.signal(Signal::SIGKILL)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            if !record.wait_for_exit(deadline).await? {
                return Err(SupervisorError::InstanceConflict);
            }
        }
        let group_deadline = Instant::now() + Duration::from_secs(5);
        while super::group_exists(Pid::from_raw(record.pid as i32))
            .map_err(|_| SupervisorError::InstanceConflict)?
        {
            // A zombie leader retains its group until its parent reaps it.
            // Allow reaping to complete without sending any further signals.
            if Instant::now() >= group_deadline {
                // Unknown descendants cannot authorize group signalling.
                return Err(SupervisorError::InstanceConflict);
            }
            sleep(Duration::from_millis(20)).await;
        }
        self.clear()?;
        Ok(())
    }
}
use std::time::Duration;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn dropping_lease_unlocks_inherited_descriptors() {
        let directory = tempfile::tempdir().unwrap();
        let lease = Lease::acquire(directory.path()).unwrap();
        // dup retains the same open file description, just as a concurrent
        // fork does before exec closes its inherited CLOEXEC descriptors.
        let inherited = lease._lock.try_clone().unwrap();
        drop(lease);
        let next = Lease::acquire(directory.path());
        drop(inherited);
        assert!(next.is_ok(), "an inherited fd retained a released lease");
    }
}
