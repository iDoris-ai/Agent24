#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use std::path::PathBuf;

const REQ: SigningRequirement = SigningRequirement {
    team_id: "HYAP444FN9",
    notarized: true,
};

/// S1-2 / E1: install-time and enrol-time use the very same requirement text.
#[test]
fn install_requirement_is_the_enrol_requirement() {
    assert_eq!(
        team_requirement(agent24_os_proc::DEVELOPER_ID_TEAM_ID).unwrap(),
        format!(
            "={}",
            agent24_os_proc::developer_id_requirement("HYAP444FN9").unwrap()
        )
    );
}

#[test]
fn requirement_has_the_mandatory_leading_equals() {
    // Without the `=`, codesign reads the value as a file path (#1533).
    let req = team_requirement("HYAP444FN9").unwrap();
    assert!(req.starts_with("=anchor apple generic and "));
    // Developer ID only: both chain markers, then the team.
    assert!(req.contains("certificate 1[field.1.2.840.113635.100.6.2.6] exists"));
    assert!(req.contains("certificate leaf[field.1.2.840.113635.100.6.1.13] exists"));
    assert!(req.ends_with("certificate leaf[subject.OU] = \"HYAP444FN9\""));
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::cell::RefCell;

    type Script = Box<dyn Fn(&str, &[&str]) -> io::Result<CommandOutput>>;

    /// Records every invocation and answers from a script.
    struct Fake {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        script: Script,
    }
    impl Fake {
        fn new(script: impl Fn(&str, &[&str]) -> io::Result<CommandOutput> + 'static) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                script: Box::new(script),
            }
        }
    }
    impl CommandRunner for &Fake {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            self.calls.borrow_mut().push((
                program.into(),
                args.iter().map(|s| (*s).to_owned()).collect(),
            ));
            (self.script)(program, args)
        }
    }

    fn ok(stderr: &str) -> io::Result<CommandOutput> {
        Ok(CommandOutput {
            success: true,
            stderr: stderr.into(),
        })
    }
    fn fail(stderr: &str) -> io::Result<CommandOutput> {
        Ok(CommandOutput {
            success: false,
            stderr: stderr.into(),
        })
    }

    const DETAILS_OK: &str = "Identifier=demo\nCodeDirectory v=20500 size=900 flags=0x10000(runtime) hashes=20+7 location=embedded\nTeamIdentifier=HYAP444FN9\n";

    const SPCTL_OK: &str = "/x/demo: accepted\nsource=Notarized Developer ID\n";

    fn healthy(program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        if program == CODESIGN && args.contains(&"-dv") {
            ok(DETAILS_OK)
        } else if program == SPCTL {
            ok(SPCTL_OK)
        } else {
            ok("")
        }
    }

    fn run(fake: &Fake, required: &SigningRequirement) -> Result<(), InstallError> {
        CodesignWith(fake).verify(&PathBuf::from("/x/demo"), required)
    }

    #[test]
    fn requirement_is_passed_to_codesign_verbatim_with_strict() {
        let fake = Fake::new(healthy);
        run(&fake, &REQ).unwrap();
        let calls = fake.calls.borrow();
        let args = &calls[0].1;
        let at = args.iter().position(|a| a == "-R").unwrap();
        assert_eq!(args[at + 1], team_requirement("HYAP444FN9").unwrap());
        assert!(args.contains(&"--strict".to_owned()));
        assert!(!args.contains(&"--deep".to_owned()));
    }

    #[test]
    fn all_checks_run_in_order_for_a_notarized_executable() {
        let fake = Fake::new(healthy);
        run(&fake, &REQ).unwrap();
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0, CODESIGN);
        assert_eq!(calls[1].1[0], "-dv");
        assert_eq!(calls[2].0, SPCTL);
        // Bare executables are assessed as `install`; `execute` rejects them.
        assert_eq!(calls[2].1[..3], ["--assess", "--type", "install"]);
    }

    #[test]
    fn spctl_is_skipped_unless_notarized() {
        let fake = Fake::new(healthy);
        run(
            &fake,
            &SigningRequirement {
                notarized: false,
                ..REQ
            },
        )
        .unwrap();
        assert!(fake.calls.borrow().iter().all(|(p, _)| p != SPCTL));
    }

    #[test]
    fn unmet_requirement_is_a_mismatch_not_a_tool_failure() {
        let fake =
            Fake::new(|_, _| fail("/x/demo: code failed to satisfy specified code requirement(s)"));
        assert!(matches!(
            run(&fake, &REQ),
            Err(InstallError::SignatureMismatch)
        ));
        let fake = Fake::new(|_, _| fail("/x/demo: No such file or directory"));
        assert!(matches!(
            run(&fake, &REQ),
            Err(InstallError::SignatureCheckFailed(m)) if m.contains("No such file")
        ));
    }

    #[test]
    fn wrong_team_identifier_is_rejected_even_if_the_requirement_passed() {
        let fake = Fake::new(|p, a| {
            if p == CODESIGN && a.contains(&"-dv") {
                ok(&DETAILS_OK.replace("HYAP444FN9", "OTHERTEAM1"))
            } else {
                healthy(p, a)
            }
        });
        assert!(matches!(
            run(&fake, &REQ),
            Err(InstallError::SignatureMismatch)
        ));
    }

    #[test]
    fn missing_hardened_runtime_is_rejected() {
        for flags in [
            "0x0(none)",
            "0x2(adhoc)",
            "0x10000",
            "0x12(adhoc,linker-signed)",
        ] {
            let details = DETAILS_OK.replace("0x10000(runtime)", flags);
            let fake = Fake::new(move |p, a| {
                if p == CODESIGN && a.contains(&"-dv") {
                    ok(&details)
                } else {
                    healthy(p, a)
                }
            });
            assert!(
                matches!(run(&fake, &REQ), Err(InstallError::HardenedRuntimeMissing)),
                "{flags} accepted"
            );
        }
        // The flag may sit among others.
        let details = DETAILS_OK.replace("0x10000(runtime)", "0x10002(adhoc,runtime)");
        let fake = Fake::new(move |p, a| {
            if p == CODESIGN && a.contains(&"-dv") {
                ok(&details)
            } else {
                healthy(p, a)
            }
        });
        run(&fake, &REQ).unwrap();
    }

    #[test]
    fn spctl_rejection_blocks_a_notarized_lock() {
        let fake = Fake::new(|p, a| {
            if p == SPCTL {
                fail("rejected")
            } else {
                healthy(p, a)
            }
        });
        assert!(matches!(run(&fake, &REQ), Err(InstallError::NotNotarized)));
    }

    #[test]
    fn a_tool_that_never_ran_says_which_tool() {
        // spctl that cannot finish: the Gatekeeper phase, not a codesign failure.
        let fake = Fake::new(|p, a| {
            if p == SPCTL {
                Err(io::Error::new(io::ErrorKind::TimedOut, "spctl deadline"))
            } else {
                healthy(p, a)
            }
        });
        assert!(matches!(
            run(&fake, &REQ),
            Err(InstallError::NotarizationCheckFailed(e)) if e.kind() == io::ErrorKind::TimedOut
        ));
        // codesign that cannot be spawned stays plain I/O, and Gatekeeper is never asked.
        let fake = Fake::new(|p, a| {
            if p == CODESIGN {
                Err(io::Error::new(io::ErrorKind::NotFound, "no codesign"))
            } else {
                healthy(p, a)
            }
        });
        assert!(matches!(run(&fake, &REQ), Err(InstallError::Io(_))));
        assert!(fake.calls.borrow().iter().all(|(p, _)| p != SPCTL));
    }

    #[test]
    fn notarization_needs_the_verdict_text_not_just_exit_zero() {
        for text in [
            "",
            "/x/demo: accepted\nsource=Developer ID\n",
            "/x/demo: accepted\n",
            "/x/demo: rejected\nsource=Notarized Developer ID\n",
        ] {
            let text = text.to_owned();
            let fake = Fake::new(
                move |p, a| {
                    if p == SPCTL { ok(&text) } else { healthy(p, a) }
                },
            );
            assert!(matches!(run(&fake, &REQ), Err(InstallError::NotNotarized)));
        }
    }

    #[test]
    fn bundles_are_assessed_as_execute() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new(healthy);
        CodesignWith(&fake).verify(dir.path(), &REQ).unwrap();
        let calls = fake.calls.borrow();
        let spctl = calls.iter().find(|(p, _)| p == SPCTL).unwrap();
        assert_eq!(spctl.1[..3], ["--assess", "--type", "execute"]);
    }

    /// Real spctl on a notarized CLI. Run with
    /// `A24_TEST_NOTARIZED_BIN=/path/to/notarized-cli cargo test -- --ignored`;
    /// ignored (not silently passing) when no such binary is supplied.
    #[test]
    #[ignore = "needs A24_TEST_NOTARIZED_BIN"]
    fn real_spctl_accepts_notarized_cli_and_rejects_adhoc_copy() {
        let cli = PathBuf::from(
            std::env::var_os("A24_TEST_NOTARIZED_BIN")
                .expect("set A24_TEST_NOTARIZED_BIN to a notarized Mach-O CLI"),
        );
        let cli = cli.as_path();
        let assess = |path: &Path| {
            SystemRunner
                .run(
                    SPCTL,
                    &[
                        "--assess",
                        "--type",
                        "install",
                        "-vv",
                        &path.to_string_lossy(),
                    ],
                )
                .unwrap()
        };
        let good = assess(cli);
        assert!(
            good.success && notarized_verdict(&good.stderr),
            "{}",
            good.stderr
        );
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("agent24");
        std::fs::copy(cli, &copy).unwrap();
        let signed = Command::new(CODESIGN)
            .args(["--force", "-s", "-"])
            .arg(&copy)
            .output()
            .unwrap();
        assert!(signed.status.success());
        let bad = assess(&copy);
        assert!(
            !(bad.success && notarized_verdict(&bad.stderr)),
            "{}",
            bad.stderr
        );
    }

    /// Developer-ID-signed but never notarized: Gatekeeper must not say Notarized.
    /// `A24_TEST_DEVID_IDENTITY` is a codesign identity (it may prompt for the keychain).
    #[test]
    #[ignore = "needs A24_TEST_DEVID_IDENTITY"]
    fn real_spctl_rejects_developer_id_signed_but_not_notarized() {
        let identity = std::env::var("A24_TEST_DEVID_IDENTITY")
            .expect("set A24_TEST_DEVID_IDENTITY to a Developer ID Application identity");
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("true");
        std::fs::copy("/usr/bin/true", &exe).unwrap();
        let signed = Command::new(CODESIGN)
            .args(["--force", "--options", "runtime", "-s", &identity])
            .arg(&exe)
            .output()
            .unwrap();
        assert!(signed.status.success(), "{signed:?}");
        let verdict = SystemRunner
            .run(
                SPCTL,
                &[
                    "--assess",
                    "--type",
                    "install",
                    "-vv",
                    &exe.to_string_lossy(),
                ],
            )
            .unwrap();
        assert!(!notarized_verdict(&verdict.stderr), "{}", verdict.stderr);
        assert!(!verdict.stderr.contains("source=Notarized Developer ID"));
        // The whole verifier agrees: signature fine, notarization refused.
        let team = identity
            .rsplit('(')
            .next()
            .unwrap()
            .trim_end_matches(')')
            .to_owned();
        let result = Codesign.verify(
            &exe,
            &SigningRequirement {
                team_id: &team,
                notarized: true,
            },
        );
        assert!(
            matches!(result, Err(InstallError::NotNotarized)),
            "{result:?}"
        );
    }

    #[test]
    fn junk_team_ids_never_reach_codesign() {
        for junk in ["", "short", "hyap444fn9", "HYAP444FN9\"x", "HYAP444FN99"] {
            let fake = Fake::new(healthy);
            let req = SigningRequirement {
                team_id: junk,
                notarized: false,
            };
            assert!(matches!(
                run(&fake, &req),
                Err(InstallError::SignatureMismatch)
            ));
            assert!(fake.calls.borrow().is_empty(), "codesign ran for {junk:?}");
        }
    }

    #[test]
    fn a_directory_is_verified_deep() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new(healthy);
        CodesignWith(&fake).verify(dir.path(), &REQ).unwrap();
        assert!(fake.calls.borrow()[0].1.contains(&"--deep".to_owned()));
    }

    /// Real codesign on an ad-hoc signed copy of a system binary.
    #[test]
    fn real_codesign_distinguishes_unmet_from_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("demo");
        std::fs::copy("/usr/bin/true", &exe).unwrap();
        let signed = Command::new(CODESIGN)
            .args([
                "--force",
                "-s",
                "-",
                "-i",
                "ai.test.c1",
                "--options",
                "runtime",
            ])
            .arg(&exe)
            .output()
            .unwrap();
        assert!(signed.status.success(), "{signed:?}");
        let verifier = CodesignWith(SystemRunner);
        // A requirement the ad-hoc signature does satisfy.
        verifier
            .check_requirement(&exe, "=identifier \"ai.test.c1\"", false)
            .unwrap();
        // An unmet Team ID requirement: "unmet", never a file error.
        assert!(matches!(
            verifier.check_requirement(&exe, &team_requirement("HYAP444FN9").unwrap(), false),
            Err(InstallError::SignatureMismatch)
        ));
        // Without the leading `=` codesign would treat the text as a file path.
        let no_equals = team_requirement("HYAP444FN9").unwrap().replacen('=', "", 1);
        assert!(
            matches!(
                verifier.check_requirement(&exe, &no_equals, false),
                Err(InstallError::SignatureCheckFailed(_))
            ),
            "a requirement without the leading `=` must fail as a tool error"
        );
        // A missing file is a tool failure that says so.
        assert!(matches!(
            verifier.check_requirement(&dir.path().join("absent"), "=identifier \"x\"", false),
            Err(InstallError::SignatureCheckFailed(m)) if m.contains("No such file")
        ));
        // The full check rejects the ad-hoc binary (no Team ID).
        assert!(matches!(
            Codesign.verify(&exe, &REQ),
            Err(InstallError::SignatureMismatch)
        ));
    }
}

#[test]
fn unsigned_verifier_accepts_everything_for_linux_assets() {
    Unsigned.verify(&PathBuf::from("/x/demo"), &REQ).unwrap();
}

#[test]
fn runtime_flag_parser_ignores_other_lines() {
    assert!(has_runtime_flag(
        "CodeDirectory v=20500 flags=0x10000(runtime) hashes=1"
    ));
    assert!(!has_runtime_flag(
        "Identifier=runtime flags=0x10000(runtime)"
    ));
    assert!(!has_runtime_flag("CodeDirectory v=20500 flags=0x0(none)"));
}

#[test]
fn a_hung_tool_is_killed_at_the_deadline_and_fails_closed() {
    use std::time::{Duration, Instant};
    let started = Instant::now();
    let err = run_with_deadline("/bin/sleep", &["30"], Duration::from_millis(200)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(err.to_string().contains("did not finish"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
    // A tool that finishes in time is unaffected and its stderr is captured.
    let out = run_with_deadline(
        "/bin/sh",
        &["-c", "echo hi >&2; exit 3"],
        Duration::from_secs(10),
    )
    .unwrap();
    assert!(!out.success && out.stderr.contains("hi"));
}

#[test]
fn signing_requirement_is_decided_by_platform_and_mode() {
    let mut lock = crate::lock::test_lock();
    lock.signing.as_mut().unwrap().notarized = true;
    let install = signing_requirement(&lock, "darwin-arm64", VerifyMode::Install)
        .unwrap()
        .unwrap();
    assert_eq!(install.team_id, "HYAP444FN9");
    assert!(install.notarized, "install asks Gatekeeper");
    let at_rest = signing_requirement(&lock, "darwin-arm64", VerifyMode::AtRest)
        .unwrap()
        .unwrap();
    assert!(!at_rest.notarized, "at rest stays offline");
    // Linux has no system signature: an explicit `None`, for both modes.
    for mode in [VerifyMode::Install, VerifyMode::AtRest] {
        assert!(
            signing_requirement(&lock, "linux-x64", mode)
                .unwrap()
                .is_none()
        );
    }
    // The lock cannot waive notarization.
    lock.signing.as_mut().unwrap().notarized = false;
    for mode in [VerifyMode::Install, VerifyMode::AtRest] {
        assert!(matches!(
            signing_requirement(&lock, "darwin-arm64", mode),
            Err(InstallError::ManifestViolation(_))
        ));
    }
    assert!(
        signing_requirement(&lock, "linux-x64", VerifyMode::Install)
            .unwrap()
            .is_none()
    );
    lock.signing.as_mut().unwrap().notarized = true;
    lock.signing.as_mut().unwrap().macos_team_id = "bad\"team".into();
    assert!(matches!(
        signing_requirement(&lock, "darwin-arm64", VerifyMode::AtRest),
        Err(InstallError::ManifestViolation(_))
    ));
    lock.signing = None;
    assert!(matches!(
        signing_requirement(&lock, "darwin-arm64", VerifyMode::AtRest),
        Err(InstallError::ManifestViolation(_))
    ));
}

#[test]
fn team_reader_rejects_unsigned_adhoc_duplicate_and_malformed_claims() {
    struct Details(bool, &'static str);
    impl CommandRunner for Details {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            assert_eq!(program, CODESIGN);
            assert_eq!(&args[..2], &["-dv", "--verbose=4"]);
            Ok(CommandOutput {
                success: self.0,
                stderr: self.1.into(),
            })
        }
    }
    for (success, text) in [
        (false, "TeamIdentifier=HYAP444FN9"),
        (true, "Signature=adhoc\nTeamIdentifier=not set"),
        (true, ""),
        (true, "TeamIdentifier=HYAP444FN9\nTeamIdentifier=ABCDE12345"),
        (true, "TeamIdentifier=../../oops"),
    ] {
        assert!(
            CodesignWith(Details(success, text))
                .team_id(Path::new("binary"))
                .is_err()
        );
    }
    assert_eq!(
        CodesignWith(Details(true, "TeamIdentifier=HYAP444FN9\n"))
            .team_id(Path::new("binary"))
            .unwrap(),
        "HYAP444FN9"
    );
}

#[cfg(debug_assertions)]
#[test]
fn adhoc_requires_explicit_metadata_and_valid_seal() {
    struct Runner {
        details: &'static str,
        display_ok: bool,
        seal_ok: bool,
    }
    impl CommandRunner for Runner {
        fn run(&self, _: &str, args: &[&str]) -> io::Result<CommandOutput> {
            if args[0] == "-dv" {
                Ok(CommandOutput {
                    success: self.display_ok,
                    stderr: self.details.into(),
                })
            } else {
                assert_eq!(args, ["--verify", "--strict", "/stub"]);
                Ok(CommandOutput {
                    success: self.seal_ok,
                    stderr: String::new(),
                })
            }
        }
    }
    for (details, display_ok, seal_ok, expected) in [
        ("Signature=adhoc\nTeamIdentifier=not set", true, true, true),
        (
            "Signature=adhoc\nTeamIdentifier=not set",
            true,
            false,
            false,
        ),
        (
            "Signature=adhoc\nTeamIdentifier=not set",
            false,
            true,
            false,
        ),
        ("TeamIdentifier=not set", true, true, false),
        (
            "Signature=adhoc\nTeamIdentifier=ABCDE12345",
            true,
            true,
            false,
        ),
        (
            "Signature=adhoc\nTeamIdentifier=not set\nTeamIdentifier=ABCDE12345",
            true,
            true,
            false,
        ),
        ("", false, false, false),
    ] {
        assert_eq!(
            CodesignWith(Runner {
                details,
                display_ok,
                seal_ok
            })
            .verify_adhoc(Path::new("/stub"))
            .is_ok(),
            expected
        );
    }
}
