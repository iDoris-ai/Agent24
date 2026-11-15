use agent24_protocol::state_file::AuthMode;

/// Another daemon holds the singleton lock: the start that loses a
/// concurrent-start race. Its `failed` line says `already_running`, so a
/// launcher knows to look for the winner instead of reporting an error.
#[derive(Debug)]
pub struct AlreadyRunning;

impl std::fmt::Display for AlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another agent24d is already running (singleton lock held)")
    }
}

impl std::error::Error for AlreadyRunning {}

/// How this process is started (CORE-2 D8). The single source of the
/// bootstrap / enroll matrix; `serve` acts on the returned mode only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupMode {
    /// Single-token daemon, unchanged.
    Legacy,
    /// Capability daemon owned by a trusted parent over stdio pipes: the
    /// parent-EOF lease applies and the enrolment endpoint opens by itself.
    HostChild,
    /// Capability daemon run by a service manager: no pipes, no parent lease,
    /// no host credential; the enrolment endpoint is the only way in.
    Service,
}

/// Only a host-child daemon has a trusted pipe to hand a `ProductHost`
/// credential to; a service never mints one (CORE-2 R5, T17).
pub(super) fn mints_product_host(mode: StartupMode) -> bool {
    mode == StartupMode::HostChild
}

/// Validate the auth / bootstrap / enroll combination. Mutually exclusive by
/// construction, not by argument order; bootstrap together with enroll is
/// refused as ambiguous (§9 Q5).
pub(super) fn validate_auth_startup(
    auth_mode: AuthMode,
    host_bootstrap_stdio: bool,
    enroll_endpoint: bool,
    ephemeral: bool,
) -> Result<StartupMode, std::io::Error> {
    use std::io::{Error, ErrorKind};
    match (auth_mode, host_bootstrap_stdio, enroll_endpoint) {
        (AuthMode::LegacySingleToken, false, false) => Ok(StartupMode::Legacy),
        (AuthMode::LegacySingleToken, true, _) => Err(Error::new(
            ErrorKind::InvalidInput,
            "host bootstrap stdio is valid only in capability mode",
        )),
        (AuthMode::LegacySingleToken, false, true) => Err(Error::new(
            ErrorKind::InvalidInput,
            "--enroll-endpoint is valid only in capability mode",
        )),
        (AuthMode::Capabilities, true, false) => Ok(StartupMode::HostChild),
        (AuthMode::Capabilities, false, false) => Err(Error::new(
            ErrorKind::PermissionDenied,
            "capability mode requires trusted host bootstrap stdio or --enroll-endpoint",
        )),
        (AuthMode::Capabilities, true, true) => Err(Error::new(
            ErrorKind::InvalidInput,
            "host bootstrap stdio and --enroll-endpoint are mutually exclusive",
        )),
        (AuthMode::Capabilities, false, true) if ephemeral => Err(Error::new(
            ErrorKind::InvalidInput,
            "--enroll-endpoint needs a discoverable daemon and cannot be ephemeral",
        )),
        (AuthMode::Capabilities, false, true) => Ok(StartupMode::Service),
    }
}
