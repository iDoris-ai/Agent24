#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

fn apple_peer() -> (
    tempfile::TempDir,
    std::process::Child,
    agent24_os_proc::AuditToken,
    std::os::unix::net::UnixStream,
) {
    peer_running("/usr/bin/nc", &["-U"])
}

fn peer_running(
    program: impl AsRef<std::path::Path>,
    args_before_socket: &[&str],
) -> (
    tempfile::TempDir,
    std::process::Child,
    agent24_os_proc::AuditToken,
    std::os::unix::net::UnixStream,
) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("peer.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let child = std::process::Command::new(program.as_ref())
        .args(args_before_socket)
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // Never block forever if the peer fails to start (e.g. is killed at exec).
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(std::time::Instant::now() < deadline, "peer never connected");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("accept: {e}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
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

/// "Apple" is not a ten-character Team ID: the shared rule rejects it up front.
#[test]
fn malformed_team_parameter_is_rejected_before_any_signature_work() {
    let (_dir, mut child, token, _stream) = apple_peer();
    let result = agent24_os_proc::verify_code_identity(token, "Apple", &["agent24"]);
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

/// Full name of a codesigning identity in the login keychain whose name starts
/// with `prefix`, or None (the test then skips, e.g. on CI without the cert).
fn keychain_identity(prefix: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let name = line.split('"').nth(1)?;
            name.starts_with(prefix).then(|| name.to_owned())
        })
}

const HELPER_C: &str = r#"
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <string.h>
int main(int c, char **v) {
  int s = socket(AF_UNIX, SOCK_STREAM, 0);
  struct sockaddr_un a;
  memset(&a, 0, sizeof a);
  a.sun_family = AF_UNIX;
  strncpy(a.sun_path, v[1], sizeof a.sun_path - 1);
  if (connect(s, (void *)&a, sizeof a)) return 1;
  sleep(60);
  return 0;
}
"#;

/// A tiny helper that connects to the socket, compiled and signed with
/// `identity` (hardened runtime), running like a real enrol peer. Returns the
/// binary dir too: it must outlive the process. None = cannot build/sign here.
fn signed_peer(
    identity: &str,
    ident: &str,
) -> Option<(
    tempfile::TempDir,
    tempfile::TempDir,
    std::process::Child,
    agent24_os_proc::AuditToken,
    std::os::unix::net::UnixStream,
)> {
    let bin_dir = tempfile::tempdir_in("/tmp").unwrap();
    let src = bin_dir.path().join("peer.c");
    let exe = bin_dir.path().join("peer");
    std::fs::write(&src, HELPER_C).unwrap();
    let built = std::process::Command::new("/usr/bin/cc")
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .output();
    if !matches!(&built, Ok(o) if o.status.success()) {
        eprintln!("SKIP: no C compiler for the peer helper: {built:?}");
        return None;
    }
    let signed = std::process::Command::new("/usr/bin/codesign")
        .args([
            "--force",
            "-s",
            identity,
            "-i",
            ident,
            "--options",
            "runtime",
        ])
        .arg(&exe)
        .output()
        .unwrap();
    if !signed.status.success() {
        eprintln!("SKIP: cannot sign with {identity}: {signed:?}");
        return None;
    }
    let (dir, child, token, stream) = peer_running(&exe, &[]);
    Some((bin_dir, dir, child, token, stream))
}

/// S1-2 positive control: a real Developer ID signed process passes the
/// enrol-time check, a locally signed (non Developer ID) one does not.
#[test]
fn developer_id_signed_peer_passes_and_local_cert_peer_is_rejected() {
    let team = agent24_os_proc::DEVELOPER_ID_TEAM_ID;
    let Some(dev_id) = keychain_identity("Developer ID Application:").filter(|n| n.contains(team))
    else {
        eprintln!("SKIP: no Developer ID Application identity for {team} in the keychain");
        return;
    };
    if let Some((_b, _d, mut child, token, _s)) = signed_peer(&dev_id, "agent24") {
        let result = agent24_os_proc::verify_code_identity(token, team, &["agent24"]);
        let _ = child.kill();
        child.wait().unwrap();
        let identity = result.expect("Developer ID signed peer must pass");
        assert_eq!(identity.team_id, team);
    }
    let Some(local) = keychain_identity("AgentEar Local Signing") else {
        eprintln!("SKIP: no non-Developer-ID identity in the keychain");
        return;
    };
    if let Some((_b, _d, mut child, token, _s)) = signed_peer(&local, "agent24") {
        let result = agent24_os_proc::verify_code_identity(token, team, &["agent24"]);
        let _ = child.kill();
        child.wait().unwrap();
        assert!(
            result.is_err(),
            "non Developer ID peer must be rejected: {result:?}"
        );
    }
}
