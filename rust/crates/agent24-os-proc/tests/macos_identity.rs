#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used)]

#[test]
fn apple_signed_process_fails_team_requirement_before_identifier() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("5")
        .spawn()
        .unwrap();
    let result =
        agent24_os_proc::verify_code_identity(child.id() as i32, "HYAP444FN9", &["agent24"]);
    let _ = child.kill();
    child.wait().unwrap();
    assert!(
        matches!(
            result,
            Err(agent24_os_proc::IdentityError::RequirementFailed)
        ),
        "{result:?}"
    );
}

#[test]
fn unsigned_test_process_fails_before_identifier() {
    let result = agent24_os_proc::verify_code_identity(
        std::process::id() as i32,
        "HYAP444FN9",
        &["agent24"],
    );
    assert!(
        matches!(
            result,
            Err(agent24_os_proc::IdentityError::SignatureInvalid
                | agent24_os_proc::IdentityError::RequirementFailed)
        ),
        "{result:?}"
    );
}

#[test]
fn apple_team_parameter_reaches_identifier_when_requirement_supported() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("5")
        .spawn()
        .unwrap();
    let result = agent24_os_proc::verify_code_identity(child.id() as i32, "Apple", &["agent24"]);
    let _ = child.kill();
    child.wait().unwrap();
    match result {
        Err(agent24_os_proc::IdentityError::IdentifierNotAllowed) => {}
        Err(agent24_os_proc::IdentityError::RequirementFailed) => {
            eprintln!(
                "SKIP Apple team variant: /bin/sleep does not satisfy the Apple generic anchor + leaf OU requirement"
            );
        }
        other => panic!("unexpected Apple team result: {other:?}"),
    }
}
