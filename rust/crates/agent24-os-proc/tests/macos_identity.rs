#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used)]

fn apple_peer() -> (
    tempfile::TempDir,
    std::process::Child,
    agent24_os_proc::AuditToken,
    std::os::unix::net::UnixStream,
) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("peer.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let child = std::process::Command::new("/usr/bin/nc")
        .arg("-U")
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (stream, _) = listener.accept().unwrap();
    let token = agent24_os_proc::peer_audit_token(&stream).unwrap();
    assert_eq!(token.pid(), child.id() as i32);
    (dir, child, token, stream)
}

#[test]
fn apple_signed_process_fails_team_requirement_before_identifier() {
    let (_dir, mut child, token, _stream) = apple_peer();
    let result = agent24_os_proc::verify_code_identity(token, "HYAP444FN9", &["agent24"]);
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
    let (stream, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let token = agent24_os_proc::peer_audit_token(&stream).unwrap();
    let result = agent24_os_proc::verify_code_identity(token, "HYAP444FN9", &["agent24"]);
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
    let (_dir, mut child, token, _stream) = apple_peer();
    let result = agent24_os_proc::verify_code_identity(token, "Apple", &["agent24"]);
    let _ = child.kill();
    child.wait().unwrap();
    match result {
        Err(agent24_os_proc::IdentityError::IdentifierNotAllowed) => {}
        Err(agent24_os_proc::IdentityError::RequirementFailed) => {
            eprintln!(
                "SKIP Apple team variant: /usr/bin/nc does not satisfy the Apple generic anchor + leaf OU requirement"
            );
        }
        other => panic!("unexpected Apple team result: {other:?}"),
    }
}
