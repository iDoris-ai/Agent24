//! Signature verification of installed executables (ADR-038 D2a).

use crate::{install::InstallError, lock::ComponentLock};
use std::{
    io::{self, Read},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// When a verification runs decides how much of D2a it may require.
///
/// `Install` is the full check, run while downloading and installing (the network is
/// there anyway): digests, the Developer ID codesign requirement, Team ID, Hardened
/// Runtime and, when the lock says `notarized`, Gatekeeper's online notarization verdict.
///
/// `AtRest` is for everything after that (start, restore, status re-checks): digests plus
/// the fully offline codesign checks. It never asks Gatekeeper, so a machine without a
/// network can still start what it already installed; notarization is trusted from the
/// install-time record (`notarized_verified_at` in the host receipt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    Install,
    AtRest,
}

/// What the lock demands of every listed executable.
#[derive(Debug, Clone, Copy)]
pub struct SigningRequirement<'a> {
    pub team_id: &'a str,
    /// `spctl --assess` must also accept it (Gatekeeper verdict, not just a ticket).
    pub notarized: bool,
}

/// What D2a demands of `platform`'s executables in `mode`, decided from the lock alone.
///
/// - `darwin-*`: always `Some`. A lock without `signing`, with a malformed Team ID or
///   with `notarized: false` is refused, never treated as weaker. Notarization is asked for only in
///   [`VerifyMode::Install`]; [`VerifyMode::AtRest`] stays offline.
/// - every other platform: `None`, meaning *this platform has no system signature*
///   and the sha256 from the lock (plus the reproducible-build CI) is the whole check.
///   Callers must treat `None` as that explicit decision, not as a skipped step.
pub fn signing_requirement<'a>(
    lock: &'a ComponentLock,
    platform: &str,
    mode: VerifyMode,
) -> Result<Option<SigningRequirement<'a>>, InstallError> {
    if !platform.starts_with("darwin-") {
        return Ok(None);
    }
    let signing = lock
        .signing
        .as_ref()
        .ok_or_else(|| InstallError::ManifestViolation("unsigned macOS asset".into()))?;
    if !valid_team_id(&signing.macos_team_id) {
        return Err(InstallError::ManifestViolation(
            "invalid signing team".into(),
        ));
    }
    // Notarization is not the lock's to waive: every macOS asset we install must be
    // notarized, so `notarized: false` makes the lock unusable rather than weaker.
    if !signing.notarized {
        return Err(InstallError::ManifestViolation(
            "macOS assets must be notarized (signing.notarized=false)".into(),
        ));
    }
    Ok(Some(SigningRequirement {
        team_id: &signing.macos_team_id,
        // Only an install-time check may reach out to Gatekeeper.
        notarized: mode == VerifyMode::Install,
    }))
}

pub trait SignatureVerifier {
    /// Debug OS-package fixtures only; never substitutes for Developer ID verification.
    #[cfg(debug_assertions)]
    fn verify_adhoc(&self, _executable: &Path) -> Result<(), InstallError> {
        Err(InstallError::SignatureMismatch)
    }

    /// Read the claimed team; callers must still verify its signature requirement.
    fn team_id(&self, _executable: &Path) -> Result<String, InstallError> {
        Err(InstallError::SignatureMismatch)
    }

    fn check_platform(&self) -> Result<(), InstallError> {
        Ok(())
    }
    fn verify(&self, executable: &Path, required: &SigningRequirement) -> Result<(), InstallError>;
}

/// Verifier for platforms without a system signature (Linux): digests only (D2a).
pub struct Unsigned;
impl SignatureVerifier for Unsigned {
    fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct CommandOutput {
    pub success: bool,
    pub stderr: String,
}

/// Seam for tests: the real verifier runs system tools with a cleared environment.
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput>;
}

pub struct SystemRunner;

/// A hung `spctl` (network stall) must not hold the component's operation lock forever.
pub const TOOL_DEADLINE: Duration = Duration::from_secs(60);

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        run_with_deadline(program, args, TOOL_DEADLINE)
    }
}

/// Run a tool with a cleared environment; kill it and fail closed (`TimedOut`) when it
/// has not finished by the deadline.
pub fn run_with_deadline(
    program: &str,
    args: &[&str],
    deadline: Duration,
) -> io::Result<CommandOutput> {
    let mut child = Command::new(program)
        .env_clear()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr pipe"))?;
    // Drain concurrently so a chatty tool cannot block on a full pipe.
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(1 << 20).read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let bytes = reader.join().unwrap_or_default();
            return Ok(CommandOutput {
                success: status.success(),
                stderr: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        if started.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{program} did not finish within {}s", deadline.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

const CODESIGN: &str = "/usr/bin/codesign";
const SPCTL: &str = "/usr/sbin/spctl";

/// The system verifier: `codesign` + `spctl`, on any macOS architecture.
pub struct Codesign;
impl SignatureVerifier for Codesign {
    #[cfg(debug_assertions)]
    fn verify_adhoc(&self, executable: &Path) -> Result<(), InstallError> {
        CodesignWith(SystemRunner).verify_adhoc(executable)
    }

    fn team_id(&self, executable: &Path) -> Result<String, InstallError> {
        CodesignWith(SystemRunner).team_id(executable)
    }

    fn check_platform(&self) -> Result<(), InstallError> {
        CodesignWith(SystemRunner).check_platform()
    }
    fn verify(&self, executable: &Path, required: &SigningRequirement) -> Result<(), InstallError> {
        CodesignWith(SystemRunner).verify(executable, required)
    }
}

pub struct CodesignWith<R>(pub R);

/// The requirement for "signed by this Developer ID team". The leading `=` is
/// mandatory: without it codesign reads the value as a file path (#1533).
/// The requirement text itself is `agent24_os_proc::developer_id_requirement`,
/// the same one the daemon applies at enrol time (S1-2 / E1).
pub fn team_requirement(team_id: &str) -> Result<String, InstallError> {
    agent24_os_proc::developer_id_requirement(team_id)
        .map(|r| format!("={r}"))
        .map_err(|_| InstallError::SignatureMismatch)
}

/// Apple Team IDs are ten upper-case letters or digits (the shared os-proc rule).
pub use agent24_os_proc::valid_team_id;

impl<R: CommandRunner> CodesignWith<R> {
    /// One `codesign --verify --strict -R` run. A requirement that is not met is
    /// `SignatureMismatch`; a tool failure (missing file, unreadable) is its own error.
    pub fn check_requirement(
        &self,
        path: &Path,
        requirement: &str,
        deep: bool,
    ) -> Result<(), InstallError> {
        let path = path.to_string_lossy();
        let mut args = vec!["--verify", "--strict"];
        if deep {
            args.push("--deep");
        }
        args.extend(["-R", requirement, &path]);
        let out = self.0.run(CODESIGN, &args)?;
        if out.success {
            Ok(())
        } else if out.stderr.contains("failed to satisfy")
            || out.stderr.contains("not signed")
            || out.stderr.contains("invalid signature")
            || out
                .stderr
                .contains("a sealed resource is missing or invalid")
        {
            Err(InstallError::SignatureMismatch)
        } else {
            Err(InstallError::SignatureCheckFailed(out.stderr.trim().into()))
        }
    }
}

impl<R: CommandRunner> SignatureVerifier for CodesignWith<R> {
    #[cfg(debug_assertions)]
    fn verify_adhoc(&self, executable: &Path) -> Result<(), InstallError> {
        let path = executable.to_string_lossy();
        let details = self.0.run(CODESIGN, &["-dv", "--verbose=4", &path])?;
        let signatures: Vec<_> = details
            .stderr
            .lines()
            .filter_map(|line| line.strip_prefix("Signature="))
            .collect();
        let teams: Vec<_> = details
            .stderr
            .lines()
            .filter_map(|line| line.strip_prefix("TeamIdentifier="))
            .collect();
        if !details.success || signatures != ["adhoc"] || teams != ["not set"] {
            return Err(InstallError::SignatureMismatch);
        }
        let verified = self.0.run(CODESIGN, &["--verify", "--strict", &path])?;
        if verified.success {
            Ok(())
        } else {
            Err(InstallError::SignatureMismatch)
        }
    }

    fn team_id(&self, executable: &Path) -> Result<String, InstallError> {
        let path = executable.to_string_lossy();
        let details = self.0.run(CODESIGN, &["-dv", "--verbose=4", &path])?;
        let teams: Vec<_> = details
            .stderr
            .lines()
            .filter_map(|line| line.strip_prefix("TeamIdentifier="))
            .collect();
        match teams.as_slice() {
            [team] if details.success && valid_team_id(team) => Ok((*team).to_owned()),
            _ => Err(InstallError::SignatureMismatch),
        }
    }

    fn check_platform(&self) -> Result<(), InstallError> {
        if cfg!(target_os = "macos") {
            Ok(())
        } else {
            Err(InstallError::UnsupportedPlatform)
        }
    }
    fn verify(&self, executable: &Path, required: &SigningRequirement) -> Result<(), InstallError> {
        self.check_platform()?;
        // Bundles (.app/.framework/.xpc directories) need --deep.
        let deep = executable.is_dir();
        self.check_requirement(executable, &team_requirement(required.team_id)?, deep)?;
        let path = executable.to_string_lossy();
        let details = self.0.run(CODESIGN, &["-dv", "--verbose=4", &path])?;
        if !details.success {
            return Err(InstallError::SignatureMismatch);
        }
        let teams: Vec<_> = details
            .stderr
            .lines()
            .filter_map(|line| line.strip_prefix("TeamIdentifier="))
            .collect();
        if teams != [required.team_id] {
            return Err(InstallError::SignatureMismatch);
        }
        if !details.stderr.lines().any(has_runtime_flag) {
            return Err(InstallError::HardenedRuntimeMissing);
        }
        if required.notarized {
            // `--type execute` rejects every bare command-line binary ("does not seem to
            // be an app"), notarized or not; `--type install` is the Gatekeeper verdict
            // for those. Bundles keep `execute`. The verdict text is parsed, not just the
            // exit status: it must say accepted by a Notarized Developer ID.
            let kind = if deep { "execute" } else { "install" };
            let verdict = self
                .0
                .run(SPCTL, &["--assess", "--type", kind, "-vv", &path])
                .map_err(InstallError::NotarizationCheckFailed)?;
            if !(verdict.success && notarized_verdict(&verdict.stderr)) {
                return Err(InstallError::NotNotarized);
            }
        }
        Ok(())
    }
}

fn notarized_verdict(output: &str) -> bool {
    output
        .lines()
        .any(|l| l.trim_end().ends_with(": accepted") || l.trim() == "accepted")
        && output
            .lines()
            .any(|l| l.trim() == "source=Notarized Developer ID")
}

/// `CodeDirectory v=20500 size=… flags=0x10000(runtime) hashes=…`
fn has_runtime_flag(line: &str) -> bool {
    line.starts_with("CodeDirectory")
        && line
            .split_whitespace()
            .find_map(|part| part.strip_prefix("flags="))
            .and_then(|flags| flags.split_once('('))
            .is_some_and(|(_, names)| {
                names
                    .trim_end_matches(')')
                    .split(',')
                    .any(|name| name == "runtime")
            })
}

#[cfg(test)]
mod tests;
