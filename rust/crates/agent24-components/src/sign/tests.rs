#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use std::{cell::RefCell, path::PathBuf};

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
const REQ: SigningRequirement = SigningRequirement {
    team_id: "HYAP444FN9",
    notarized: true,
};

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

#[test]
fn requirement_has_the_mandatory_leading_equals() {
    // Without the `=`, codesign reads the value as a file path (#1533).
    assert_eq!(
        team_requirement("HYAP444FN9"),
        "=anchor apple generic and certificate leaf[subject.OU] = \"HYAP444FN9\""
    );
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

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
        assert_eq!(args[at + 1], team_requirement("HYAP444FN9"));
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

    /// Real spctl on a notarized CLI; skipped where the release artifact is absent.
    #[test]
    fn real_spctl_accepts_notarized_cli_and_rejects_adhoc_copy() {
        let cli = Path::new("/Users/jason/Dev/auraai/Agent24/.wt/rel/dist/mac-cli/agent24");
        if !cli.exists() {
            eprintln!("skipped: {} not present", cli.display());
            return;
        }
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
            verifier.check_requirement(&exe, &team_requirement("HYAP444FN9"), false),
            Err(InstallError::SignatureMismatch)
        ));
        // Without the leading `=` codesign would treat the text as a file path.
        let no_equals = team_requirement("HYAP444FN9").replacen('=', "", 1);
        assert!(!matches!(
            verifier.check_requirement(&exe, &no_equals, false),
            Err(InstallError::SignatureMismatch)
        ));
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
