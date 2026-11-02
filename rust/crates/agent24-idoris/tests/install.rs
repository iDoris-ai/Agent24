#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use agent24_idoris::{
    IdorisLock,
    download::VerifiedArchive,
    install::{
        InstallError, InstallOptions, SignatureVerifier, SigningRequirement,
        install_locked_asset as install_async,
    },
};
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
};
use tempfile::TempDir;

fn install_locked_asset(
    lock: &IdorisLock,
    triple: &str,
    archive: &VerifiedArchive,
    opts: &InstallOptions,
    signature: impl SignatureVerifier + Send + 'static,
) -> Result<agent24_idoris::install::InstalledVersion, InstallError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(install_async(
            lock,
            triple,
            archive,
            opts,
            signature,
            &tokio_util::sync::CancellationToken::new(),
        ))
}

const TRIPLE: &str = "aarch64-apple-darwin";
struct Signature(bool);
impl SignatureVerifier for Signature {
    fn verify(&self, _: &Path, required: &SigningRequirement) -> Result<(), InstallError> {
        assert_eq!(required.team_id, "HYAP444FN9");
        if self.0 {
            Ok(())
        } else {
            Err(InstallError::SignatureMismatch)
        }
    }
}
struct Fixture {
    dir: TempDir,
    lock: IdorisLock,
    archive: VerifiedArchive,
    opts: InstallOptions,
}
impl Fixture {
    fn new(bad: Option<(&str, u8, u64)>) -> Self {
        Self::with_pax(bad, None)
    }
    fn with_pax(bad: Option<(&str, u8, u64)>, pax: Option<&str>) -> Self {
        let dir = TempDir::new().unwrap();
        let mut lock = IdorisLock::parse(include_str!("../idoris.lock.json")).unwrap();
        let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for name in &lock.assets[TRIPLE].files {
            if let Some(key) = pax {
                tar.append_pax_extensions([(key, b"ignored".as_slice())])
                    .unwrap();
            }
            if name == "idoris" {
                let data = b"#!/bin/sh\nexit 0\n";
                let mut header = tar::Header::new_gnu();
                header.set_path(name).unwrap();
                header.set_size(data.len() as u64);
                header.set_mode(0o777);
                header.set_cksum();
                tar.append(&header, data.as_slice()).unwrap();
            } else {
                append(&mut tar, name, b'0', 7);
            }
        }
        if let Some((name, kind, size)) = bad {
            append(&mut tar, name, kind, size);
        }
        let bytes = tar.into_inner().unwrap().finish().unwrap();
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        lock.assets.get_mut(TRIPLE).unwrap().sha256 = sha256.clone();
        let path = dir.path().join("archive.tar.gz");
        fs::write(&path, bytes).unwrap();
        let opts = InstallOptions::new(
            dir.path().join("install"),
            env!("CARGO_BIN_EXE_idoris-install-child"),
        );
        Self {
            dir,
            lock,
            archive: VerifiedArchive { path, sha256 },
            opts,
        }
    }
    fn install(&self, signature: bool) -> Result<std::path::PathBuf, InstallError> {
        self.install_with(Signature(signature))
    }
    fn install_with(
        &self,
        signature: impl SignatureVerifier + Send + 'static,
    ) -> Result<std::path::PathBuf, InstallError> {
        install_locked_asset(&self.lock, TRIPLE, &self.archive, &self.opts, signature)
            .map(|installed| installed.directory().to_owned())
    }
    fn clean(&self) {
        assert!(!self.opts.root.join("0.4.0").exists());
        assert!(!fs::read_dir(&self.opts.root).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".staging-")
        }));
    }
}
fn append<W: io::Write>(tar: &mut tar::Builder<W>, name: &str, kind: u8, size: u64) {
    let mut header = tar::Header::new_gnu();
    // Set raw bytes so malformed paths reach the installer rather than Builder's guard.
    header.as_mut_bytes()[..100].fill(0);
    header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
    header.set_entry_type(tar::EntryType::new(kind));
    header.set_mode(0o777);
    header.set_size(size);
    if matches!(kind, b'1' | b'2') {
        header.set_link_name("idoris").unwrap();
    }
    header.set_cksum();
    tar.append(&header, vec![b'x'; size as usize].as_slice())
        .unwrap();
}

#[test]
fn state_dirs_are_created_private_and_apart_from_releases() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("state");
    agent24_idoris::install::prepare_state_dirs(&root).unwrap();
    for name in ["", "data", "instances"] {
        assert_eq!(
            fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o7777,
            0o700
        );
    }
    // Idempotent, and an install root never grows state directories.
    agent24_idoris::install::prepare_state_dirs(&root).unwrap();
    let f = Fixture::new(None);
    f.install(true).unwrap();
    assert!(!f.opts.root.join("data").exists() && !f.opts.root.join("instances").exists());
}

#[test]
fn success_permissions_and_existing_version_reverification() {
    let f = Fixture::new(None);
    let installed = f.install(true).unwrap();
    for (path, mode) in [
        ("", 0o700),
        ("0.4.0", 0o700),
        ("0.4.0/config", 0o700),
        ("0.4.0/config/components", 0o700),
        ("0.4.0/idoris", 0o500),
        ("0.4.0/config/catalog.yaml", 0o400),
    ] {
        assert_eq!(
            fs::metadata(f.opts.root.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            mode,
            "{path}"
        );
    }
    assert_eq!(f.install(true).unwrap(), installed);
    fs::set_permissions(installed.join("idoris"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(installed.join("idoris"), b"changed").unwrap();
    assert!(matches!(
        f.install(true),
        Err(InstallError::HashMismatch(_))
    ));
    assert_eq!(fs::read(installed.join("idoris")).unwrap(), b"changed");
}

fn reject_entry(path: &str, kind: u8, size: u64) {
    let f = Fixture::new(Some((path, kind, size)));
    assert!(
        matches!(f.install(true), Err(InstallError::ManifestViolation(_))),
        "{path}"
    );
    f.clean();
    assert!(!f.dir.path().join("escape").exists());
    // Also call the actual child directly, bypassing the parent's preflight.
    let staging = f.dir.path().join("child-staging");
    fs::create_dir(&staging).unwrap();
    let reply = child_request(
        &f,
        &staging,
        &f.lock.assets[TRIPLE].files,
        f.opts.max_unpacked_bytes,
    );
    assert!(!reply.status.success());
    let reply: serde_json::Value = serde_json::from_slice(&reply.stdout).unwrap();
    assert!(reply.get("ManifestViolation").is_some(), "{reply}");
    assert!(!f.dir.path().join("escape").exists());
}
fn child_request(
    f: &Fixture,
    staging: &Path,
    files: &[String],
    limit: u64,
) -> std::process::Output {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let request = serde_json::json!({ "sha256": f.archive.sha256, "staging": fs::canonicalize(staging).unwrap(), "files": files, "limit": limit });
    let mut child = Command::new(env!("CARGO_BIN_EXE_idoris-install-child"))
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let request = request.to_string();
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    input.write_all(request.as_bytes()).unwrap();
    input
        .write_all(&fs::read(&f.archive.path).unwrap())
        .unwrap();
    drop(input);
    child.wait_with_output().unwrap()
}
macro_rules! rejected_tar {
    ($test:ident, $path:expr, $kind:expr, $size:expr) => {
        #[test]
        fn $test() {
            reject_entry($path, $kind, $size);
        }
    };
}
rejected_tar!(tar_parent_traversal_rejected, "../escape", b'0', 1);
rejected_tar!(tar_absolute_path_rejected, "/absolute", b'0', 1);
rejected_tar!(tar_symlink_rejected, "sym", b'2', 0);
rejected_tar!(tar_hardlink_rejected, "hard", b'1', 0);
rejected_tar!(tar_duplicate_path_rejected, "idoris", b'0', 7);
rejected_tar!(tar_extra_file_rejected, "extra", b'0', 1);
rejected_tar!(tar_device_rejected, "device", b'3', 0);
rejected_tar!(tar_path_alias_rejected, "config/../idoris", b'0', 7);

#[test]
fn unpack_limit_rejected() {
    let mut f = Fixture::new(None);
    f.opts.max_unpacked_bytes = 16;
    assert!(matches!(
        f.install(true),
        Err(InstallError::ManifestViolation(_))
    ));
    f.clean();
}

#[test]
fn signature_failure_rolls_back_new_but_preserves_existing_version() {
    let f = Fixture::new(None);
    assert!(matches!(
        f.install(false),
        Err(InstallError::SignatureMismatch)
    ));
    f.clean();
    let installed = f.install(true).unwrap();
    assert!(matches!(
        f.install(false),
        Err(InstallError::SignatureMismatch)
    ));
    assert!(installed.exists());
}

/// Verifier that records whether the final version directory exists while it runs.
struct WatchesFinalDir {
    final_dir: std::path::PathBuf,
    saw_final: std::sync::Mutex<Vec<bool>>,
}
impl SignatureVerifier for WatchesFinalDir {
    fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
        self.saw_final.lock().unwrap().push(self.final_dir.exists());
        Err(InstallError::SignatureMismatch)
    }
}

#[test]
fn a_failing_signature_never_creates_the_final_directory() {
    let f = Fixture::new(None);
    let watcher = std::sync::Arc::new(WatchesFinalDir {
        final_dir: f.opts.root.join("0.4.0"),
        saw_final: Default::default(),
    });
    struct Shared(std::sync::Arc<WatchesFinalDir>);
    impl SignatureVerifier for Shared {
        fn verify(&self, p: &Path, r: &SigningRequirement) -> Result<(), InstallError> {
            self.0.verify(p, r)
        }
    }
    let result = f.install_with(Shared(watcher.clone()));
    assert!(matches!(result, Err(InstallError::SignatureMismatch)));
    // Verified once, inside staging, before the rename: no final directory then or after.
    assert_eq!(*watcher.saw_final.lock().unwrap(), [false]);
    assert!(!f.opts.root.join("0.4.0").exists());
    f.clean();
}

/// Gatekeeper unreachable: any demand for a notarization verdict fails.
struct OfflineGatekeeper;
impl SignatureVerifier for OfflineGatekeeper {
    fn verify(&self, _: &Path, required: &SigningRequirement) -> Result<(), InstallError> {
        if required.notarized {
            Err(InstallError::NotNotarized)
        } else {
            Ok(())
        }
    }
}

#[test]
fn install_demands_notarization_and_rejects_when_gatekeeper_is_unreachable() {
    let f = Fixture::new(None);
    assert!(f.lock.component_notarized());
    assert!(matches!(
        f.install_with(OfflineGatekeeper),
        Err(InstallError::NotNotarized)
    ));
    f.clean();
}

#[test]
fn child_failure_cleans_staging() {
    let mut f = Fixture::new(None);
    f.opts.child_program = "/usr/bin/false".into();
    assert!(matches!(f.install(true), Err(InstallError::ChildFailed(_))));
    f.clean();
}

#[test]
fn operation_lock_is_busy_for_second_installer() {
    let f = Fixture::new(None);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&f.opts.root)
        .unwrap();
    let lock = fs::File::create(f.opts.root.join(".operation.lock")).unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    assert!(matches!(f.install(true), Err(InstallError::Busy)));
}

#[test]
fn insufficient_and_unknown_space_are_rejected() {
    let mut f = Fixture::new(None);
    f.opts.available_space = |_| Ok(0);
    assert!(matches!(
        f.install(true),
        Err(InstallError::InsufficientSpace { .. })
    ));
    f.clean();
    f.opts.available_space = |_| Err(io::Error::other("unknown volume"));
    assert!(matches!(
        f.install(true),
        Err(InstallError::SpaceUnknown(_))
    ));
    f.clean();
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "downloads and installs signed iDoris v0.4.0; runs real Apple codesign"]
async fn real_v0_4_0_download_install_and_l6() {
    use agent24_idoris::{
        download::{DownloadOptions, download_locked_asset},
        install::Codesign,
    };
    use tokio_util::sync::CancellationToken;
    let dir = TempDir::new().unwrap();
    let lock = IdorisLock::embedded();
    let archive = download_locked_asset(
        lock,
        TRIPLE,
        &dir.path().join("components/idoris/cache"),
        &DownloadOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let opts = InstallOptions::new(
        dir.path().join("components/idoris"),
        env!("CARGO_BIN_EXE_idoris-install-child"),
    );
    let installed = install_async(
        lock,
        TRIPLE,
        &archive,
        &opts,
        Codesign,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    Codesign
        .verify(
            &installed.binary(),
            &SigningRequirement {
                team_id: &lock.team_id,
                notarized: true,
            },
        )
        .unwrap();
    let again = install_async(
        lock,
        TRIPLE,
        &archive,
        &opts,
        Codesign,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(again.directory(), installed.directory());
    eprintln!(
        "PASS: v0.4.0 archive {}, five files installed, strict requirement + TeamIdentifier={}, existing version reverified",
        archive.sha256, lock.team_id
    );
}

#[test]
fn release_pax_provenance_is_ignored_without_applying_xattrs() {
    let f = Fixture::with_pax(None, Some("SCHILY.xattr.com.apple.provenance"));
    f.install(true).unwrap();
}

#[test]
fn pax_path_link_size_and_sparse_overrides_are_rejected() {
    for key in ["path", "linkpath", "size", "GNU.sparse.map"] {
        let f = Fixture::with_pax(None, Some(key));
        assert!(
            matches!(f.install(true), Err(InstallError::ManifestViolation(_))),
            "{key}"
        );
        f.clean();
    }
}

#[test]
fn child_independently_enforces_unpacked_limit() {
    let f = Fixture::new(None);
    let staging = f.dir.path().join("child-staging");
    fs::create_dir(&staging).unwrap();
    let reply = child_request(&f, &staging, &f.lock.assets[TRIPLE].files, 16);
    assert!(!reply.status.success());
    let reply: serde_json::Value = serde_json::from_slice(&reply.stdout).unwrap();
    assert!(reply.get("ManifestViolation").is_some());
}

#[test]
fn changed_archive_is_rehashed_in_parent_and_child() {
    let f = Fixture::new(None);
    fs::write(&f.archive.path, b"tampered").unwrap();
    assert!(matches!(
        f.install(true),
        Err(InstallError::HashMismatch(_))
    ));
    f.clean();
    let staging = f.dir.path().join("child-staging");
    fs::create_dir(&staging).unwrap();
    let reply = child_request(
        &f,
        &staging,
        &f.lock.assets[TRIPLE].files,
        f.opts.max_unpacked_bytes,
    );
    assert!(!reply.status.success());
    let reply: serde_json::Value = serde_json::from_slice(&reply.stdout).unwrap();
    assert!(reply.get("HashMismatch").is_some());
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
}

#[test]
fn parent_rehashes_child_files_after_successful_exit() {
    let mut f = Fixture::new(None);
    f.opts.child_program = "/bin/sh".into();
    f.opts.child_args = vec![
        "-c".into(),
        "\"$1\"; printf changed > idoris".into(),
        "_".into(),
        env!("CARGO_BIN_EXE_idoris-install-child").into(),
    ];
    assert!(matches!(
        f.install(true),
        Err(InstallError::HashMismatch(_))
    ));
    f.clean();
}

#[test]
fn failed_child_partial_output_is_removed_and_lock_released() {
    let mut f = Fixture::new(None);
    let program = f.opts.child_program.clone();
    f.opts.child_program = "/bin/sh".into();
    f.opts.child_args = vec!["-c".into(), "printf partial > idoris; exit 9".into()];
    assert!(matches!(f.install(true), Err(InstallError::ChildFailed(_))));
    f.clean();
    f.opts.child_program = program;
    f.opts.child_args.clear();
    f.install(true).unwrap();
}

#[test]
fn environment_credentials_are_not_inherited() {
    let mut f = Fixture::new(None);
    f.opts.child_program = "/bin/sh".into();
    f.opts.child_args = vec![
        "-c".into(),
        "test -z \"${HOME+x}${AWS_SECRET_ACCESS_KEY+x}\" || exit 9; exec \"$1\"".into(),
        "_".into(),
        env!("CARGO_BIN_EXE_idoris-install-child").into(),
    ];
    f.install(true).unwrap();
}

#[test]
fn existing_symlinks_extra_files_and_missing_files_are_rejected() {
    for change in ["symlink", "extra", "missing", "hardlink", "directory"] {
        let f = Fixture::new(None);
        let installed = f.install(true).unwrap();
        match change {
            "symlink" => {
                fs::remove_file(installed.join("idoris")).unwrap();
                std::os::unix::fs::symlink(&f.archive.path, installed.join("idoris")).unwrap();
            }
            "extra" => fs::write(installed.join("extra"), b"extra").unwrap(),
            "missing" => fs::remove_file(installed.join("config/catalog.yaml")).unwrap(),
            "hardlink" => {
                fs::hard_link(installed.join("idoris"), f.dir.path().join("alias")).unwrap()
            }
            "directory" => fs::create_dir(installed.join("extra")).unwrap(),
            _ => unreachable!(),
        }
        assert!(
            matches!(f.install(true), Err(InstallError::ManifestViolation(_))),
            "{change}"
        );
        assert!(installed.exists());
    }
}

#[test]
fn unsupported_platform_fails_before_creating_install_root() {
    struct Unsupported;
    impl SignatureVerifier for Unsupported {
        fn check_platform(&self) -> Result<(), InstallError> {
            Err(InstallError::UnsupportedPlatform)
        }
        fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
            panic!("must not run");
        }
    }
    let f = Fixture::new(None);
    assert!(matches!(
        install_locked_asset(&f.lock, TRIPLE, &f.archive, &f.opts, Unsupported),
        Err(InstallError::UnsupportedPlatform)
    ));
    assert!(!f.opts.root.exists());
}

#[test]
fn concurrent_second_installer_gets_busy() {
    use std::{sync::mpsc, time::Duration};
    struct BlockingSignature {
        ready: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl SignatureVerifier for BlockingSignature {
        fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
            self.ready.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok(())
        }
    }
    let f = Fixture::new(None);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            install_locked_asset(
                &f.lock,
                TRIPLE,
                &f.archive,
                &f.opts,
                BlockingSignature {
                    ready: ready_tx,
                    release: release_rx,
                },
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let second = f.install(true);
        // Verification runs twice per install: in staging, then at the final path.
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        assert!(matches!(second, Err(InstallError::Busy)));
        first.join().unwrap().unwrap();
    });
}

#[test]
fn installed_binary_executes_immediately_during_parallel_spawns() {
    use std::process::Command;
    let f = Fixture::new(None);
    std::thread::scope(|scope| {
        let other_spawns = scope.spawn(|| {
            for _ in 0..32 {
                assert!(Command::new("/usr/bin/true").status().unwrap().success());
            }
        });
        let installed = f.install(true).unwrap();
        for _ in 0..16 {
            assert!(
                Command::new(installed.join("idoris"))
                    .status()
                    .unwrap()
                    .success()
            );
        }
        other_spawns.join().unwrap();
    });
}

#[test]
fn forged_child_manifest_is_rejected() {
    let mut f = Fixture::new(None);
    f.opts.child_program = "/bin/sh".into();
    f.opts.child_args = vec![
        "-c".into(),
        "reply=$(\"$1\"); printf '%s' '{\"Ok\":{}}'".into(),
        "_".into(),
        env!("CARGO_BIN_EXE_idoris-install-child").into(),
    ];
    assert!(matches!(
        f.install(true),
        Err(InstallError::HashMismatch(_))
    ));
    f.clean();
}

#[test]
fn archive_symlink_is_rejected_without_following_target() {
    let mut f = Fixture::new(None);
    let link = f.dir.path().join("archive-link");
    std::os::unix::fs::symlink(&f.archive.path, &link).unwrap();
    f.archive.path = link;
    assert!(f.install(true).is_err());
    f.clean();
}

#[test]
fn replacing_archive_path_after_open_does_not_change_installed_bytes() {
    let mut f = Fixture::new(None);
    f.opts.available_space = |root| {
        let archive = root.parent().unwrap().join("archive.tar.gz");
        fs::rename(&archive, archive.with_extension("old"))?;
        fs::write(archive, b"replacement bytes")?;
        Ok(u64::MAX)
    };
    let installed = f.install(true).unwrap();
    assert_eq!(
        fs::read(installed.join("idoris")).unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
}

#[test]
fn modifying_archive_inode_after_verification_does_not_change_pipe_bytes() {
    let mut f = Fixture::new(None);
    f.opts.available_space = |root| {
        fs::write(
            root.parent().unwrap().join("archive.tar.gz"),
            b"in-place replacement",
        )?;
        Ok(u64::MAX)
    };
    let installed = f.install(true).unwrap();
    assert_eq!(
        fs::read(installed.join("idoris")).unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
}

#[tokio::test]
async fn install_disk_work_keeps_runtime_responsive_and_cancellation_releases_lock() {
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    let mut f = Fixture::new(None);
    f.opts.available_space = wait_for_space_release;
    let cancel = CancellationToken::new();
    let install = install_async(
        &f.lock,
        TRIPLE,
        &f.archive,
        &f.opts,
        Signature(true),
        &cancel,
    );
    tokio::pin!(install);
    let started = async {
        while !f.opts.root.join("space-started").exists() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    };
    tokio::select! {
        _ = tokio::time::timeout(Duration::from_secs(2), started) => (),
        result = &mut install => panic!("installation completed before runtime could cancel: {result:?}"),
    }
    assert!(f.opts.root.join("space-started").exists());
    cancel.cancel();
    fs::write(f.opts.root.join("space-release"), b"").unwrap();
    assert!(matches!(install.await, Err(InstallError::Cancelled)));
    f.clean();
    let held = fs::File::open(f.opts.root.join(".operation.lock")).unwrap();
    fs2::FileExt::try_lock_exclusive(&held).unwrap();
}

fn wait_for_space_release(root: &Path) -> io::Result<u64> {
    use std::time::{Duration, Instant};
    fs::write(root.join("space-started"), b"")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("space-release").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(u64::MAX)
}

#[tokio::test]
async fn dropped_install_future_holds_lock_until_disk_worker_cleans_up() {
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    let mut f = Fixture::new(None);
    f.opts.available_space = wait_for_space_release;
    let cancel = CancellationToken::new();
    let mut install = Box::pin(install_async(
        &f.lock,
        TRIPLE,
        &f.archive,
        &f.opts,
        Signature(true),
        &cancel,
    ));
    let started = async {
        while !f.opts.root.join("space-started").exists() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    };
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(2), started) => result.unwrap(),
        result = &mut install => panic!("installation returned early: {result:?}"),
    }
    drop(install);
    let lock = fs::File::open(f.opts.root.join(".operation.lock")).unwrap();
    assert!(fs2::FileExt::try_lock_exclusive(&lock).is_err());
    fs::write(f.opts.root.join("space-release"), b"").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while fs2::FileExt::try_lock_exclusive(&lock).is_err() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    f.clean();
    assert!(
        !cancel.is_cancelled(),
        "dropping the operation must not cancel its caller"
    );
}

#[tokio::test]
async fn cancellation_reaps_an_unresponsive_install_child_and_removes_staging() {
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    let mut f = Fixture::new(None);
    f.opts.child_program = "/bin/sh".into();
    f.opts.child_args = vec![
        "-c".into(),
        "printf ready > ../child-ready; while test ! -f ../child-release; do :; done".into(),
    ];
    let cancel = CancellationToken::new();
    let install = install_async(
        &f.lock,
        TRIPLE,
        &f.archive,
        &f.opts,
        Signature(true),
        &cancel,
    );
    tokio::pin!(install);
    let ready = async {
        while !f.opts.root.join("child-ready").exists() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    };
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(2), ready) => result.unwrap(),
        result = &mut install => panic!("child did not wait: {result:?}"),
    }
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), &mut install).await;
    if result.is_err() {
        fs::write(f.opts.root.join("child-release"), b"").unwrap();
        let _ = (&mut install).await;
    }
    assert!(matches!(result, Ok(Err(InstallError::Cancelled))));
    f.clean();
    let lock = fs::File::open(f.opts.root.join(".operation.lock")).unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
}
