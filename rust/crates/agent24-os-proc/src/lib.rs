//! Kernel process identity primitives for CORE-2 D5.
#![deny(unsafe_code)]

use std::{io, path::PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcStart {
    pub sec: u64,
    pub usec: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeIdentity {
    pub identifier: String,
    pub team_id: String,
    pub executable: PathBuf,
}

#[derive(Debug)]
pub enum IdentityError {
    Unsupported,
    SignatureInvalid,
    RequirementFailed,
    IdentifierNotAllowed,
    PathMismatch,
    Io(io::Error),
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub fn is_alive(pid: i32) -> bool {
    rustix::process::Pid::from_raw(pid)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

#[cfg(target_os = "macos")]
pub fn is_alive(pid: i32) -> bool {
    process_info(pid).is_ok_and(|info| info.pbi_status != libc::SZOMB)
}

#[cfg(target_os = "linux")]
pub fn is_alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .and_then(|(_, tail)| tail.split_whitespace().next())
            .is_some_and(|state| !matches!(state, "Z" | "X"))
    })
}

/// Kernel-supplied connection identity, including the exec-sensitive pidversion.
/// There is deliberately no constructor from a PID or client-supplied bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuditToken([u32; 8]);

impl AuditToken {
    pub fn pid(self) -> i32 {
        self.0[5] as i32
    }
    pub fn euid(self) -> u32 {
        self.0[1]
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn peer_audit_token(socket: &impl std::os::fd::AsRawFd) -> io::Result<AuditToken> {
    let mut token = AuditToken([0; 8]);
    let mut size = std::mem::size_of_val(&token.0) as libc::socklen_t;
    // SAFETY: the borrowed socket remains open; token and size are writable
    // buffers with exactly the size supplied to getsockopt.
    let result = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            token.0.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if size as usize != std::mem::size_of_val(&token.0) || token.pid() <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid peer audit token",
        ));
    }
    Ok(token)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn peer_audit_token(_socket: &impl std::os::fd::AsRawFd) -> io::Result<AuditToken> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer audit token unavailable",
    ))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn audit_status(token: AuditToken) -> io::Result<u32> {
    unsafe extern "C" {
        fn csops_audittoken(
            pid: libc::pid_t,
            ops: libc::c_uint,
            address: *mut libc::c_void,
            size: libc::size_t,
            token: *const u32,
        ) -> libc::c_int;
    }
    let mut flags = 0u32;
    // SAFETY: CS_OPS_STATUS (0) writes one u32; the audit token points to
    // eight initialized native u32s. The kernel rejects a stale pidversion.
    let result = unsafe {
        csops_audittoken(
            token.pid(),
            0,
            (&mut flags as *mut u32).cast(),
            std::mem::size_of_val(&flags),
            token.0.as_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags)
}

#[cfg(target_os = "macos")]
pub fn audit_token_is_live(token: AuditToken) -> bool {
    audit_status(token).is_ok() && is_alive(token.pid())
}

#[cfg(not(target_os = "macos"))]
pub fn audit_token_is_live(_token: AuditToken) -> bool {
    false
}

#[cfg(not(unix))]
pub fn is_alive(_pid: i32) -> bool {
    false
}

#[cfg(target_os = "linux")]
pub fn executable_path(pid: i32) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
}

#[cfg(target_os = "linux")]
pub fn start_time(pid: i32) -> io::Result<ProcStart> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // comm may contain whitespace and ')'; fields after its final ')' start at 3.
    let ticks = stat
        .rsplit_once(')')
        .and_then(|(_, tail)| tail.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process stat"))?;
    Ok(ProcStart {
        sec: ticks,
        usec: 0,
    })
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn executable_path(pid: i32) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: buffer is writable for exactly the size supplied to libproc.
    let count = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if count <= 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(
        buffer
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(count as usize),
    );
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
}

#[cfg(target_os = "macos")]
pub fn start_time(pid: i32) -> io::Result<ProcStart> {
    let info = process_info(pid)?;
    Ok(ProcStart {
        sec: info.pbi_start_tvsec,
        usec: info.pbi_start_tvusec,
    })
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn process_info(pid: i32) -> io::Result<libc::proc_bsdinfo> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // SAFETY: info points to a correctly sized output structure; it is read only
    // after libproc reports that the entire structure was initialized.
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if count != size as i32 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the complete output was initialized above.
    let info = unsafe { info.assume_init() };
    Ok(info)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn executable_path(_pid: i32) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process identity unavailable",
    ))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn start_time(_pid: i32) -> io::Result<ProcStart> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process identity unavailable",
    ))
}

/// The one Apple Developer ID Team ID that signs every first-party Agent24
/// binary. Lock files, release scripts and package metadata repeat this value;
/// `tests/team_id_consistency.rs` fails if any of them drifts from it.
pub const DEVELOPER_ID_TEAM_ID: &str = "HYAP444FN9";

/// The code requirement for "signed by this Developer ID team", shared by the
/// install-time check (`agent24-components`, which prefixes the `=` that
/// `codesign -R` needs) and the enrol-time check below, so the two cannot drift.
///
/// It demands the Developer ID chain markers (intermediate
/// 1.2.840.113635.100.6.2.6 and the Developer ID Application leaf
/// 1.2.840.113635.100.6.1.13), so a Mac App Store or Apple Development
/// certificate of the same team does not pass.
pub fn developer_id_requirement(team_id: &str) -> Result<String, InvalidTeamId> {
    if !valid_team_id(team_id) {
        // The id is interpolated into a code requirement: never pass junk through.
        return Err(InvalidTeamId);
    }
    Ok(format!(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team_id}\""
    ))
}

/// Not a ten-character `[A-Z0-9]` Apple Team ID. Returned instead of panicking
/// because the value comes from lock files and configuration, and a security
/// check must fail closed (reject) rather than abort the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTeamId;

/// Apple Team IDs are ten upper-case letters or digits. The one rule for both
/// install-time and enrol-time checks.
pub fn valid_team_id(team_id: &str) -> bool {
    team_id.len() == 10
        && team_id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

#[cfg(not(target_os = "macos"))]
pub fn verify_code_identity(
    _token: AuditToken,
    _team_id: &str,
    _allowed_identifiers: &[&str],
) -> Result<CodeIdentity, IdentityError> {
    Err(IdentityError::Unsupported)
}

#[cfg(target_os = "macos")]
pub fn verify_code_identity(
    token: AuditToken,
    team_id: &str,
    allowed_identifiers: &[&str],
) -> Result<CodeIdentity, IdentityError> {
    use security_framework::os::macos::code_signing::{
        Flags, GuestAttributes, SecCode, SecRequirement,
    };
    let mut attributes = GuestAttributes::new();
    use core_foundation::{base::TCFType, data::CFData};
    let bytes: Vec<u8> = token.0.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let data = CFData::from_buffer(&bytes);
    attributes.set_audit_token(data.as_concrete_TypeRef());
    let code = SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE)
        .map_err(|_| IdentityError::SignatureInvalid)?;
    let requirement: SecRequirement = developer_id_requirement(team_id)
        .map_err(|_| IdentityError::RequirementFailed)?
        .parse()
        .map_err(|_| IdentityError::RequirementFailed)?;
    // SecCode validates both the signature and the requirement before we read
    // identifier/path metadata. A failed certificate trust requirement does
    // not mean that the executable is unsigned or its signature is corrupt.
    code.check_validity(Flags::NONE, &requirement)
        .map_err(|error| match error.code() {
            -67050 | -2147409622 => IdentityError::RequirementFailed, // errSecCSReqFailed / CSSMERR_TP_NOT_TRUSTED
            _ => IdentityError::SignatureInvalid,
        })?;
    let (identifier, signed_team, signed_path) = signing_information(&code)?;
    // CS_RUNTIME is part of the running code's signature flags.
    require_hardened_runtime(audit_status(token).map_err(IdentityError::Io)?)?;
    let executable = executable_path(token.pid())
        .and_then(|p| p.canonicalize())
        .map_err(IdentityError::Io)?;
    let identity = check_identity_fields(
        identifier,
        signed_team,
        signed_path,
        team_id,
        allowed_identifiers,
        executable,
    )?;
    if !audit_token_is_live(token) {
        return Err(IdentityError::SignatureInvalid);
    }
    Ok(identity)
}

#[cfg(any(target_os = "macos", test))]
fn require_hardened_runtime(flags: u32) -> Result<(), IdentityError> {
    const CS_RUNTIME: u32 = 0x10000;
    if flags & CS_RUNTIME == 0 {
        return Err(IdentityError::RequirementFailed);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn check_identity_fields(
    identifier: String,
    signed_team: String,
    signed_path: PathBuf,
    expected_team: &str,
    allowed: &[&str],
    executable: PathBuf,
) -> Result<CodeIdentity, IdentityError> {
    if signed_team != expected_team {
        return Err(IdentityError::RequirementFailed);
    }
    if !allowed.contains(&identifier.as_str()) {
        return Err(IdentityError::IdentifierNotAllowed);
    }
    if signed_path.canonicalize().map_err(IdentityError::Io)? != executable {
        return Err(IdentityError::PathMismatch);
    }
    Ok(CodeIdentity {
        identifier,
        team_id: signed_team,
        executable,
    })
}

/// Security.framework's safe wrapper does not yet expose signing information.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn signing_information(
    code: &security_framework::os::macos::code_signing::SecCode,
) -> Result<(String, String, PathBuf), IdentityError> {
    use core_foundation::{
        base::{CFType, TCFType},
        dictionary::{CFDictionary, CFDictionaryRef},
        string::CFString,
        url::CFURL,
    };
    unsafe extern "C" {
        static kSecCodeInfoIdentifier: core_foundation::string::CFStringRef;
        static kSecCodeInfoTeamIdentifier: core_foundation::string::CFStringRef;
        static kSecCodeInfoMainExecutable: core_foundation::string::CFStringRef;
        fn SecCodeCopySigningInformation(
            code: core_foundation::base::CFTypeRef,
            flags: u32,
            info: *mut CFDictionaryRef,
        ) -> i32;
    }
    let mut raw = std::ptr::null();
    // SAFETY: code is a live SecCode; raw is a writable out pointer. On success
    // the Copy rule transfers one retained dictionary, released by CFDictionary.
    if unsafe { SecCodeCopySigningInformation(code.as_CFTypeRef(), 1 << 1, &mut raw) } != 0
        || raw.is_null()
    {
        return Err(IdentityError::SignatureInvalid);
    }
    // SAFETY: successful Copy returned a retained dictionary of CF values.
    let info: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_create_rule(raw) };
    // SAFETY: these are immortal CFString constants exported by Security.framework.
    let (identifier_key, team_key, executable_key) = unsafe {
        (
            CFString::wrap_under_get_rule(kSecCodeInfoIdentifier),
            CFString::wrap_under_get_rule(kSecCodeInfoTeamIdentifier),
            CFString::wrap_under_get_rule(kSecCodeInfoMainExecutable),
        )
    };
    let string = |key: CFString| {
        info.find(key)
            .and_then(|v| v.downcast::<CFString>())
            .map(|s| s.to_string())
            .ok_or(IdentityError::SignatureInvalid)
    };
    let identifier = string(identifier_key)?;
    let team = string(team_key)?;
    let executable = info
        .find(executable_key)
        .and_then(|v| v.downcast::<CFURL>())
        .and_then(|url| url.to_path())
        .ok_or(IdentityError::SignatureInvalid)?;
    Ok((identifier, team, executable))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn current_process_has_stable_kernel_identity() {
        let pid = std::process::id() as i32;
        assert_eq!(start_time(pid).unwrap(), start_time(pid).unwrap());
        assert_eq!(
            executable_path(pid).unwrap().canonicalize().unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
    }
    #[cfg(unix)]
    #[test]
    fn zombie_process_is_not_alive() {
        use std::io::Read;
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut Vec::new())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while is_alive(child.id() as i32) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let alive = is_alive(child.id() as i32);
        child.wait().unwrap();
        assert!(!alive, "an unreaped zombie must not keep credentials alive");
    }
    #[test]
    fn hardened_runtime_flag_is_required() {
        for flags in [0, 1, 0x2000, 0xffff] {
            assert!(matches!(
                require_hardened_runtime(flags),
                Err(IdentityError::RequirementFailed)
            ));
        }
        assert!(require_hardened_runtime(0x10000).is_ok());
        assert!(require_hardened_runtime(0x10001).is_ok());
    }
    #[test]
    fn signed_identifier_team_and_canonical_path_must_all_match() {
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        for (identifier, team, path) in [
            ("other", "TEAM", executable.clone()),
            ("agent24.extra", "TEAM", executable.clone()),
            ("agent24", "WRONG", executable.clone()),
            ("agent24", "TEAM", PathBuf::from("/bin/sh")),
        ] {
            assert!(
                check_identity_fields(
                    identifier.into(),
                    team.into(),
                    path,
                    "TEAM",
                    &["agent24"],
                    executable.clone()
                )
                .is_err()
            );
        }
        assert!(
            check_identity_fields(
                "agent24".into(),
                "TEAM".into(),
                executable.clone(),
                "TEAM",
                &["agent24"],
                executable
            )
            .is_ok()
        );
    }
    #[test]
    fn developer_id_requirement_demands_chain_markers_and_team() {
        let req = developer_id_requirement(DEVELOPER_ID_TEAM_ID).unwrap();
        assert!(req.starts_with("anchor apple generic and "));
        assert!(req.contains("certificate 1[field.1.2.840.113635.100.6.2.6] exists"));
        assert!(req.contains("certificate leaf[field.1.2.840.113635.100.6.1.13] exists"));
        assert!(req.ends_with("certificate leaf[subject.OU] = \"HYAP444FN9\""));
    }

    #[test]
    fn developer_id_requirement_rejects_malformed_team_ids() {
        for junk in [
            "",
            "short",
            "hyap444fn9",
            "HYAP444FN99",
            "HYAP444FN\"",
            "HYAP444FN9\" or true",
            "Apple",
        ] {
            assert_eq!(
                developer_id_requirement(junk),
                Err(InvalidTeamId),
                "{junk:?}"
            );
        }
    }

    /// S1-2 review F1: the enrol path must build its requirement from
    /// `developer_id_requirement` and no other literal, or the install and
    /// enrol checks can drift apart again.
    #[test]
    fn enrol_check_uses_the_shared_requirement_and_no_other_literal() {
        let full = include_str!("lib.rs");
        let src = &full[..full.find("#[cfg(test)]\nmod tests").unwrap()];
        let needle = ["anchor", " apple"].concat();
        assert_eq!(
            src.matches(needle.as_str()).count(),
            1,
            "exactly one literal"
        );
        let at = src.find(needle.as_str()).unwrap();
        let start = src.find("pub fn developer_id_requirement").unwrap();
        let end = start + src[start..].find("\n}\n").unwrap();
        assert!(
            (start..end).contains(&at),
            "literal lives in developer_id_requirement"
        );
        let v = src.find("pub fn verify_code_identity(").unwrap();
        let macos = v + src[v + 1..].find("pub fn verify_code_identity(").unwrap() + 1;
        let body = &src[macos..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(body.contains("developer_id_requirement(team_id)"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn developer_id_requirement_parses_as_a_sec_requirement() {
        use security_framework::os::macos::code_signing::SecRequirement;
        assert!(
            developer_id_requirement(DEVELOPER_ID_TEAM_ID)
                .unwrap()
                .parse::<SecRequirement>()
                .is_ok()
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn code_identity_is_unsupported() {
        assert!(matches!(
            verify_code_identity(AuditToken([0; 8]), "HYAP444FN9", &["agent24"]),
            Err(IdentityError::Unsupported)
        ));
    }
}
