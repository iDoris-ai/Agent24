#![allow(dead_code)]

use crate::WorkspaceError;
use std::fs::File;

type Result<T> = std::result::Result<T, WorkspaceError>;

#[cfg(unix)]
use rustix::fs::{self, FileType, Mode, OFlags};

// `state_dir` is caller-trusted; same-UID concurrent mutation is out of scope.
// POSIX mkdirat→openat is intentionally not atomic create-and-pin in this slice.
const WORKSPACE_ROOTS: &str = "workspace-roots";

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RootIdentity {
    pub(crate) dev_le: [u8; 8],
    pub(crate) ino_le: [u8; 8],
}

#[cfg(unix)]
pub(crate) struct ManagedParent {
    file: File,
    trusted_owner: u64,
    identity: RootIdentity,
}

#[cfg(not(unix))]
pub(crate) struct ManagedParent {
    _private: (),
}

#[cfg(unix)]
pub(crate) struct PinnedWorkspaceRoot {
    file: File,
    identity: RootIdentity,
}

#[cfg(unix)]
fn unavailable(reason: &'static str) -> WorkspaceError {
    WorkspaceError::RootUnavailable { reason }
}

#[cfg(unix)]
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('\0') || name.contains('/') || name == "." || name == ".." {
        return Err(WorkspaceError::InvalidSpec { field: "root_name" });
    }
    Ok(())
}

#[cfg(unix)]
fn inspect(file: &File, owner: u64, reason: &'static str) -> Result<RootIdentity> {
    let stat = fs::fstat(file).map_err(|_| unavailable(reason))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(unavailable("root_not_directory"));
    }
    if Mode::from_raw_mode(stat.st_mode) != Mode::RWXU {
        return Err(unavailable("root_mode"));
    }
    if u64::from(stat.st_uid) != owner {
        return Err(unavailable("root_owner"));
    }
    #[allow(clippy::unnecessary_cast)]
    Ok(RootIdentity {
        dev_le: (stat.st_dev as u64).to_le_bytes(),
        ino_le: (stat.st_ino as u64).to_le_bytes(),
    })
}

#[cfg(unix)]
fn open_child(parent: &File, name: &str, reason: &'static str) -> Result<File> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = fs::openat(parent, name, flags, Mode::empty()).map_err(|_| unavailable(reason))?;
    Ok(File::from(fd))
}

#[cfg(unix)]
impl ManagedParent {
    pub(crate) fn open_or_create(state_dir: &File) -> Result<Self> {
        let state = fs::fstat(state_dir).map_err(|_| unavailable("state_dir_metadata"))?;
        if FileType::from_raw_mode(state.st_mode) != FileType::Directory {
            return Err(unavailable("state_dir_not_directory"));
        }
        let trusted_owner = u64::from(state.st_uid);
        let newly_created = match fs::mkdirat(state_dir, WORKSPACE_ROOTS, Mode::RWXU) {
            Ok(()) => true,
            Err(error) if error == rustix::io::Errno::EXIST => false,
            Err(_) => return Err(unavailable("workspace_roots_create")),
        };
        let file = open_child(state_dir, WORKSPACE_ROOTS, "workspace_roots_open")?;
        if newly_created {
            fs::fchmod(&file, Mode::RWXU).map_err(|_| unavailable("workspace_roots_mode"))?;
        }
        let identity = inspect(&file, trusted_owner, "workspace_roots_metadata")?;
        Ok(Self {
            file,
            trusted_owner,
            identity,
        })
    }

    pub(crate) fn create_root(&self, name: &str) -> Result<PinnedWorkspaceRoot> {
        validate_name(name)?;
        match fs::mkdirat(&self.file, name, Mode::RWXU) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::EXIST => {
                return Err(WorkspaceError::RootConflict {
                    reason: "root_exists",
                });
            }
            Err(_) => return Err(unavailable("root_create")),
        }
        let file = open_child(&self.file, name, "root_open")?;
        fs::fchmod(&file, Mode::RWXU).map_err(|_| unavailable("root_mode"))?;
        let identity = inspect(&file, self.trusted_owner, "root_metadata")?;
        Ok(PinnedWorkspaceRoot { file, identity })
    }

    pub(crate) fn reopen_root(
        &self,
        name: &str,
        expected: RootIdentity,
    ) -> Result<PinnedWorkspaceRoot> {
        validate_name(name)?;
        let file = open_child(&self.file, name, "root_open")?;
        let identity = inspect(&file, self.trusted_owner, "root_metadata")?;
        if identity != expected {
            return Err(unavailable("root_identity_mismatch"));
        }
        Ok(PinnedWorkspaceRoot { file, identity })
    }

    pub(crate) fn identity(&self) -> RootIdentity {
        self.identity
    }
}

#[cfg(unix)]
impl PinnedWorkspaceRoot {
    pub(crate) fn identity(&self) -> RootIdentity {
        self.identity
    }
}

#[cfg(not(unix))]
impl ManagedParent {
    pub(crate) fn open_or_create(_: &File) -> Result<Self> {
        Err(WorkspaceError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests;
