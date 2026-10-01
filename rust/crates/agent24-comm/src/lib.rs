//! COMM-1a: `agent24-comm` — the `HyphaeRunner` layer.
//!
//! This crate is the only place that knows how to invoke the Hyphae CLI as a
//! single-shot subprocess: verifying the binary against the embedded lock
//! file, building a scrubbed environment, piping a password over stdin, and
//! parsing the `{ok:true,data}` / `{ok:false,error,message,data?}` envelope
//! Hyphae prints on stdout/stderr.
//!
//! Scope: COMM-1a added `binary` (lock parsing + `VerifiedBinary`),
//! `password` (`Password`), and `runner` (`HyphaeRunner`, envelope parsing).
//! COMM-1b adds `keystore_lock` (`KeystoreWriteLock`, serializing every
//! Hyphae invocation that writes `keystore.json`) and `password_store`
//! (`PasswordStore`, the keychain/in-memory backends that hold the keystore
//! password between agent24d restarts). COMM-2a adds `error` (the REST
//! layer's closed error set, §4), `npub` (bech32 validity, G6), and `router`
//! (`CommState` + `router(CommState) -> axum::Router`: identity / contact /
//! relay). COMM-2b adds `import` (`~/.hyphae` import, §4.1): validates and
//! locks the source, copies its six files into a staging HOME, verifies
//! them (and the keystore password) against the real Hyphae CLI, then
//! commits by renaming staging into place. Send/history/outbox and daemon
//! supervision are later tasks (COMM-3 onward) and are intentionally not
//! implemented here.

pub mod binary;
pub mod error;
pub mod import;
pub mod keystore_lock;
pub mod npub;
pub mod password;
pub mod password_store;
pub mod router;
pub mod runner;

pub use binary::{BinaryError, HyphaeLock, Sha256Digest, VerifiedBinary, current_platform};
pub use error::CommError;
pub use import::{ImportReport, ImportRequest, import};
pub use keystore_lock::{KeystoreGuard, KeystoreWriteLock};
pub use npub::is_valid_npub;
pub use password::{PASSWORD_MAX, Password};
pub use password_store::{
    Account, KEYRING_SERVICE, KeyringPasswordStore, MemoryPasswordStore, PasswordStore, StoreError,
};
pub use router::{CommState, router};
pub use runner::{
    ENV_ALLOW, Envelope, ExitClass, HyphaeRunner, Invocation, OUTPUT_CAP, RunnerError,
    parse_envelope,
};
