//! Signature verification of installed executables (ADR-038 D2a).

use crate::install::InstallError;
use std::{io, path::Path, process::Command};

/// What the lock demands of every listed executable.
#[derive(Debug, Clone, Copy)]
pub struct SigningRequirement<'a> {
    pub team_id: &'a str,
    /// `spctl --assess` must also accept it (Gatekeeper verdict, not just a ticket).
    pub notarized: bool,
}

pub trait SignatureVerifier {
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

pub struct CommandOutput {
    pub success: bool,
    pub stderr: String,
}

/// Seam for tests: the real verifier runs system tools with a cleared environment.
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput>;
}

pub struct SystemRunner;
impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        let out = Command::new(program).env_clear().args(args).output()?;
        Ok(CommandOutput {
            success: out.status.success(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

const CODESIGN: &str = "/usr/bin/codesign";
const SPCTL: &str = "/usr/sbin/spctl";

/// The system verifier: `codesign` + `spctl`, on any macOS architecture.
pub struct Codesign;
impl SignatureVerifier for Codesign {
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
/// It also demands the Developer ID chain markers (intermediate 1.2.840.113635.100.6.2.6
/// and the Developer ID Application leaf 1.2.840.113635.100.6.1.13), so a Mac App Store or
/// development certificate of the same team does not pass.
pub fn team_requirement(team_id: &str) -> String {
    format!(
        "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team_id}\""
    )
}

/// Apple Team IDs are ten upper-case letters or digits.
pub fn valid_team_id(team_id: &str) -> bool {
    team_id.len() == 10
        && team_id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

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
    fn check_platform(&self) -> Result<(), InstallError> {
        if cfg!(target_os = "macos") {
            Ok(())
        } else {
            Err(InstallError::UnsupportedPlatform)
        }
    }
    fn verify(&self, executable: &Path, required: &SigningRequirement) -> Result<(), InstallError> {
        self.check_platform()?;
        if !valid_team_id(required.team_id) {
            // The id is interpolated into a codesign requirement: never pass junk through.
            return Err(InstallError::SignatureMismatch);
        }
        // Bundles (.app/.framework/.xpc directories) need --deep.
        let deep = executable.is_dir();
        self.check_requirement(executable, &team_requirement(required.team_id), deep)?;
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
                .run(SPCTL, &["--assess", "--type", kind, "-vv", &path])?;
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
