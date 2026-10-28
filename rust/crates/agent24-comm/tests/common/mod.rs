//! Shared fixture helper: put a fake `hyphae` script where `VerifiedBinary`
//! expects it WITHOUT this process ever holding a writable fd on an
//! executable file.
//!
//! Why (Linux `ETXTBSY`, "Text file busy"): `VerifiedBinary::install` creates
//! `<install_dir>/hyphae-<sha>` with `create_new` + `write_all` on a
//! `spawn_blocking` thread. While that write fd is open, any *other* thread
//! of this test process that `fork`s (cargo runs the tests of one binary in
//! parallel threads of ONE process, and every supervisor/runner spawn is a
//! fork) copies the fd into its child, which holds it until its `exec`
//! closes it (`O_CLOEXEC`). If our own `exec` of the freshly written file
//! lands in that window the kernel answers `ETXTBSY`. macOS has no such
//! check, so it only ever shows up on Linux CI.
//!
//! Renaming a temp file into place does NOT help: the inherited fd refers to
//! the same inode, which stays "open for write" under its new name.
//!
//! What does work: let a short-lived child process (`cp`) do the writing.
//! The writable fd then only ever exists inside that child, which is not a
//! fork of this multithreaded process (it cannot be copied into anybody
//! else's child), and we wait for it to exit before anything execs the file.
//! `chmod` afterwards opens no fd. `VerifiedBinary::install` is then called
//! as usual and takes its `AlreadyExists` branch (read + hash only, no
//! write), so product code is exercised unchanged.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use agent24_comm::VerifiedBinary;
use agent24_comm::binary::Sha256Digest;

pub async fn install_verified(
    source: &Path,
    expected: Sha256Digest,
    install_dir: &Path,
) -> VerifiedBinary {
    let source = source.to_path_buf();
    let install_dir = install_dir.to_path_buf();
    {
        let source = source.clone();
        let install_dir = install_dir.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&install_dir).unwrap();
            // Same naming rule as `install_blocking`.
            let dest = install_dir.join(format!("hyphae-{}", expected.short()));
            if !dest.exists() {
                let status = std::process::Command::new("cp")
                    .arg(&source)
                    .arg(&dest)
                    .status()
                    .expect("spawn cp");
                assert!(status.success(), "cp {source:?} -> {dest:?}: {status}");
                std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o500)).unwrap();
            }
        })
        .await
        .unwrap();
    }
    VerifiedBinary::install(&source, expected, &install_dir)
        .await
        .unwrap()
}
