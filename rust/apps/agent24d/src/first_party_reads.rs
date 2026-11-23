//! Loop 4 (David 11-20, option b): the reads of the documents module this
//! app ships are known to the host as reads, so they are `Read` (no approval
//! per call), not `External`.
//!
//! ADR-K1-02 §2.1.1 item 6 stands: a module tool's class is the host's own
//! knowledge of it, never what the module says of itself. Here the host
//! knows the tools by name from its own list ([`DOCUMENTS_READS`], not the
//! manifest's `risk`), and knows the module is the one it ships by its bytes:
//! the program the package is really started with (its manifest's
//! `spawn.command`, resolved as the launcher resolves it, with no arguments)
//! must be byte for byte the one in this app's bundle (SHA-256). A
//! third-party package named `documents`, one whose program was changed, or
//! one that keeps a copy of the real program as a decoy while starting
//! another (PR-Daemon #1954 F1), matches nothing and keeps `External`.
//! Every other tool of every module, and the documents module's own writes
//! (`compose`, `patch`, `fill`, `extract`, ...), keep `External`.
//!
//! A read still needs its grant, as any module tool does: resources, expiry
//! and the package's artifact identity are checked at each call
//! (K1-6a-G §2.2), so a package swapped while the daemon runs has no grant.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The first-party module, and its tools the host knows to only read.
pub const MODULE: &str = "documents";
pub const DOCUMENTS_READS: &[&str] = &["get", "fields", "list", "paragraphs", "compare"];
/// The program of the package, relative to its directory.
const PROGRAM: &str = "bin/agent24-documents";

/// The bundled program, beside this daemon in the app:
/// `Contents/Resources/backend/agent24d` → `Contents/Resources/documents-module/bin/agent24-documents`.
/// None when this daemon does not run from an app bundle (a copy, a dev
/// build): then nothing is first-party and every tool keeps `External`.
#[must_use]
pub fn bundled_program() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let resources = exe.parent()?.parent()?;
    let program = resources.join("documents-module").join(PROGRAM);
    program.is_file().then_some(program)
}

fn sha256(path: &Path) -> Option<[u8; 32]> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hasher.finalize().into())
}

/// Whether the package at `package_dir`, started by `spawn`, runs the very
/// program of `bundled`: the program the launcher would start
/// (`agent24_os_proto::launch::resolve`), with no arguments, hashes the same.
#[must_use]
pub fn is_bundled(
    package_dir: &Path,
    spawn: &agent24_domain::SpawnCommand,
    bundled: &Path,
) -> bool {
    if !spawn.args.is_empty() {
        return false;
    }
    let Ok(started) = agent24_os_proto::launch::resolve(spawn, package_dir) else {
        return false;
    };
    match (sha256(&started), sha256(bundled)) {
        (Some(installed), Some(shipped)) => installed == shipped,
        _ => false,
    }
}

/// The `(module, tool)` pairs that are first-party reads, for the documents
/// package at `package_dir` started by `spawn` (None: not a package, or no
/// spawn), checked against `bundled`.
#[must_use]
pub fn host_reads(
    package: Option<(&Path, &agent24_domain::SpawnCommand)>,
    bundled: Option<&Path>,
) -> HashSet<(String, String)> {
    match (package, bundled) {
        (Some((dir, spawn)), Some(bundled)) if is_bundled(dir, spawn, bundled) => DOCUMENTS_READS
            .iter()
            .map(|tool| (MODULE.to_owned(), (*tool).to_owned()))
            .collect(),
        _ => HashSet::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use agent24_domain::SpawnCommand;
    use std::os::unix::fs::PermissionsExt;

    fn spawn(command: &str, args: &[&str]) -> SpawnCommand {
        SpawnCommand {
            command: command.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    /// A package tree the launcher accepts: owner-only, not group/other writable.
    fn package(dir: &Path, files: &[(&str, &[u8])]) {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        for (name, bytes) in files {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        for d in [dir.to_path_buf(), dir.join("bin")] {
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn bundle(root: &Path) -> std::path::PathBuf {
        let bundled = root.join("bundle/documents-module");
        package(&bundled, &[(PROGRAM, b"the program this app ships")]);
        bundled.join(PROGRAM)
    }

    #[test]
    fn only_the_bundled_program_makes_its_reads_first_party() {
        let root = tempfile::tempdir().unwrap();
        let shipped = bundle(root.path());
        let installed = root.path().join("packages/documents");
        package(&installed, &[(PROGRAM, b"the program this app ships")]);
        let reads = host_reads(Some((&installed, &spawn(PROGRAM, &[]))), Some(&shipped));
        let mut names: Vec<&str> = reads.iter().map(|(_, t)| t.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["compare", "fields", "get", "list", "paragraphs"]);
        assert!(reads.iter().all(|(m, _)| m == "documents"));
        for write in ["compose", "patch", "fill", "extract", "convert"] {
            assert!(!reads.contains(&("documents".to_owned(), write.to_owned())));
        }
    }

    #[test]
    fn a_package_named_documents_that_is_not_the_bundled_one_gets_nothing() {
        let root = tempfile::tempdir().unwrap();
        let shipped = bundle(root.path());
        let impostor = root.path().join("packages/documents");
        package(&impostor, &[(PROGRAM, b"a third party's program")]);
        assert!(host_reads(Some((&impostor, &spawn(PROGRAM, &[]))), Some(&shipped)).is_empty());
        let empty = root.path().join("packages/empty");
        package(&empty, &[]);
        assert!(host_reads(Some((&empty, &spawn(PROGRAM, &[]))), Some(&shipped)).is_empty());
        assert!(host_reads(Some((&impostor, &spawn(PROGRAM, &[]))), None).is_empty());
        assert!(host_reads(None, Some(&shipped)).is_empty());
    }

    /// PR-Daemon #1954 F1: a decoy copy of the real program at
    /// `bin/agent24-documents`, while the manifest starts another program
    /// (or the real one with arguments), is not the bundled module.
    #[test]
    fn a_decoy_copy_of_the_real_program_beside_another_started_one_gets_nothing() {
        let root = tempfile::tempdir().unwrap();
        let shipped = bundle(root.path());
        let decoy = root.path().join("packages/documents");
        package(
            &decoy,
            &[
                (PROGRAM, b"the program this app ships"),
                ("bin/evil", b"#!/bin/sh\n"),
            ],
        );
        assert!(host_reads(Some((&decoy, &spawn("bin/evil", &[]))), Some(&shipped)).is_empty());
        assert!(
            host_reads(
                Some((&decoy, &spawn(PROGRAM, &["--other"]))),
                Some(&shipped)
            )
            .is_empty()
        );
        // A bare name resolved on PATH is never the bundled program either.
        assert!(host_reads(Some((&decoy, &spawn("sh", &[]))), Some(&shipped)).is_empty());
        // The same package started as the bundle is: first-party.
        assert_eq!(
            host_reads(Some((&decoy, &spawn(PROGRAM, &[]))), Some(&shipped)).len(),
            5
        );
    }
}
