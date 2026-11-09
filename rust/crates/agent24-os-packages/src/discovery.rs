//! ME-3a — where the catalogue comes from when it is not compiled in.
//!
//! Before this, the daemon's `Installed` catalogue entries were a `vec![]`
//! written by hand in `server.rs`. That made ME-3's headline acceptance — *installing a
//! third-party domain OS requires no kernel change* — untestable in the only way
//! that counts: a mock module dropped into the catalogue proved nothing, because
//! reaching the catalogue meant editing and rebuilding the daemon.
//!
//! This module reads packages off disk instead. What it deliberately does NOT do
//! is construct anything: an out-of-process module has no Rust type to construct,
//! and the transport that would give it one is ME-3b. A discovered package
//! therefore reaches the mounter and is refused there, by the check that already
//! exists — but it is now VISIBLE (`agent24 os list`) and its manifest is
//! validated, which is what makes ME-3b's work a transport problem rather than a
//! discovery problem.
//!
//! # The property this file exists to hold
//!
//! **A package that is refused leaves nothing behind.** Not a partially-written
//! directory, not a temp file, not a registry row. The acceptance in §8 of
//! `SPEC-ME3-OUT-OF-PROCESS.md` says "rejected AND not persisted", and the second
//! half is the half that is easy to write a decorative test for: asserting that a
//! function returned `Err` says nothing about what it left on disk.

use std::path::{Path, PathBuf};

use agent24_domain::DomainOsManifest;
use agent24_domain::tool::ModuleToolRegistry;
use tokio_util::sync::CancellationToken;

/// The file a package must contain to be a package at all.
pub const MANIFEST_FILE: &str = "domain-os.yml";

/// One package found on disk, with its manifest already validated.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub manifest: DomainOsManifest,
    /// The directory the manifest was read from. Kept for diagnostics and for
    /// ME-3b, which needs it to resolve a spawn command relative to the package.
    pub dir: PathBuf,
    /// `sha256:<hex>` of the manifest file's exact bytes — what the module must
    /// report in its `initialize`, so a module started against one manifest
    /// cannot be served under another (SPEC §3). Computed from the bytes that
    /// were parsed, not re-read, so the digest and the manifest are one read.
    pub digest: String,
}

/// `sha256:` and the lowercase hex of `bytes`: the manifest digest format.
///
/// ME4-S3 §4.4 (M4): moved into `agent24_os_proto::manifest`, so the
/// kernel's discovery and a module's handshake share one function; this
/// re-export keeps every existing call site in this crate unchanged.
pub use agent24_os_proto::manifest::manifest_digest;

/// Why one directory under the packages root did not become a [`Discovered`].
///
/// A refusal is a REPORT, not an error that stops the scan: one malformed package
/// must not hide the others, and the operator needs to be told which one and why.
#[derive(Debug, Clone)]
pub struct Refused {
    pub dir: PathBuf,
    pub why: String,
}

/// The result of one scan. Both halves matter: `found` is what can be mounted,
/// `refused` is what the operator has to fix, and neither is an error.
/// `cancelled` is neither: the shutdown token fired before that package's
/// check finished, so it is not mounted and it is not a damaged package.
#[derive(Debug, Default)]
pub struct Scan {
    pub found: Vec<Discovered>,
    pub refused: Vec<Refused>,
    /// Directories left unverified because `cancel` fired. Not discovered,
    /// not registered, not spawned. A later scan with a live token may
    /// still discover them. A stat match is not a reason to skip the hash.
    pub cancelled: Vec<PathBuf>,
}

/// Read every package directly under `root`.
///
/// `root` missing is not a refusal — a daemon with no third-party modules is the
/// normal case, and reporting it as a problem would train operators to ignore the
/// report. An unreadable `root` IS reported, because that is a real difference
/// from "empty" that the operator can act on. (Distinguishing those two is the
/// same rule FU-32 is about: an empty answer and a broken channel must not look
/// alike.)
///
/// `cancel` is the daemon shutdown token. Every live scan still hashes the
/// whole payload — a matching size or mtime does not prove the bytes — but
/// a fired token stops this walk. A package whose check did not finish is
/// listed on [`Scan::cancelled`], not [`Scan::found`] and not
/// [`Scan::refused`]. Only a check that returns while the token is still
/// live may be discovered; the caller must not register its tools or spawn
/// it otherwise.
pub fn scan(root: &Path, cancel: &CancellationToken) -> Scan {
    let mut out = Scan::default();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.refused.push(Refused {
                dir: root.to_owned(),
                why: format!("packages root could not be read: {e}"),
            });
            return out;
        }
    };

    // Every entry is CLASSIFIED; nothing is dropped on the floor.
    //
    // `DirEntry::file_type()` does not follow symlinks, so the obvious
    // `.filter(|e| e.is_dir())` silently discards a symlinked package directory —
    // neither found nor refused, invisible in `agent24 os list`. That would make a
    // THIRD cause of an empty catalogue ("the scanner could not see it") look
    // exactly like the other two ("nothing installed", "everything refused"), which
    // is the FU-32 mistake in a new place. It is also inconsistent: a symlinked
    // MANIFEST is refused explicitly one level down.
    let mut dirs: Vec<PathBuf> = Vec::new();
    for e in entries.filter_map(std::result::Result::ok) {
        let path = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => {
                // A dot-prefixed directory is never a package. This is not tidiness:
                // `install` stages into `.staging-<name>-<pid>-<seq>` INSIDE this
                // very root, so a crash leaves a HALF-COPIED package here — with a
                // valid manifest, because the manifest is copied first. Worse,
                // `.` (0x2E) sorts before every letter, so after `dirs.sort()` the
                // wreckage reaches `mount_all` BEFORE the real package and wins the
                // name claim (first-come-first-served). The operator then sees the
                // real package refused as a duplicate, which reads as if the real
                // one were the intruder.
                //
                // Refused rather than skipped, for the reason the symlink arm gives:
                // an entry that is neither found nor refused makes a cause of an
                // empty catalogue that nobody can see.
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
                {
                    out.refused.push(Refused {
                        dir: path,
                        why: "dot-prefixed directory; not a package (install debris \
                              looks like this — remove it)"
                            .to_owned(),
                    });
                } else {
                    dirs.push(path);
                }
            }
            Ok(t) if t.is_symlink() => out.refused.push(Refused {
                dir: path,
                // Refused rather than followed, for the reason the manifest-level
                // check gives: a package's identity is decided by a file inside it,
                // and a link lets that file live somewhere the packages root does
                // not govern. The workaround is a copy, and the message says so
                // because a developer who linked a work-in-progress module here
                // needs to know what to do instead.
                why: "package directory is a symlink; refusing to follow it (copy                       the directory instead)"
                    .to_owned(),
            }),
            // A stray file in the packages root is a mistake worth naming — most
            // often a stray archive or a manifest left at the top level.
            Ok(_) => out.refused.push(Refused {
                dir: path,
                why: "not a directory; a package is a directory containing                       domain-os.yml"
                    .to_owned(),
            }),
            Err(e) => out.refused.push(Refused {
                dir: path,
                why: format!("could not determine what this entry is: {e}"),
            }),
        }
    }
    // Deterministic order: the scan feeds a catalogue whose duplicate handling
    // depends on which entry is seen first, so a filesystem's arbitrary readdir
    // order would make "which of two same-named packages wins" vary between runs
    // on the same disk.
    dirs.sort();

    let mut rest = dirs.into_iter();
    while let Some(dir) = rest.next() {
        // Same outcome as `read_package`'s first check, which is what
        // refuses to parse a manifest. Kept so a fired token does not
        // enter `read_package` once per remaining directory.
        if cancel.is_cancelled() {
            out.cancelled.push(dir);
            out.cancelled.extend(rest);
            return out;
        }
        match read_package(&dir, cancel) {
            PackageRead::Found(found) => out.found.push(*found),
            PackageRead::Refused(why) => out.refused.push(Refused { dir, why }),
            PackageRead::Cancelled => {
                out.cancelled.push(dir);
                out.cancelled.extend(rest);
                return out;
            }
        }
    }
    out
}

/// One directory, after the manifest was read. `Cancelled` is not a refusal:
/// the package is not damaged, and it must not become [`Discovered`].
enum PackageRead {
    Found(Box<Discovered>),
    Refused(String),
    Cancelled,
}

/// Read and validate one package directory.
fn read_package(dir: &Path, cancel: &CancellationToken) -> PackageRead {
    if cancel.is_cancelled() {
        return PackageRead::Cancelled;
    }
    let path = dir.join(MANIFEST_FILE);

    // A symlinked manifest is refused rather than followed. The kernel derives a
    // module's data directory from its validated name, and following a link here
    // would let a package point the manifest — the thing that DECIDES that name —
    // at a file outside its own directory.
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) => return PackageRead::Refused(format!("no readable {MANIFEST_FILE}: {e}")),
    };
    if meta.file_type().is_symlink() {
        return PackageRead::Refused(format!(
            "{MANIFEST_FILE} is a symlink; refusing to follow it"
        ));
    }
    if !meta.is_file() {
        return PackageRead::Refused(format!("{MANIFEST_FILE} is not a regular file"));
    }
    // Bound the read BEFORE it happens. `from_yaml` also checks a size limit, but
    // it checks it on a string that has already been read into memory.
    if meta.len() > DomainOsManifest::MAX_YAML_BYTES as u64 {
        return PackageRead::Refused(format!(
            "{MANIFEST_FILE} is {} bytes, over the {} byte limit",
            meta.len(),
            DomainOsManifest::MAX_YAML_BYTES
        ));
    }

    // Read BYTES, not a string. `read_to_string` would fail on non-UTF-8 with
    // "stream did not contain valid UTF-8" and no hint of which file — and a
    // UTF-16 manifest (a real Windows accident, with a FF FE BOM) is exactly that.
    // Decoding here lets each case have its own readable reason.
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return PackageRead::Refused(format!("could not read {MANIFEST_FILE}: {e}"));
        }
    };
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return PackageRead::Refused(format!(
            "{MANIFEST_FILE} is UTF-16 (byte-order mark {:02X} {:02X}); it must be UTF-8",
            bytes[0], bytes[1]
        ));
    }
    let digest = manifest_digest(&bytes);
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => {
            return PackageRead::Refused(format!("{MANIFEST_FILE} is not valid UTF-8: {e}"));
        }
    };

    let manifest = match DomainOsManifest::from_yaml(&text) {
        Ok(manifest) => manifest,
        Err(e) => return PackageRead::Refused(e.to_string()),
    };
    // S1-9 PR-1: a package that declares integrity is verified in full before
    // it can become Discovered. A package that does not declare it keeps the
    // previous manifest-only behaviour. PR-3 replaces this with verify_required.
    // Cancel is not success and not a hash mismatch: the package stays off
    // `found` so nothing registers its tools or spawns it.
    match crate::package_integrity::verify_if_present(dir, &manifest, cancel) {
        Ok(()) => {}
        Err(agent24_components::integrity::IntegrityError::Cancelled) => {
            return PackageRead::Cancelled;
        }
        Err(e) => return PackageRead::Refused(e.to_string()),
    }
    if cancel.is_cancelled() {
        return PackageRead::Cancelled;
    }
    PackageRead::Found(Box::new(Discovered {
        manifest,
        dir: dir.to_owned(),
        digest,
    }))
}

/// Build the host-side module-tool registry from a scan's `found` packages,
/// in order (K1-5.1, ADR-K1-01 §2.1/§2.2/§3/§5(2)).
///
/// Each package's manifest has already validated its OWN tool list
/// internally (no duplicate operation, valid names/schema/enums — see
/// `agent24_domain::tool::validate_tools`), so registering it here can only
/// fail if an EARLIER package in this same batch already claimed the same
/// module `name` — `scan` does not dedupe by name (see its own doc comment;
/// that is "downstream", the mounter's job). The gate is the MODULE NAME, not
/// just an operation-name collision: two found directories that declare the
/// same `name` but DIFFERENT operations produce no full-name collision at
/// all, yet the later one must still be refused in full — see
/// `ModuleToolRegistry::register_module`'s own docs for why a full-name-only
/// check would miss exactly the cross-module impersonation ADR-K1-01 §3
/// requires to fail closed. A colliding module's tools are refused IN FULL
/// via [`Refused`], exactly like a malformed manifest: the registry already
/// built from earlier packages is left untouched, never partially updated.
///
/// This function only builds the registry; it offers no invocation path.
/// What the agent may see is decided by `ModuleToolRegistry::adverts`
/// through the host's `ModuleToolAdvertView` gates (K1-5.2).
pub fn build_tool_registry(found: &[Discovered]) -> (ModuleToolRegistry, Vec<Refused>) {
    let mut registry = ModuleToolRegistry::new();
    let mut refused = Vec::new();
    for d in found {
        if let Err(why) = registry.register_module(d.manifest.name(), d.manifest.tools()) {
            refused.push(Refused {
                dir: d.dir.clone(),
                why,
            });
        }
    }
    (registry, refused)
}

/// Re-read the package and compare the manifest digest frozen at mount time.
/// Packages declaring integrity also repeat the full payload verification;
/// legacy packages retain manifest-only checking until PR-3.
///
/// `Err` means the caller must not spawn. `Err("cancelled")` is a shutdown,
/// not a damaged package and not a hash mismatch: the hash did not finish,
/// so this restart is not a success. The full tree is still hashed whenever
/// the token is live.
pub fn recheck(dir: &Path, expected: &str, cancel: &CancellationToken) -> Result<(), String> {
    // The metadata branches below return before `read_package`, so its
    // cancel check never runs for a missing, linked, or non-directory path.
    if cancel.is_cancelled() {
        return Err("cancelled".to_owned());
    }
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_symlink() => {
            return Err("the package directory is now a symlink; refusing to follow it".to_owned());
        }
        Ok(m) if !m.is_dir() => {
            return Err("the package directory is no longer a directory".to_owned());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("the package directory no longer exists".to_owned());
        }
        Err(e) => return Err(format!("could not check the package directory: {e}")),
    }
    match read_package(dir, cancel) {
        PackageRead::Found(found) if found.digest == expected => Ok(()),
        PackageRead::Found(found) => Err(format!(
            "the manifest changed ({expected} \u{2192} {})",
            found.digest
        )),
        PackageRead::Cancelled => Err("cancelled".to_owned()),
        PackageRead::Refused(why) => Err(format!("the package is no longer usable: {why}")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::io::Write;

    /// Live token. Tests of cancellation call [`super::scan`] directly.
    fn scan(root: &Path) -> Scan {
        super::scan(root, &CancellationToken::new())
    }

    fn recheck(dir: &Path, expected: &str) -> Result<(), String> {
        super::recheck(dir, expected, &CancellationToken::new())
    }

    fn read_package(dir: &Path) -> Result<Discovered, String> {
        match super::read_package(dir, &CancellationToken::new()) {
            PackageRead::Found(found) => Ok(*found),
            PackageRead::Refused(why) => Err(why),
            PackageRead::Cancelled => Err("cancelled".to_owned()),
        }
    }

    /// A manifest for a name this binary has never heard of. That is the point:
    /// `sin90` is compiled in, so discovering it would prove nothing.
    fn manifest_yaml(name: &str, kind: &str) -> String {
        // An out-of-process module must declare how to start it (ME-3b-3), so the
        // fixture supplies one; an in-process crate must NOT, so it supplies none.
        // Both halves are enforced at parse time, which is why the fixture cannot
        // just always include it.
        let spawn = if kind == "out_of_process_provider" {
            format!("spawn:\n  command: bin/{name}\n")
        } else {
            String::new()
        };
        format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: [events]\nimpl_kind: {kind}\n{spawn}"
        )
    }

    /// Like [`manifest_yaml`], plus one declared tool operation named `op`.
    fn manifest_yaml_with_tool(name: &str, kind: &str, op: &str) -> String {
        format!(
            "{}tools:\n  - operation: {op}\n    description: does a thing\n    \
             input_schema: {{type: object, properties: {{}}}}\n    risk: read\n    \
             output_privacy: local_only\n    timeout_ms: 5000\n    inline_wait_ms: 1000\n",
            manifest_yaml(name, kind)
        )
    }

    fn install(root: &Path, name: &str, body: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join(MANIFEST_FILE)).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        // A manifest this loader accepts is sealed with a native stub at
        // `spawn.command`, so the package stays discoverable once integrity
        // is required. Unparseable bodies stay as written: those tests want
        // a refusal, not a sealed tree.
        if let Ok(manifest) = agent24_domain::DomainOsManifest::from_yaml(body)
            && let Some(spawn) = manifest.spawn()
        {
            crate::test_support::seal_exit_stub(&dir, &spawn.command).unwrap();
        }
        dir
    }

    #[test]
    fn a_package_written_after_this_binary_was_built_is_found() {
        // The ONLY shape that demonstrates "the catalogue is not compiled in".
        // `rg -c 'include_str!'` returning 0 would show it is not compiled in THAT
        // way; it cannot show that anything is actually read from disk.
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            "cos72",
            &manifest_yaml("cos72", "out_of_process_provider"),
        );

        let scan = scan(root.path());
        assert!(scan.refused.is_empty(), "{:?}", scan.refused);
        assert_eq!(scan.found.len(), 1);
        assert_eq!(scan.found[0].manifest.name(), "cos72");
    }

    /// The digest a module must report is `sha256:` + lowercase hex of the
    /// manifest file's exact bytes (the known vector pins the format), and a
    /// discovered package carries the digest of the file it was parsed from.
    #[test]
    fn a_discovered_package_carries_the_digest_of_its_manifest_bytes() {
        assert_eq!(
            manifest_digest(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let root = tempfile::tempdir().unwrap();
        let body = manifest_yaml("cos72", "out_of_process_provider");
        let dir = install(root.path(), "cos72", &body);
        let yml = dir.join(MANIFEST_FILE);
        // `install` seals, so the digest is of the file on disk, not the
        // pre-seal text.
        let sealed = std::fs::read(&yml).unwrap();
        let scan = scan(root.path());
        assert_eq!(scan.found[0].digest, manifest_digest(&sealed));
        // One byte more is another manifest, and another digest. The payload
        // hash is unchanged; only the manifest bytes move. Mode stays 0400,
        // which is what a sealed manifest is published as.
        crate::test_support::make_writable(&yml).unwrap();
        let mut extra = sealed.clone();
        extra.push(b'\n');
        std::fs::write(&yml, &extra).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&yml).unwrap().permissions();
            permissions.set_mode(0o400);
            std::fs::set_permissions(&yml, permissions).unwrap();
        }
        assert_ne!(scan_digest(root.path()), manifest_digest(&sealed));
    }

    fn scan_digest(root: &Path) -> String {
        scan(root).found[0].digest.clone()
    }

    #[test]
    fn a_missing_root_is_empty_not_an_error() {
        // A daemon with no third-party modules is the normal case. Reporting it as
        // a problem trains operators to skim the report, and then a real refusal
        // goes unread.
        let scan = scan(Path::new("/nonexistent/agent24-packages"));
        assert!(scan.found.is_empty() && scan.refused.is_empty());
    }

    #[test]
    fn an_unreadable_root_is_reported_not_silently_empty() {
        // The FU-32 rule in a new place: "nothing here" and "I could not look"
        // must not produce the same answer.
        let root = tempfile::tempdir().unwrap();
        let sub = root.path().join("locked");
        std::fs::create_dir(&sub).unwrap();
        install(
            &sub,
            "cos72",
            &manifest_yaml("cos72", "out_of_process_provider"),
        );
        let mut perms = std::fs::metadata(&sub).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o000);
        }
        std::fs::set_permissions(&sub, perms).unwrap();

        let scan = scan(&sub);
        // Restore before asserting, or a failure leaves an undeletable tempdir.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&sub).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&sub, p).unwrap();
        }
        assert_eq!(scan.found.len(), 0);
        assert_eq!(scan.refused.len(), 1, "an unreadable root must be REPORTED");
        assert!(
            scan.refused[0].why.contains("could not be read"),
            "{:?}",
            scan.refused
        );
    }

    #[test]
    fn one_bad_package_does_not_hide_the_others() {
        // A scan that stops at the first refusal turns one operator mistake into
        // "none of my modules load", with only the first one explained.
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "aaa-broken", "name: [not, a, string]\n");
        install(
            root.path(),
            "zzz-good",
            &manifest_yaml("zzz", "out_of_process_provider"),
        );

        let scan = scan(root.path());
        assert_eq!(scan.found.len(), 1, "the good package must still load");
        assert_eq!(scan.refused.len(), 1);
    }

    #[test]
    fn a_utf16_manifest_says_so_instead_of_stream_did_not_contain_valid_utf8() {
        // A real Windows accident. Read as a string, this fails with "stream did
        // not contain valid UTF-8" and no file name — useless. Byte-level
        // detection is why this path reads bytes.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("utf16");
        std::fs::create_dir_all(&dir).unwrap();
        let mut bytes = vec![0xFF, 0xFE];
        for b in "name: x\n".encode_utf16() {
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        std::fs::write(dir.join(MANIFEST_FILE), &bytes).unwrap();

        let scan = scan(root.path());
        assert_eq!(scan.refused.len(), 1);
        let why = &scan.refused[0].why;
        assert!(
            why.contains("UTF-16") && why.contains(MANIFEST_FILE),
            "the reason must name the file and the actual problem: {why}"
        );
    }

    #[test]
    fn invalid_utf8_without_a_bom_is_named_as_such() {
        // The UTF-16 test above never reaches `from_utf8` — the byte-order mark
        // catches it first. So without this case, swapping `from_utf8` for
        // `from_utf8_lossy` breaks nothing that is tested, and a manifest with
        // corrupt bytes would be silently repaired with U+FFFD and then fail with
        // a confusing YAML error instead of the real one. (A mutation found this:
        // replacing the strict decode killed no test.)
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let mut bytes = manifest_yaml("corrupt", "out_of_process_provider").into_bytes();
        // A lone continuation byte: invalid UTF-8, and NOT a byte-order mark.
        bytes.insert(6, 0x80);
        // Control: the fixture must really be invalid UTF-8, or this proves nothing.
        assert!(String::from_utf8(bytes.clone()).is_err());
        std::fs::write(dir.join(MANIFEST_FILE), &bytes).unwrap();

        let scan = scan(root.path());
        assert_eq!(scan.refused.len(), 1);
        let why = &scan.refused[0].why;
        assert!(
            why.contains("not valid UTF-8"),
            "the reason must say the bytes are the problem, not blame YAML: {why}"
        );
    }

    #[test]
    fn a_utf8_bom_is_accepted_through_the_real_file_path() {
        // The byte-layer half of the BOM fix. The string-layer test in
        // `agent24-domain` knows nothing about `fs::read` + `from_utf8`; this one
        // goes through the loader that a Windows-authored package actually hits.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("bom");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(MANIFEST_FILE),
            manifest_yaml("bom", "out_of_process_provider"),
        )
        .unwrap();
        crate::test_support::seal_exit_stub(&dir, "bin/bom").unwrap();
        // The BOM is a property of the bytes the loader reads. Prepend it
        // after seal: the integrity section still names the same files.json,
        // and the file must stay mode 0400 or the permission check refuses it
        // for a different reason.
        let yml = dir.join(MANIFEST_FILE);
        crate::test_support::make_writable(&yml).unwrap();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(std::fs::read(&yml).unwrap());
        std::fs::write(&yml, &bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&yml).unwrap().permissions();
            permissions.set_mode(0o400);
            std::fs::set_permissions(&yml, permissions).unwrap();
        }

        // Control: the fixture must really carry a BOM, or this proves nothing.
        assert_eq!(
            &std::fs::read(dir.join(MANIFEST_FILE)).unwrap()[..3],
            &[0xEF, 0xBB, 0xBF]
        );

        let scan = scan(root.path());
        assert!(scan.refused.is_empty(), "{:?}", scan.refused);
        assert_eq!(scan.found.len(), 1);
    }

    #[test]
    fn a_symlinked_manifest_is_refused_not_followed() {
        // The manifest decides the module's name, and the name decides its data
        // directory. Following a link here would let a package point that decision
        // at a file outside itself.
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside.yml");
        std::fs::write(&outside, manifest_yaml("evil", "out_of_process_provider")).unwrap();
        let dir = root.path().join("linky");
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join(MANIFEST_FILE)).unwrap();

        let scan = scan(root.path());
        assert!(scan.found.is_empty(), "a symlinked manifest must not load");
        assert!(
            scan.refused.iter().any(|r| r.why.contains("symlink")),
            "{:?}",
            scan.refused
        );
    }

    #[test]
    fn every_entry_is_classified_none_are_silently_dropped() {
        // `DirEntry::file_type()` does not follow symlinks, so the obvious
        // `.filter(|e| e.is_dir())` drops a symlinked package directory into
        // NOTHING — not found, not refused, invisible in `agent24 os list`. That
        // makes a THIRD cause of an empty catalogue ("the scanner could not see
        // it") indistinguishable from the other two. It is also inconsistent: a
        // symlinked MANIFEST is refused explicitly one level down.
        //
        // The property is 4-in / 4-seen, not "the symlink is refused" — the point
        // is that nothing falls off the edge.
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            "good",
            &manifest_yaml("good", "out_of_process_provider"),
        );

        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        install(
            &elsewhere,
            "real",
            &manifest_yaml("real", "out_of_process_provider"),
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(elsewhere.join("real"), root.path().join("linked-pkg")).unwrap();

        std::fs::write(root.path().join("stray.txt"), b"not a package").unwrap();

        let scan = scan(root.path());
        let seen: Vec<String> = scan
            .found
            .iter()
            .map(|d| d.dir.clone())
            .chain(scan.refused.iter().map(|r| r.dir.clone()))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();

        for entry in ["good", "linked-pkg", "stray.txt", "elsewhere"] {
            assert!(
                seen.contains(&entry.to_owned()),
                "{entry} fell off the edge — it is neither found nor refused. \
                 Seen: {seen:?}"
            );
        }
        // Control: the one legitimate package still loads, or "everything is
        // classified" would be satisfiable by refusing everything.
        assert!(
            scan.found.iter().any(|d| d.manifest.name() == "good"),
            "the good package must still load"
        );
    }

    #[test]
    fn install_debris_is_not_discovered_as_a_package() {
        // A daemon killed mid-install leaves `.staging-<name>-<pid>-<seq>` in this
        // very root, and it contains a VALID manifest — the manifest is copied
        // first, so the wreckage parses. Worse, `.` (0x2E) sorts before every
        // letter, so after `dirs.sort()` the wreckage reaches `mount_all` ahead of
        // the real package and wins the name claim (first-come-first-served). The
        // operator then sees the REAL package refused as a duplicate, which reads
        // as though the real one were the intruder.
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            ".staging-cos72-99999-0",
            &manifest_yaml("cos72", "out_of_process_provider").replace("0.1.0", "0.0.1"),
        );
        install(
            root.path(),
            "cos72",
            &manifest_yaml("cos72", "out_of_process_provider"),
        );
        install(
            root.path(),
            "alpha",
            &manifest_yaml("alpha", "out_of_process_provider"),
        );

        let scan = scan(root.path());
        let found: Vec<(&str, &str)> = scan
            .found
            .iter()
            .map(|d| (d.manifest.name(), d.manifest.version()))
            .collect();

        assert!(
            !scan.found.iter().any(|d| d
                .dir
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))),
            "debris must not be discovered as a package: {found:?}"
        );
        // The real package must be the one that survives, at its real version.
        assert!(found.contains(&("cos72", "0.1.0")), "{found:?}");
        // Control: an ordinary package is still found, so "nothing is found" cannot
        // satisfy the assertion above.
        assert!(found.contains(&("alpha", "0.1.0")), "{found:?}");
        // And it is REFUSED, not silently skipped — otherwise debris becomes an
        // invisible cause of a confusing catalogue.
        assert!(
            scan.refused.iter().any(|r| r.why.contains("dot-prefixed")),
            "{:?}",
            scan.refused
        );
    }

    #[test]
    fn the_scan_order_is_deterministic() {
        // Duplicate handling downstream depends on which entry is seen first, so
        // an arbitrary readdir order would make "which of two same-named packages
        // wins" vary between runs on the same disk.
        let root = tempfile::tempdir().unwrap();
        for n in ["m-c", "m-a", "m-b"] {
            install(
                root.path(),
                n,
                &manifest_yaml(&n.replace('-', ""), "out_of_process_provider"),
            );
        }
        let names: Vec<String> = scan(root.path())
            .found
            .iter()
            .map(|d| d.dir.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["m-a", "m-b", "m-c"]);
    }

    // FU-61: `recheck` — re-validating a package right before a restart, not
    // just at install/mount time.

    #[test]
    fn recheck_accepts_the_manifest_it_saw_before() {
        let root = tempfile::tempdir().unwrap();
        let dir = install(
            root.path(),
            "cos72",
            &manifest_yaml("cos72", "out_of_process_provider"),
        );
        let found = read_package(&dir).unwrap();

        assert_eq!(recheck(&dir, &found.digest), Ok(()));
    }

    #[test]
    fn recheck_reports_both_digests_when_the_manifest_changed() {
        let root = tempfile::tempdir().unwrap();
        let dir = install(
            root.path(),
            "cos72",
            &manifest_yaml("cos72", "out_of_process_provider"),
        );
        let old_digest = read_package(&dir).unwrap().digest;
        let new_body = manifest_yaml("cos72", "out_of_process_provider").replace("0.1.0", "0.2.0");
        let yml = dir.join(MANIFEST_FILE);
        crate::test_support::make_writable(&yml).unwrap();
        std::fs::write(&yml, &new_body).unwrap();
        // Reseal so the replacement is still a package recheck can read. The
        // digest compared below is the sealed file, which is what recheck hashes.
        crate::test_support::seal(&dir, &["bin/cos72"]).unwrap();
        let new_digest = manifest_digest(&std::fs::read(&yml).unwrap());
        // Control: the rewrite must actually produce a different digest, or
        // this test would pass even if `recheck` echoed the same value twice.
        assert_ne!(old_digest, new_digest);

        let err = recheck(&dir, &old_digest).unwrap_err();
        assert!(err.contains(&old_digest), "{err}");
        assert!(
            err.contains(&new_digest),
            "must name what it changed TO, not just what it changed FROM: {err}"
        );
        assert!(err.contains("changed"), "{err}");
    }

    #[test]
    fn recheck_reports_a_removed_package_directory() {
        let root = tempfile::tempdir().unwrap();
        let dir = install(
            root.path(),
            "cos72",
            &manifest_yaml("cos72", "out_of_process_provider"),
        );
        let digest = read_package(&dir).unwrap().digest;
        std::fs::remove_dir_all(&dir).unwrap();

        let err = recheck(&dir, &digest).unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
    }

    #[test]
    fn recheck_reports_the_path_becoming_a_plain_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = install(
            root.path(),
            "cos72",
            &manifest_yaml("cos72", "out_of_process_provider"),
        );
        let digest = read_package(&dir).unwrap().digest;
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::write(&dir, b"not a directory any more").unwrap();

        let err = recheck(&dir, &digest).unwrap_err();
        assert!(err.contains("no longer a directory"), "{err}");
    }

    #[test]
    fn recheck_refuses_a_package_directory_replaced_by_a_symlink() {
        // Mirrors `scan`'s own package-directory-level symlink refusal
        // (`every_entry_is_classified_none_are_silently_dropped`) — even
        // when the manifest reachable through the link is byte-identical
        // (same digest), the directory-level check must still say no.
        // `read_package` alone only re-validates the MANIFEST file; without
        // this check a symlinked replacement would pass `recheck`.
        let root = tempfile::tempdir().unwrap();
        let dir = install(
            root.path(),
            "cos72",
            &manifest_yaml("cos72", "out_of_process_provider"),
        );
        let digest = read_package(&dir).unwrap().digest;

        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let real = install(
            &elsewhere,
            "real",
            &manifest_yaml("cos72", "out_of_process_provider"),
        );
        // Control: the linked-to manifest really is byte-identical (same
        // digest) — a digest mismatch alone would also fail `recheck` and
        // prove nothing about the directory-level check specifically.
        assert_eq!(read_package(&real).unwrap().digest, digest);

        std::fs::remove_dir_all(&dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &dir).unwrap();

        let err = recheck(&dir, &digest).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
    }

    // K1-5.1 — `build_tool_registry` (ADR-K1-01 §2.1/§2.2/§5(2)).

    #[test]
    fn two_distinct_modules_tools_register_without_conflict() {
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            "sin90",
            &manifest_yaml_with_tool("sin90", "out_of_process_provider", "list-notes"),
        );
        install(
            root.path(),
            "cos72",
            &manifest_yaml_with_tool("cos72", "out_of_process_provider", "list-notes"),
        );

        let scan = scan(root.path());
        assert!(scan.refused.is_empty(), "{:?}", scan.refused);
        let (registry, refused) = build_tool_registry(&scan.found);
        assert!(refused.is_empty(), "{:?}", refused);
        assert!(registry.get("sin90.list-notes").is_some());
        assert!(registry.get("cos72.list-notes").is_some());
    }

    #[test]
    fn two_found_packages_sharing_a_module_name_do_not_overwrite_each_others_tools() {
        // `scan` does not dedupe by declared `name` — that is the mounter's
        // job, downstream. Two package DIRECTORIES can therefore both declare
        // `name: dup`; registering the second one's tools must be refused
        // without disturbing the first's.
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            "aaa-dup",
            &manifest_yaml_with_tool("dup", "out_of_process_provider", "list-notes"),
        );
        install(
            root.path(),
            "zzz-dup",
            &manifest_yaml_with_tool("dup", "out_of_process_provider", "list-notes"),
        );

        let scan = scan(root.path());
        assert!(scan.refused.is_empty(), "{:?}", scan.refused);
        assert_eq!(
            scan.found.len(),
            2,
            "both directories parse as valid packages"
        );

        let (registry, refused) = build_tool_registry(&scan.found);
        assert_eq!(
            registry.len(),
            1,
            "the second registration must not add or overwrite"
        );
        assert_eq!(refused.len(), 1);
        assert!(refused[0].why.contains("dup"), "{:?}", refused);
        assert_eq!(
            refused[0].dir.file_name().unwrap().to_string_lossy(),
            "zzz-dup",
            "the LATER package (by scan order) is the one refused, the earlier stays"
        );
    }

    /// THE real reflex case (ADR-K1-01 §3): two found packages share a module
    /// `name` ("dup") but declare DIFFERENT operations, so there is no
    /// full-name collision at all (`dup.list-notes` vs `dup.delete-all`).
    /// Before the module-level fix, a full-name-only check let BOTH through
    /// — the test above alone could not catch this, because its two packages
    /// happen to declare the SAME operation and so collide on the full name
    /// regardless of whether the check is module-level or name-level.
    #[test]
    fn two_found_packages_sharing_a_module_name_with_different_operations_are_still_refused() {
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            "aaa-dup",
            &manifest_yaml_with_tool("dup", "out_of_process_provider", "list-notes"),
        );
        install(
            root.path(),
            "zzz-dup",
            &manifest_yaml_with_tool("dup", "out_of_process_provider", "delete-all"),
        );

        let scan = scan(root.path());
        assert!(scan.refused.is_empty(), "{:?}", scan.refused);

        let (registry, refused) = build_tool_registry(&scan.found);
        assert_eq!(
            registry.len(),
            1,
            "the second package's non-colliding operation must still be refused \
             at the module level"
        );
        assert!(registry.get("dup.list-notes").is_some());
        assert!(
            registry.get("dup.delete-all").is_none(),
            "no full name collided, but the module name was already claimed"
        );
        assert_eq!(refused.len(), 1);
        assert_eq!(
            refused[0].dir.file_name().unwrap().to_string_lossy(),
            "zzz-dup"
        );
    }

    /// A fired token stops the scan before a broken sibling is recorded as
    /// damaged, and it does not discover the good package. The same tree
    /// with a live token still finds one and refuses the other — so this
    /// stays red if the cancel branch is removed. `recheck` then fails with
    /// `cancelled`, which is the signal its caller uses to not spawn.
    #[test]
    fn a_cancelled_scan_does_not_discover_or_damage_the_package() {
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            "cos72",
            &manifest_yaml_with_tool("cos72", "out_of_process_provider", "ping"),
        );
        install(root.path(), "broken", "this is not: [valid yaml\n");
        let cancel = CancellationToken::new();
        cancel.cancel();

        let scan = super::scan(root.path(), &cancel);
        assert!(
            scan.found.is_empty(),
            "cancelled package was discovered: {:?}",
            scan.found
                .iter()
                .map(|d| d.manifest.name())
                .collect::<Vec<_>>()
        );
        assert!(
            scan.refused.is_empty(),
            "cancel was recorded as damage: {:?}",
            scan.refused
        );
        let mut names: Vec<String> = scan
            .cancelled
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["broken".to_owned(), "cos72".to_owned()]);
        let (registry, refused) = build_tool_registry(&scan.found);
        assert_eq!(registry.len(), 0, "a cancelled package registered a tool");
        assert!(refused.is_empty(), "{refused:?}");

        let live = super::scan(root.path(), &CancellationToken::new());
        assert_eq!(live.found.len(), 1, "refused: {:?}", live.refused);
        assert_eq!(live.found[0].manifest.name(), "cos72");
        assert_eq!(live.refused.len(), 1, "{:?}", live.refused);
        assert!(
            live.refused[0].dir.ends_with("broken"),
            "{:?}",
            live.refused
        );
        assert!(live.cancelled.is_empty());

        let digest = live.found[0].digest.clone();
        let err = super::recheck(&live.found[0].dir, &digest, &cancel).unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(
            !err.contains("hash_mismatch"),
            "cancel must not look like a bad hash: {err}"
        );
        // The supervisor spawns the next run only when this returns Ok.
        assert!(super::recheck(&live.found[0].dir, &digest, &cancel).is_err());
    }

    /// The directory is already gone, and the token is already cancelled.
    /// That must be `cancelled`, not "no longer exists": the metadata
    /// branches return before `read_package`'s cancel check. Dropping the
    /// check at the top of `recheck` turns the first assertion red.
    #[test]
    fn recheck_of_a_missing_directory_while_cancelled_is_cancelled() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("gone");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = super::recheck(&missing, "sha256:ab", &cancel).unwrap_err();
        assert_eq!(err, "cancelled");
        let live = super::recheck(&missing, "sha256:ab", &CancellationToken::new()).unwrap_err();
        assert!(
            live.contains("no longer exists"),
            "a live token must still report the missing directory: {live}"
        );
    }

    /// Executable is a copy of this test binary. There is no native-format
    /// check on this branch; the copy only has to be a real file the hasher
    /// walks. `bin/{name}` is also `spawn.command`.
    fn integrity_package(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join(MANIFEST_FILE),
            format!(
                "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
                 event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
                 kernel_capabilities: [events]\nimpl_kind: out_of_process_provider\n\
                 spawn:\n  command: bin/{name}\n"
            ),
        )
        .unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), dir.join("bin").join(name)).unwrap();
        crate::package_integrity::generate(&dir, &[format!("bin/{name}")]).unwrap();
        dir
    }

    /// Pauses inside the first hashed chunk, then returns. Later chunks do
    /// not wait, so one `release` unblocks the walk.
    fn arm_hash_pause(
        prefix: &Path,
    ) -> (
        agent24_components::integrity::HashProgressGuard,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let once = Arc::new(AtomicBool::new(false));
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let guard = agent24_components::integrity::install_hash_progress_hook(
            prefix,
            Arc::new(move || {
                if once.swap(true, Ordering::SeqCst) {
                    return;
                }
                let _ = entered_tx.send(());
                if let Some(rx) = release_rx
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    let _ = rx.recv();
                }
            }),
        );
        (guard, entered_rx, release_tx)
    }

    /// Cancel after the hasher has read a chunk, not before it starts.
    /// The package must not become [`Discovered`], and recheck must not
    /// return `Ok` (that is what lets a supervisor spawn).
    #[test]
    fn a_cancel_during_the_hash_is_not_discovered() {
        let root = tempfile::tempdir().unwrap();
        let dir = integrity_package(root.path(), "cos72");
        let live = super::scan(root.path(), &CancellationToken::new());
        assert_eq!(live.found.len(), 1, "refused: {:?}", live.refused);
        let digest = live.found[0].digest.clone();

        let cancel = CancellationToken::new();
        let scan = {
            let (_guard, entered, release) = arm_hash_pause(&dir);
            let scan_root = root.path().to_owned();
            let scan_cancel = cancel.clone();
            let worker = std::thread::spawn(move || super::scan(&scan_root, &scan_cancel));
            entered
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("the startup scan never reached the hash");
            cancel.cancel();
            release.send(()).unwrap();
            worker.join().unwrap()
        };
        assert!(
            scan.found.is_empty(),
            "discovered after a mid-hash cancel: {:?}",
            scan.found
                .iter()
                .map(|d| d.manifest.name())
                .collect::<Vec<_>>()
        );
        assert!(
            scan.cancelled.iter().any(|path| path == &dir),
            "cancelled: {:?}",
            scan.cancelled
        );
        assert!(
            scan.refused
                .iter()
                .all(|r| !r.why.contains("hash_mismatch")),
            "{:?}",
            scan.refused
        );

        let cancel = CancellationToken::new();
        let (_guard, entered, release) = arm_hash_pause(&dir);
        let recheck_dir = dir.clone();
        let recheck_cancel = cancel.clone();
        let worker =
            std::thread::spawn(move || super::recheck(&recheck_dir, &digest, &recheck_cancel));
        entered
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("recheck never reached the hash");
        cancel.cancel();
        release.send(()).unwrap();
        let err = worker.join().unwrap().unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!err.contains("hash_mismatch"), "{err}");
    }
}
