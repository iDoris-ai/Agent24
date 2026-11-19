//! Loop 4 (David 11-20, option b): the reads of the documents module this
//! app ships are known to the host as reads, so they are `Read` (no approval
//! per call), not `External`.
//!
//! ADR-K1-02 §2.1.1 item 6 stands: a module tool's class is the host's own
//! knowledge of it, never what the module says of itself. Here the host
//! knows the tools by name from its own list ([`DOCUMENTS_READS`], not the
//! manifest's `risk`), and knows the module is the one it ships by its bytes:
//! the installed `bin/agent24-documents` must be byte for byte the one in
//! this app's bundle (SHA-256). A third-party package named `documents`, or
//! one whose program was changed, matches nothing and keeps `External`.
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
pub const DOCUMENTS_READS: &[&str] = &["get", "fields", "list", "paragraphs"];
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

/// Whether the package at `package_dir` runs the very program of `bundled`.
#[must_use]
pub fn is_bundled(package_dir: &Path, bundled: &Path) -> bool {
    match (sha256(&package_dir.join(PROGRAM)), sha256(bundled)) {
        (Some(installed), Some(shipped)) => installed == shipped,
        _ => false,
    }
}

/// The `(module, tool)` pairs that are first-party reads, for the documents
/// package at `package_dir` (None: not a package) checked against `bundled`.
#[must_use]
pub fn host_reads(package_dir: Option<&Path>, bundled: Option<&Path>) -> HashSet<(String, String)> {
    match (package_dir, bundled) {
        (Some(dir), Some(bundled)) if is_bundled(dir, bundled) => DOCUMENTS_READS
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

    fn package(dir: &Path, program: &[u8]) {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join(PROGRAM), program).unwrap();
    }

    #[test]
    fn only_the_bundled_program_makes_its_reads_first_party() {
        let root = tempfile::tempdir().unwrap();
        let bundled = root.path().join("bundle/documents-module");
        package(&bundled, b"the program this app ships");
        let installed = root.path().join("packages/documents");
        package(&installed, b"the program this app ships");
        let reads = host_reads(Some(&installed), Some(&bundled.join(PROGRAM)));
        let mut names: Vec<&str> = reads.iter().map(|(_, t)| t.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["fields", "get", "list", "paragraphs"]);
        assert!(reads.iter().all(|(m, _)| m == "documents"));
        // The writes are never on the list.
        for write in ["compose", "patch", "fill", "extract", "convert"] {
            assert!(!reads.contains(&("documents".to_owned(), write.to_owned())));
        }
    }

    #[test]
    fn a_package_named_documents_that_is_not_the_bundled_one_gets_nothing() {
        let root = tempfile::tempdir().unwrap();
        let bundled = root.path().join("bundle/documents-module");
        package(&bundled, b"the program this app ships");
        let impostor = root.path().join("packages/documents");
        package(&impostor, b"a third party's program");
        assert!(host_reads(Some(&impostor), Some(&bundled.join(PROGRAM))).is_empty());
        // No program at all, no bundle, or no package: nothing either.
        let empty = root.path().join("packages/empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(host_reads(Some(&empty), Some(&bundled.join(PROGRAM))).is_empty());
        assert!(host_reads(Some(&impostor), None).is_empty());
        assert!(host_reads(None, Some(&bundled.join(PROGRAM))).is_empty());
    }
}
