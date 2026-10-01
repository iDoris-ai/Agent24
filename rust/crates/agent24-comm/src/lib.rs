//! COMM-1a: `agent24-comm` — the `HyphaeRunner` layer.
//!
//! This crate is the only place that knows how to invoke the Hyphae CLI as a
//! single-shot subprocess: verifying the binary against the embedded lock
//! file, building a scrubbed environment, piping a password over stdin, and
//! parsing the `{ok:true,data}` / `{ok:false,error,message,data?}` envelope
//! Hyphae prints on stdout/stderr.
//!
//! Scope (COMM-1a only, see `docs/design/COMM-HYPHAE.md` §3 and the task that
//! produced this crate): `binary` (lock parsing + `VerifiedBinary`),
//! `password` (`Password`), and `runner` (`HyphaeRunner`, envelope parsing).
//! REST routes, daemon supervision, keychain-backed `PasswordStore`, and
//! `~/.hyphae` import are later tasks (COMM-1b onward) and are intentionally
//! not implemented here.

pub mod binary;
pub mod password;
pub mod runner;

pub use binary::{BinaryError, HyphaeLock, Sha256Digest, VerifiedBinary, current_platform};
pub use password::{PASSWORD_MAX, Password};
pub use runner::{
    ENV_ALLOW, Envelope, ExitClass, HyphaeRunner, Invocation, OUTPUT_CAP, RunnerError,
    parse_envelope,
};
