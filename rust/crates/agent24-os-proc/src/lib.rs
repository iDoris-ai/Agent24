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

#[cfg(unix)]
pub fn is_alive(pid: i32) -> bool {
    rustix::process::Pid::from_raw(pid)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
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
#[allow(unsafe_code)]
pub fn start_time(pid: i32) -> io::Result<ProcStart> {
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
    Ok(ProcStart {
        sec: info.pbi_start_tvsec,
        usec: info.pbi_start_tvusec,
    })
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

#[cfg(not(target_os = "macos"))]
pub fn verify_code_identity(
    _pid: i32,
    _team_id: &str,
    _allowed_identifiers: &[&str],
) -> Result<CodeIdentity, IdentityError> {
    Err(IdentityError::Unsupported)
}

#[cfg(target_os = "macos")]
pub fn verify_code_identity(
    pid: i32,
    team_id: &str,
    allowed_identifiers: &[&str],
) -> Result<CodeIdentity, IdentityError> {
    use security_framework::os::macos::code_signing::{
        Flags, GuestAttributes, SecCode, SecRequirement,
    };
    if !team_id.bytes().all(|b| b.is_ascii_alphanumeric()) || team_id.is_empty() {
        return Err(IdentityError::RequirementFailed);
    }
    let mut attributes = GuestAttributes::new();
    attributes.set_pid(pid);
    let code = SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE)
        .map_err(|_| IdentityError::SignatureInvalid)?;
    let requirement: SecRequirement =
        format!("anchor apple generic and certificate leaf[subject.OU] = \"{team_id}\"")
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
    let executable = executable_path(pid)
        .and_then(|p| p.canonicalize())
        .map_err(IdentityError::Io)?;
    check_identity_fields(
        identifier,
        signed_team,
        signed_path,
        team_id,
        allowed_identifiers,
        executable,
    )
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
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn code_identity_is_unsupported() {
        assert!(matches!(
            verify_code_identity(std::process::id() as i32, "HYAP444FN9", &["agent24"]),
            Err(IdentityError::Unsupported)
        ));
    }
}
