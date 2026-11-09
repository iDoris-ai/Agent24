//! Signature policy supplied by the host, independent of package metadata.
use agent24_components::{
    integrity::IntegrityError,
    sign::{Codesign, SignatureVerifier, SigningRequirement, VerifyMode},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Policy {
    pub verifier: Arc<dyn SignatureVerifier + Send + Sync>,
    pub trust_file: Option<PathBuf>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            verifier: Arc::new(Codesign),
            trust_file: crate::trusted_teams::default_path(),
        }
    }
}
impl Policy {
    pub fn verify(
        &self,
        root: &Path,
        executable: &str,
        mode: VerifyMode,
        cancel: &CancellationToken,
    ) -> Result<(), IntegrityError> {
        self.verify_platform(root, executable, mode, cancel, std::env::consts::OS)
    }
    fn verify_platform(
        &self,
        root: &Path,
        executable: &str,
        mode: VerifyMode,
        cancel: &CancellationToken,
        platform: &str,
    ) -> Result<(), IntegrityError> {
        self.verify_platform_with_adhoc(
            root,
            executable,
            mode,
            cancel,
            platform,
            accept_adhoc_signatures(),
        )
    }
    fn verify_platform_with_adhoc(
        &self,
        root: &Path,
        executable: &str,
        mode: VerifyMode,
        cancel: &CancellationToken,
        platform: &str,
        _accept_adhoc: bool,
    ) -> Result<(), IntegrityError> {
        if cancel.is_cancelled() {
            return Err(IntegrityError::Cancelled);
        }
        if platform != "macos" {
            return Ok(());
        }
        let checked = (|| {
            let path = root.join(executable);
            let invalid = || IntegrityError::SignatureInvalid {
                path: executable.to_owned(),
            };
            let team = match self.verifier.team_id(&path) {
                Ok(team) => team,
                #[cfg(debug_assertions)]
                Err(_) if _accept_adhoc => {
                    return self.verifier.verify_adhoc(&path).map_err(|_| invalid());
                }
                Err(_) => return Err(invalid()),
            };
            if !agent24_os_proc::valid_team_id(&team) {
                return Err(invalid());
            }
            let teams = self
                .trust_file
                .as_deref()
                .map(crate::trusted_teams::load)
                .unwrap_or_else(crate::trusted_teams::builtin);
            if !teams.contains(&team) {
                return Err(IntegrityError::UntrustedTeam {
                    path: executable.to_owned(),
                    team,
                });
            }
            if cancel.is_cancelled() {
                return Err(IntegrityError::Cancelled);
            }
            self.verifier
                .verify(
                    &path,
                    &SigningRequirement {
                        team_id: &team,
                        notarized: mode == VerifyMode::Install,
                    },
                )
                .map_err(|error| match error {
                    agent24_components::install::InstallError::NotNotarized
                    | agent24_components::install::InstallError::NotarizationCheckFailed(_) => {
                        IntegrityError::NotarizationFailed {
                            path: executable.to_owned(),
                        }
                    }
                    _ => invalid(),
                })
        })();
        if cancel.is_cancelled() {
            Err(IntegrityError::Cancelled)
        } else {
            checked
        }
    }
}

// macOS CI cannot Developer-ID-sign its test stubs. Real-binary black-box
// tests explicitly opt into verified ad-hoc signatures. This switch and its
// environment-variable name must not exist in release binaries.
fn accept_adhoc_signatures() -> bool {
    #[cfg(debug_assertions)]
    {
        std::env::var_os("A24_TEST_ACCEPT_ADHOC_SIGNATURES").is_some_and(|value| value == "1")
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use agent24_components::install::InstallError;
    struct Fake {
        team: Option<&'static str>,
        valid: bool,
    }
    impl SignatureVerifier for Fake {
        fn team_id(&self, _: &Path) -> Result<String, InstallError> {
            self.team
                .map(str::to_owned)
                .ok_or(InstallError::SignatureMismatch)
        }
        fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
            if self.valid {
                Ok(())
            } else {
                Err(InstallError::SignatureMismatch)
            }
        }
    }
    fn check(policy: &Policy) -> Result<(), IntegrityError> {
        policy.verify_platform(
            Path::new("/package"),
            "bin/run",
            VerifyMode::AtRest,
            &CancellationToken::new(),
            "macos",
        )
    }
    #[test]
    fn adhoc_opt_in_does_not_trust_foreign_unsigned_or_damaged_code() {
        struct Fixture {
            team: Option<&'static str>,
            adhoc: bool,
            valid: bool,
        }
        impl SignatureVerifier for Fixture {
            fn team_id(&self, _: &Path) -> Result<String, InstallError> {
                self.team
                    .map(str::to_owned)
                    .ok_or(InstallError::SignatureMismatch)
            }
            #[cfg(debug_assertions)]
            fn verify_adhoc(&self, _: &Path) -> Result<(), InstallError> {
                if self.adhoc && self.valid {
                    Ok(())
                } else {
                    Err(InstallError::SignatureMismatch)
                }
            }
            fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
                if self.valid {
                    Ok(())
                } else {
                    Err(InstallError::SignatureMismatch)
                }
            }
        }
        for enabled in [false, true] {
            for (team, adhoc, valid, trusted) in [
                (None, true, true, false),
                (None, false, true, false),
                (None, true, false, false),
                (Some("ABCDE12345"), false, true, false),
                (Some("HYAP444FN9"), false, true, true),
                (Some("HYAP444FN9"), false, false, false),
            ] {
                let policy = Policy {
                    verifier: Arc::new(Fixture { team, adhoc, valid }),
                    trust_file: None,
                };
                let result = policy.verify_platform_with_adhoc(
                    Path::new("/package"),
                    "run",
                    VerifyMode::AtRest,
                    &CancellationToken::new(),
                    "macos",
                    enabled,
                );
                assert_eq!(
                    result.is_ok(),
                    trusted || (cfg!(debug_assertions) && enabled && adhoc && valid),
                    "enabled={enabled}, team={team:?}, adhoc={adhoc}, valid={valid}"
                );
            }
        }
    }
    #[cfg(not(debug_assertions))]
    #[test]
    fn release_never_reads_adhoc_environment() {
        assert!(!accept_adhoc_signatures());
    }
    #[test]
    fn untrusted_team_is_refused_then_trust_and_untrust_take_effect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::trusted_teams::FILE_NAME);
        let policy = Policy {
            verifier: Arc::new(Fake {
                team: Some("ABCDE12345"),
                valid: true,
            }),
            trust_file: Some(path.clone()),
        };
        assert!(
            check(&policy)
                .unwrap_err()
                .to_string()
                .contains("ABCDE12345")
        );
        crate::trusted_teams::update(&path, "ABCDE12345", true).unwrap();
        check(&policy).unwrap();
        crate::trusted_teams::update(&path, "ABCDE12345", false).unwrap();
        assert!(check(&policy).is_err());
        std::fs::write(&path, b"broken").unwrap();
        assert!(check(&policy).is_err());
    }
    #[test]
    fn adhoc_unsigned_and_invalid_signature_are_refused() {
        for (team, valid) in [(None, true), (Some("HYAP444FN9"), false)] {
            let policy = Policy {
                verifier: Arc::new(Fake { team, valid }),
                trust_file: None,
            };
            assert!(check(&policy).is_err());
        }
    }
    #[test]
    fn builtin_passes_and_other_platforms_do_not_call_verifier() {
        let policy = Policy {
            verifier: Arc::new(Fake {
                team: Some("HYAP444FN9"),
                valid: true,
            }),
            trust_file: None,
        };
        check(&policy).unwrap();
        struct Panic;
        impl SignatureVerifier for Panic {
            fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
                panic!("must not verify")
            }
            fn team_id(&self, _: &Path) -> Result<String, InstallError> {
                panic!("must not read team")
            }
        }
        let policy = Policy {
            verifier: Arc::new(Panic),
            trust_file: None,
        };
        for platform in ["linux", "windows"] {
            policy
                .verify_platform(
                    Path::new("/"),
                    "run",
                    VerifyMode::Install,
                    &CancellationToken::new(),
                    platform,
                )
                .unwrap();
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use agent24_components::install::InstallError;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct Recorder {
        calls: Mutex<Vec<bool>>,
        reject: AtomicBool,
    }
    impl SignatureVerifier for Recorder {
        fn team_id(&self, _: &Path) -> Result<String, InstallError> {
            Ok("HYAP444FN9".into())
        }
        fn verify(&self, _: &Path, required: &SigningRequirement) -> Result<(), InstallError> {
            self.calls.lock().unwrap().push(required.notarized);
            if self.reject.load(Ordering::SeqCst) {
                Err(InstallError::NotNotarized)
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn legacy_manifest_cannot_skip_host_signature_policy() {
        let root = tempfile::tempdir().unwrap();
        let manifest = agent24_domain::DomainOsManifest::from_yaml("name: probe\nversion: '1.0.0'\nroute_namespace: /api/v1/probe\nevent_module: probe\ndata_dir: ~/.agent24/os/probe/\nimpl_kind: out_of_process_provider\nspawn:\n  command: bin/run\n").unwrap();
        let recorder = Arc::new(Recorder {
            calls: Mutex::new(Vec::new()),
            reject: AtomicBool::new(true),
        });
        let policy = Policy {
            verifier: recorder.clone(),
            trust_file: None,
        };
        assert!(
            crate::package_integrity::verify_with_policy(
                root.path(),
                &manifest,
                &CancellationToken::new(),
                &policy,
                VerifyMode::AtRest
            )
            .is_err()
        );
        assert_eq!(*recorder.calls.lock().unwrap(), [false]);
    }
    #[cfg(debug_assertions)]
    #[test]
    fn real_codesign_accepts_only_valid_adhoc_with_opt_in() {
        let root = tempfile::tempdir().unwrap();
        crate::test_support::native_exit(root.path(), "run").unwrap();
        let policy = Policy {
            verifier: Arc::new(Codesign),
            trust_file: None,
        };
        let check = |enabled| {
            policy.verify_platform_with_adhoc(
                root.path(),
                "run",
                VerifyMode::AtRest,
                &CancellationToken::new(),
                "macos",
                enabled,
            )
        };
        assert!(check(false).is_err());
        check(true).unwrap();
        let mut bytes = std::fs::read(root.path().join("run")).unwrap();
        // Alter a byte in the signed Mach-O header without destroying its format.
        bytes[24] ^= 1;
        std::fs::write(root.path().join("run"), bytes).unwrap();
        assert!(check(true).is_err());
    }
    #[test]
    fn scan_recheck_install_modes_and_failed_install_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        let package = root.join("probe");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("domain-os.yml"), "name: probe\nversion: '1.0.0'\nroute_namespace: /api/v1/probe\nevent_module: probe\ndata_dir: ~/.agent24/os/probe/\nimpl_kind: out_of_process_provider\nspawn:\n  command: bin/run\n").unwrap();
        crate::test_support::seal_exit_stub(&package, "bin/run").unwrap();
        let recorder = Arc::new(Recorder {
            calls: Mutex::new(Vec::new()),
            reject: AtomicBool::new(false),
        });
        let policy = Policy {
            verifier: recorder.clone(),
            trust_file: None,
        };
        let cancel = CancellationToken::new();
        let scan = crate::discovery::scan_with_policy(&root, &cancel, &policy);
        assert_eq!(scan.found.len(), 1, "{:?}", scan.refused);
        crate::discovery::recheck_with_policy(&package, &scan.found[0].digest, &cancel, &policy)
            .unwrap();
        let installed = temp.path().join("installed");
        crate::install::install_with_policy(&package, &installed, &policy).unwrap();
        assert_eq!(*recorder.calls.lock().unwrap(), [false, false, true]);
        recorder.reject.store(true, Ordering::SeqCst);
        let refused = temp.path().join("refused");
        let error = crate::install::install_with_policy(&package, &refused, &policy).unwrap_err();
        assert!(error.to_string().contains("刚公证"));
        assert!(
            !refused.exists(),
            "failed install must clean staging and created root"
        );
        // Release scripts retain hash/native-only verification even when signature verification fails.
        crate::package_integrity::verify(&package).unwrap();
    }
    #[test]
    fn cancellation_during_team_read_beats_signature_failure() {
        struct Cancel(CancellationToken);
        impl SignatureVerifier for Cancel {
            fn team_id(&self, _: &Path) -> Result<String, InstallError> {
                self.0.cancel();
                Err(InstallError::SignatureMismatch)
            }
            fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
                panic!("cancelled")
            }
        }
        let cancel = CancellationToken::new();
        let policy = Policy {
            verifier: Arc::new(Cancel(cancel.clone())),
            trust_file: None,
        };
        assert!(matches!(
            policy.verify(Path::new("/"), "run", VerifyMode::AtRest, &cancel),
            Err(IntegrityError::Cancelled)
        ));
    }
    #[test]
    fn apple_platform_binary_is_not_implicitly_trusted() {
        let policy = Policy {
            verifier: Arc::new(Codesign),
            trust_file: None,
        };
        assert!(
            policy
                .verify(
                    Path::new("/usr/bin"),
                    "true",
                    VerifyMode::AtRest,
                    &CancellationToken::new()
                )
                .is_err()
        );
    }
}
