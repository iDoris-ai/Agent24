//! Narrow Unix boundary for installing a descriptor-pinned child cwd.
//!
//! Callers hand over an owned directory descriptor, never a path or raw fd.
//! The only unsafe operation is registering the post-fork/pre-exec hook; the
//! hook itself performs only the async-signal-safe `fchdir(2)` syscall.

#![cfg(unix)]

use std::os::fd::OwnedFd;

/// Make `dir` the working directory of the child produced by `command`.
///
/// The descriptor is moved into the pre-exec closure, so a later rename or
/// replacement of the directory's ambient path cannot redirect the child.
#[allow(unsafe_code)]
pub fn install_current_dir_fd(command: &mut tokio::process::Command, dir: OwnedFd) {
    use std::os::unix::process::CommandExt as _;

    // SAFETY: after fork and before exec this closure performs only fchdir on
    // an already-owned descriptor. It does not allocate, lock, inspect shared
    // process state, or call user code. fchdir is async-signal-safe on POSIX.
    unsafe {
        command
            .as_std_mut()
            .pre_exec(move || rustix::process::fchdir(&dir).map_err(std::io::Error::from));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn child_uses_owned_directory_descriptor_as_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::open(dir.path()).unwrap();
        let expected = dir.path().canonicalize().unwrap();
        let mut command = tokio::process::Command::new("/bin/pwd");
        install_current_dir_fd(&mut command, file.into());

        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(
            std::path::Path::new(String::from_utf8_lossy(&output.stdout).trim()),
            expected
        );
    }
}
