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
            // A dead leader does not prove a dead group: a TERM-ignoring
            // worker keeps the PGID. Kernels never hand out a PID that still
            // names a process group, so a reused PID proves the group emptied.
            // Otherwise wait briefly for reaping, then keep the record.
            let reused = ProcessRecord::capture(record.pid)
                .is_ok_and(|current| current.start_time != record.start_time);
            if !reused {
                wait_group_gone(record.pid, timings.terminate.min(Duration::from_secs(5))).await?;
            }
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
        // A zombie leader retains its group until its parent reaps it.
        // Allow reaping to complete without sending any further signals.
        wait_group_gone(record.pid, Duration::from_secs(5)).await?;
        self.clear()?;
        Ok(())
    }
}

/// Unknown descendants cannot authorize group signalling: a group that
/// outlives the bound keeps the record and fails closed.
async fn wait_group_gone(pid: u32, bound: Duration) -> Result<(), SupervisorError> {
    let group = Pid::from_raw(i32::try_from(pid).map_err(|_| SupervisorError::InstanceConflict)?);
    let deadline = Instant::now() + bound;
    while super::group_exists(group).map_err(|_| SupervisorError::InstanceConflict)? {
        if Instant::now() >= deadline {
            return Err(SupervisorError::InstanceConflict);
        }
        sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

/// Whether `group` still has a member that is not a zombie. macOS killpg
/// returns EPERM (even for signal 0) when every member is an unreaped zombie;
/// this tells that harmless case apart from a live member we may not signal.
#[cfg(target_os = "macos")]
pub(super) fn group_has_live_member(group: Pid) -> io::Result<bool> {
    use libproc::{
        libproc::{bsd_info::BSDInfo, proc_pid::pidinfo},
        processes::{ProcFilter, pids_by_type},
    };
    let pgrpid = u32::try_from(group.as_raw()).map_err(io::Error::other)?;
    for pid in pids_by_type(ProcFilter::ByProgramGroup { pgrpid })? {
        let Ok(raw) = i32::try_from(pid) else {
            return Ok(true);
        };
        match pidinfo::<BSDInfo>(raw, 0) {
            // 5 is SZOMB.
            Ok(info) if info.pbi_pgid == pgrpid => {
                if info.pbi_status != 5 {
                    return Ok(true);
                }
            }
            Ok(_) => {}
            // pidinfo can fail for an unreaped zombie. A same-UID zombie
            // still accepts signal 0; EPERM here means a live foreign member.
            Err(_) => match nix::sys::signal::kill(Pid::from_raw(raw), None) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(_) => return Ok(true),
            },
        }
    }
    Ok(false)
}
#[cfg(target_os = "linux")]
pub(super) fn group_has_live_member(group: Pid) -> io::Result<bool> {
    for entry in fs::read_dir("/proc")? {
        let Some(pid) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .map(|(_, rest)| rest.split_whitespace().collect())
            .unwrap_or_default();
        if fields.get(2).and_then(|pgrp| pgrp.parse::<i32>().ok()) == Some(group.as_raw())
            && fields.first().is_some_and(|state| *state != "Z")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn group_has_live_member(_group: Pid) -> io::Result<bool> {
    Ok(true)
}

/// Every process visibly holding a TCP LISTEN socket on `port`, on any local
/// address. `unattributed` reports a listener whose holder is not visible.
pub(super) struct Listeners {
    pub pids: Vec<u32>,
    pub unattributed: bool,
}

/// macOS: `unsafe_code` is forbidden in this crate, and libproc exposes the
/// socket port only through a C union, so the SIP-protected lsof does the
/// per-PID socket walk. It cannot see other users' descriptors.
#[cfg(target_os = "macos")]
pub(super) async fn listeners(port: u16, bound: Duration) -> io::Result<Listeners> {
    let output = tokio::time::timeout(
        bound,
        tokio::process::Command::new("/usr/sbin/lsof")
            .env_clear()
            .args(["-nP", "-w", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fp"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| io::Error::other("listener query timed out"))??;
    // Exit 1 with no output is lsof's "no matching file"; anything else
    // unexplained cannot prove ownership.
    let none =
        output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty();
    if !output.status.success() && !none {
        return Err(io::Error::other("listener query failed"));
    }
    let text = std::str::from_utf8(&output.stdout).map_err(io::Error::other)?;
    let mut pids = Vec::new();
    for line in text.lines() {
        if let Some(pid) = line.strip_prefix('p') {
            pids.push(pid.parse().map_err(io::Error::other)?);
        }
    }
    Ok(Listeners {
        pids,
        unattributed: false,
    })
}
#[cfg(target_os = "linux")]
pub(super) async fn listeners(port: u16, _bound: Duration) -> io::Result<Listeners> {
    let mut sockets = std::collections::BTreeSet::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let text = match fs::read_to_string(table) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for line in text.lines().skip(1) {
            let fields: Vec<_> = line.split_whitespace().collect();
            let (Some(local), Some(state), Some(inode)) =
                (fields.get(1), fields.get(3), fields.get(9))
            else {
                return Err(io::Error::other("invalid socket table"));
            };
            let local_port = local
                .rsplit_once(':')
                .and_then(|(_, hex)| u16::from_str_radix(hex, 16).ok());
            // 0A is TCP_LISTEN.
            if *state == "0A" && local_port == Some(port) {
                sockets.insert(format!("socket:[{inode}]"));
            }
        }
    }
    let mut pids = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    if !sockets.is_empty() {
        for entry in fs::read_dir("/proc")? {
            let Some(pid) = entry?
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(descriptors) = fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            let mut holds = false;
            for descriptor in descriptors.flatten() {
                if let Ok(target) = fs::read_link(descriptor.path())
                    && let Some(target) = target.to_str()
                    && sockets.contains(target)
                {
                    seen.insert(target.to_owned());
                    holds = true;
                }
            }
            if holds {
                pids.push(pid);
            }
        }
    }
    Ok(Listeners {
        pids,
        unattributed: seen.len() != sockets.len(),
    })
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) async fn listeners(_port: u16, _bound: Duration) -> io::Result<Listeners> {
    Err(io::Error::other("unsupported listener ownership platform"))
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
