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
#[must_use]
pub fn manifest_digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for b in hash {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

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
#[derive(Debug, Default)]
pub struct Scan {
    pub found: Vec<Discovered>,
    pub refused: Vec<Refused>,
}

/// Read every package directly under `root`.
///
/// `root` missing is not a refusal — a daemon with no third-party modules is the
/// normal case, and reporting it as a problem would train operators to ignore the
/// report. An unreadable `root` IS reported, because that is a real difference
/// from "empty" that the operator can act on. (Distinguishing those two is the
/// same rule FU-32 is about: an empty answer and a broken channel must not look
/// alike.)
pub fn scan(root: &Path) -> Scan {
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

    for dir in dirs {
        match read_package(&dir) {
            Ok(d) => out.found.push(d),
            Err(why) => out.refused.push(Refused { dir, why }),
        }
    }
    out
}

/// Read and validate one package directory.
fn read_package(dir: &Path) -> std::result::Result<Discovered, String> {
    let path = dir.join(MANIFEST_FILE);

    // A symlinked manifest is refused rather than followed. The kernel derives a
    // module's data directory from its validated name, and following a link here
    // would let a package point the manifest — the thing that DECIDES that name —
    // at a file outside its own directory.
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("no readable {MANIFEST_FILE}: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{MANIFEST_FILE} is a symlink; refusing to follow it"
        ));
    }
    if !meta.is_file() {
        return Err(format!("{MANIFEST_FILE} is not a regular file"));
    }
    // Bound the read BEFORE it happens. `from_yaml` also checks a size limit, but
    // it checks it on a string that has already been read into memory.
    if meta.len() > DomainOsManifest::MAX_YAML_BYTES as u64 {
        return Err(format!(
            "{MANIFEST_FILE} is {} bytes, over the {} byte limit",
            meta.len(),
            DomainOsManifest::MAX_YAML_BYTES
        ));
    }

    // Read BYTES, not a string. `read_to_string` would fail on non-UTF-8 with
    // "stream did not contain valid UTF-8" and no hint of which file — and a
    // UTF-16 manifest (a real Windows accident, with a FF FE BOM) is exactly that.
    // Decoding here lets each case have its own readable reason.
    let bytes = std::fs::read(&path).map_err(|e| format!("could not read {MANIFEST_FILE}: {e}"))?;
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(format!(
            "{MANIFEST_FILE} is UTF-16 (byte-order mark {:02X} {:02X}); it must be UTF-8",
            bytes[0], bytes[1]
        ));
    }
    let digest = manifest_digest(&bytes);
    let text =
        String::from_utf8(bytes).map_err(|e| format!("{MANIFEST_FILE} is not valid UTF-8: {e}"))?;

    let manifest = DomainOsManifest::from_yaml(&text).map_err(|e| e.to_string())?;
    Ok(Discovered {
        manifest,
        dir: dir.to_owned(),
        digest,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::io::Write;

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

    fn install(root: &Path, name: &str, body: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join(MANIFEST_FILE)).unwrap();
        f.write_all(body.as_bytes()).unwrap();
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
        let scan = scan(root.path());
        assert_eq!(scan.found[0].digest, manifest_digest(body.as_bytes()));
        // One byte more is another manifest, and another digest.
        std::fs::write(dir.join(MANIFEST_FILE), format!("{body}\n")).unwrap();
        assert_ne!(scan_digest(root.path()), manifest_digest(body.as_bytes()));
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
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(manifest_yaml("bom", "out_of_process_provider").as_bytes());
        std::fs::write(dir.join(MANIFEST_FILE), &bytes).unwrap();

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
}
