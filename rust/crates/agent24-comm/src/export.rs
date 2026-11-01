//! Private-key backup and restore on top of Hyphae's non-interactive
//! `identity export --password-stdin --format ncryptsec --out` and
//! `identity import --password-stdin --ncryptsec-stdin` (Hyphae >= v0.28.0).
//!
//! Hyphae does the cryptography (NIP-49: scrypt + XChaCha20-Poly1305) and the
//! exclusive 0600 file creation. This module adds what only the daemon can:
//!
//! - **Staging, then publish.** Hyphae is told to write a random staging name
//!   in the destination directory; the daemon then re-opens that file *through
//!   the directory descriptor it pinned before the export* (so replacing a path
//!   component in between cannot redirect the check), verifies it, and
//!   publishes it under the final name with `linkat` (which fails rather than
//!   replace anything) before removing the staging name. A partial or foreign
//!   file is therefore never visible under the final name, an existing target
//!   is never touched, and a failure is cleaned up or reported by path. Letting
//!   Hyphae write the final name directly would be one step simpler but would
//!   give up all three guarantees.
//! - The "destination is outside the Hyphae home" check, decided from the
//!   directory descriptor's identity rather than a path string.
//! - A capability probe: older Hyphae (v0.27.x) has neither flag set, and the
//!   answer is a clear `hyphae_too_old` instead of a confusing usage error.
//!
//! The file only ever holds ciphertext; the plaintext key never reaches this
//! process.

use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::StatusCode;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::error::{CommError, map_envelope_failure, map_runner_error};
use crate::password::Password;
use crate::runner::{Envelope, HyphaeRunner, Invocation};

pub(crate) const BACKUP_PASSWORD_MIN_CHARS: usize = 8;
pub(crate) const BACKUP_PASSWORD_MAX_BYTES: usize = 1024;
/// A NIP-49 string is ~163 bytes; anything near this is not a backup.
pub(crate) const BACKUP_FILE_MAX_BYTES: u64 = 4096;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const NCRYPTSEC_PREFIX: &str = "ncryptsec1";

fn upstream(msg: &str) -> CommError {
    CommError::Upstream(msg.to_owned())
}

pub(crate) fn hyphae_too_old(what: &str) -> CommError {
    CommError::Coded(
        StatusCode::CONFLICT,
        "hyphae_too_old",
        format!("{what} requires Hyphae v0.28.0 or newer"),
    )
}

fn target_exists() -> CommError {
    CommError::Conflict(
        "target_exists: the destination already exists; choose another file name".to_owned(),
        Some(json!({"reason": "target_exists"})),
    )
}

// ---------------------------------------------------------------------
// password rules
// ---------------------------------------------------------------------

/// The one rule for a NEW backup password, mirrored in the desktop main
/// process and OpenAPI: at least 8 characters, at most 1024 UTF-8 bytes, a
/// single line (no CR, LF or NUL: Hyphae reads one value per stdin line) and
/// not whitespace only.
pub(crate) fn validate_backup_password(password: &str) -> Result<(), CommError> {
    let bad = |m: &str| Err(CommError::Invalid(m.to_owned()));
    if password.chars().count() < BACKUP_PASSWORD_MIN_CHARS {
        return bad("backup password must be at least 8 characters");
    }
    validate_password_line(password, "backup password")?;
    if password.trim().is_empty() {
        return bad("backup password must not be only whitespace");
    }
    Ok(())
}

/// Restoring accepts whatever the backup was made with (it may come from
/// another tool): only the stdin-line constraints apply.
pub(crate) fn validate_restore_password(password: &str) -> Result<(), CommError> {
    if password.is_empty() {
        return Err(CommError::Invalid(
            "backup password must not be empty".to_owned(),
        ));
    }
    validate_password_line(password, "backup password")
}

fn validate_password_line(password: &str, what: &str) -> Result<(), CommError> {
    if password.len() > BACKUP_PASSWORD_MAX_BYTES {
        return Err(CommError::Invalid(format!(
            "{what} must be at most 1024 bytes"
        )));
    }
    if password.contains(['\r', '\n', '\0']) {
        return Err(CommError::Invalid(format!(
            "{what} must not contain line breaks or NUL"
        )));
    }
    Ok(())
}

/// Hyphae reads secrets one per stdin line, so a keychain password that
/// contains a line break (or NUL) cannot be passed. Generated passwords are 43
/// base64url characters.
fn check_line_safe(password: &Password) -> Result<(), CommError> {
    let pw = password.as_bytes();
    if pw.is_empty() || pw.iter().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        return Err(CommError::Invalid(
            "the keystore password cannot be passed on a stdin line (line break or NUL)".to_owned(),
        ));
    }
    Ok(())
}

/// `line1\nline2[\nline3]\n` in one zero-on-drop buffer sized exactly up front
/// (it never grows while it holds a secret).
pub(crate) fn stdin_lines(lines: &[&[u8]]) -> Result<Password, CommError> {
    let total: usize = lines.iter().map(|l| l.len() + 1).sum();
    let mut buf: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(total));
    for line in lines {
        buf.extend_from_slice(line);
        buf.push(b'\n');
    }
    // `Password::new` takes ownership of a Vec; hand it a copy-free move.
    Password::new(std::mem::take(&mut *buf)).map_err(|e| CommError::Invalid(e.to_string()))
}

// ---------------------------------------------------------------------
// capability probe
// ---------------------------------------------------------------------

#[derive(Clone, Copy)]
pub(crate) enum Feature {
    Export,
    Import,
}

/// `hyphae_too_old` unless the installed binary advertises the flags this
/// module drives (decided from its own `--help`, not from a version string).
pub(crate) async fn require_feature(
    runner: &HyphaeRunner,
    feature: Feature,
) -> Result<(), CommError> {
    let (args, flags, what): (&[&str], &[&str], &str) = match feature {
        Feature::Export => (
            &["identity", "export", "--help"],
            &["--password-stdin", "--format", "--out"],
            "Exporting a key backup",
        ),
        Feature::Import => (
            &["identity", "import", "--help"],
            &["--ncryptsec-stdin", "--password-stdin"],
            "Restoring from a backup",
        ),
    };
    let help = runner.help_text(args).await.map_err(map_runner_error)?;
    if flags.iter().all(|f| help.contains(f)) {
        Ok(())
    } else {
        Err(hyphae_too_old(what))
    }
}

// ---------------------------------------------------------------------
// directory-descriptor file operations
// ---------------------------------------------------------------------

struct Destination {
    parent: OwnedFd,
    parent_path: PathBuf,
    name: OsString,
}

/// Opens the destination's parent directory ONCE and keeps the descriptor:
/// every later step is relative to it. Refuses a parent that is, or lies
/// inside, the Hyphae home (decided from the descriptor's identity by walking
/// `..`, not from a path string), and a name that is not a plain file name.
fn open_destination(dest: &Path, hyphae_home: &Path) -> Result<Destination, CommError> {
    if !dest.is_absolute() || dest.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(CommError::Invalid("path must be absolute".to_owned()));
    }
    let name = dest
        .file_name()
        .ok_or_else(|| CommError::Invalid("path must name a file".to_owned()))?
        .to_owned();
    let parent_path = dest
        .parent()
        .ok_or_else(|| CommError::Invalid("path must name a file".to_owned()))?
        .to_path_buf();
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let parent = rustix::fs::open(&parent_path, dir_flags, Mode::empty())
        .map_err(|_| CommError::Invalid("the destination folder does not exist".to_owned()))?;
    {
        // Fail closed: if the Hyphae home cannot be identified, nothing is
        // written (we could not prove the destination is outside it).
        let home = rustix::fs::stat(hyphae_home)
            .map_err(|_| upstream("cannot identify the Hyphae data directory"))?;
        let mut current = rustix::io::fcntl_dupfd_cloexec(&parent, 0)
            .map_err(|_| upstream("cannot inspect the destination folder"))?;
        loop {
            let here = rustix::fs::fstat(&current)
                .map_err(|_| upstream("cannot inspect the destination folder"))?;
            if here.st_dev == home.st_dev && here.st_ino == home.st_ino {
                return Err(CommError::Invalid(
                    "the destination must be outside the Hyphae data directory".to_owned(),
                ));
            }
            let up = rustix::fs::openat(&current, "..", dir_flags, Mode::empty())
                .map_err(|_| upstream("cannot inspect the destination folder"))?;
            let above = rustix::fs::fstat(&up)
                .map_err(|_| upstream("cannot inspect the destination folder"))?;
            if above.st_dev == here.st_dev && above.st_ino == here.st_ino {
                break; // reached the root
            }
            current = up;
        }
    }
    Ok(Destination {
        parent,
        parent_path,
        name,
    })
}

/// A destination whose parent is pinned and whose final name is known to be
/// free, plus the random staging name Hyphae will be told to create.
pub(crate) struct Staged {
    dest: Destination,
    staging_name: OsString,
    faults: Faults,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Faults {
    pub fail_cleanup: bool,
}

impl Staged {
    pub(crate) fn begin(dest: &Path, hyphae_home: &Path) -> Result<Self, CommError> {
        Self::begin_with(dest, hyphae_home, Faults::default())
    }

    pub(crate) fn begin_with(
        dest: &Path,
        hyphae_home: &Path,
        faults: Faults,
    ) -> Result<Self, CommError> {
        let d = open_destination(dest, hyphae_home)?;
        match rustix::fs::statat(&d.parent, d.name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => return Err(target_exists()),
            Err(rustix::io::Errno::NOENT) => {}
            Err(_) => {
                return Err(CommError::Invalid(
                    "cannot inspect the destination".to_owned(),
                ));
            }
        }
        let mut nonce = [0u8; 8];
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut nonce);
        let staging_name = OsString::from(format!(".agent24-backup-{}.tmp", hex::encode(nonce)));
        Ok(Self {
            dest: d,
            staging_name,
            faults,
        })
    }

    /// Where Hyphae must create the file (`--out`).
    pub(crate) fn staging_path(&self) -> PathBuf {
        self.dest.parent_path.join(&self.staging_name)
    }

    fn unlink_staging(&self) -> bool {
        !self.faults.fail_cleanup
            && rustix::fs::unlinkat(
                &self.dest.parent,
                self.staging_name.as_os_str(),
                AtFlags::empty(),
            )
            .is_ok()
    }

    /// Give up: remove whatever sits at the staging name. If that cannot be
    /// done, say a partial encrypted file may remain, and where.
    pub(crate) fn abandon(&self, why: &str) -> CommError {
        if self.unlink_staging() || !self.staging_exists() {
            upstream(why)
        } else {
            CommError::Upstream(format!(
                "{why}; a partial encrypted file may remain at {}",
                self.staging_path().display()
            ))
        }
    }

    fn staging_exists(&self) -> bool {
        rustix::fs::statat(
            &self.dest.parent,
            self.staging_name.as_os_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .is_ok()
    }

    /// Re-open the staging file THROUGH the pinned directory (never by path)
    /// and check it is what we asked for: a regular, private, single-link file
    /// owned by us holding one `ncryptsec1…` line.
    fn verify(&self) -> Result<(), &'static str> {
        let bad = || "the backup file Hyphae wrote is not what was expected";
        let fd = rustix::fs::openat(
            &self.dest.parent,
            self.staging_name.as_os_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| "the backup file did not appear in the chosen folder")?;
        let st = rustix::fs::fstat(&fd).map_err(|_| bad())?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
            || st.st_uid != rustix::process::geteuid().as_raw()
            || st.st_nlink != 1
            || (st.st_mode & 0o777) != 0o600
            || st.st_size <= 0
            || st.st_size as u64 > BACKUP_FILE_MAX_BYTES
        {
            return Err(bad());
        }
        let mut text = String::new();
        File::from(fd)
            .take(BACKUP_FILE_MAX_BYTES)
            .read_to_string(&mut text)
            .map_err(|_| bad())?;
        let line = text.strip_suffix('\n').ok_or_else(bad)?;
        if !line.starts_with(NCRYPTSEC_PREFIX) || line.contains(char::is_whitespace) {
            return Err(bad());
        }
        Ok(())
    }

    /// Verify, then publish under the final name without overwriting anything.
    pub(crate) fn publish(self) -> Result<(), CommError> {
        if let Err(why) = self.verify() {
            return Err(self.abandon(why));
        }
        match rustix::fs::linkat(
            &self.dest.parent,
            self.staging_name.as_os_str(),
            &self.dest.parent,
            self.dest.name.as_os_str(),
            AtFlags::empty(),
        ) {
            Ok(()) => {}
            Err(e) if e == rustix::io::Errno::EXIST => {
                return Err(match self.abandon("target_exists") {
                    CommError::Upstream(m) if m.contains("may remain") => CommError::Upstream(m),
                    _ => target_exists(),
                });
            }
            Err(_) => return Err(self.abandon("publishing the backup file failed")),
        }
        // Published and complete; the staging name is now just a second link.
        if !self.unlink_staging() {
            tracing::warn!(
                path = %self.staging_path().display(),
                "backup published but its staging link could not be removed"
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------
// export
// ---------------------------------------------------------------------

/// `hyphae identity export --nickname=N --format ncryptsec --out <staging>
/// --password-stdin` with stdin `keystore-password\nbackup-password\n`, then
/// verify and publish the staged file to `dest`.
pub(crate) async fn export_identity_backup(
    runner: &HyphaeRunner,
    keystore_password: Password,
    nickname: &str,
    expected_npub: &str,
    backup_password: &str,
    staged: Staged,
) -> Result<(), CommError> {
    check_line_safe(&keystore_password)?;
    let stdin = stdin_lines(&[keystore_password.as_bytes(), backup_password.as_bytes()])?;
    drop(keystore_password);
    let out = staged.staging_path();
    let argv: Vec<OsString> = vec![
        "identity".into(),
        "export".into(),
        format!("--nickname={nickname}").into(),
        "--format".into(),
        "ncryptsec".into(),
        "--out".into(),
        out.clone().into_os_string(),
    ];
    let result = runner
        .run(Invocation {
            args: argv,
            password: Some(stdin),
            timeout: Some(OPERATION_TIMEOUT),
        })
        .await;
    match result {
        Ok(Envelope::Ok { data }) => {
            let reported_ok = data.get("npub").and_then(Value::as_str) == Some(expected_npub)
                && data.get("format").and_then(Value::as_str) == Some("ncryptsec")
                && data.get("out").and_then(Value::as_str) == out.to_str();
            if !reported_ok {
                return Err(staged.abandon("Hyphae reported an unexpected export result"));
            }
            staged.publish()
        }
        Ok(Envelope::Failed {
            error,
            message,
            data,
            ..
        }) => {
            let cleanup = staged.abandon("export failed");
            let mapped = if message.contains("already exists") {
                target_exists()
            } else if message.contains("not found") {
                CommError::NotFound("identity not found".to_owned())
            } else {
                map_envelope_failure(&error, &message, data)
            };
            Err(match cleanup {
                CommError::Upstream(m) if m.contains("may remain") => CommError::Upstream(m),
                _ => mapped,
            })
        }
        Err(e) => {
            // A timeout may have left a file behind.
            let cleanup = staged.abandon("export did not finish");
            Err(match cleanup {
                CommError::Upstream(m) if m.contains("may remain") => CommError::Upstream(m),
                _ => map_runner_error(e),
            })
        }
    }
}

// ---------------------------------------------------------------------
// restore
// ---------------------------------------------------------------------

/// Reads a backup file safely: absolute path, parent opened once, the file
/// opened relative to it with `O_NOFOLLOW|O_NONBLOCK` (so a FIFO cannot hang
/// us), regular file only, at most 4 KiB, a single `ncryptsec1…` line.
pub(crate) fn read_backup_file(path: &Path) -> Result<String, CommError> {
    let invalid = |m: &str| CommError::Invalid(m.to_owned());
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(invalid("path must be absolute"));
    }
    let name = path
        .file_name()
        .ok_or_else(|| invalid("path must name a file"))?;
    let parent_path = path
        .parent()
        .ok_or_else(|| invalid("path must name a file"))?;
    let parent = rustix::fs::open(
        parent_path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| invalid("the backup file's folder does not exist"))?;
    let fd = rustix::fs::openat(
        &parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| {
        if e == rustix::io::Errno::NOENT {
            CommError::NotFound("backup file not found".to_owned())
        } else {
            invalid("cannot open the backup file (it must be a regular file, not a link)")
        }
    })?;
    let st = rustix::fs::fstat(&fd).map_err(|_| invalid("cannot inspect the backup file"))?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return Err(invalid("the backup must be a regular file"));
    }
    if st.st_size <= 0 || st.st_size as u64 > BACKUP_FILE_MAX_BYTES {
        return Err(invalid("the backup file has an unexpected size"));
    }
    let mut text = String::new();
    File::from(fd)
        .take(BACKUP_FILE_MAX_BYTES)
        .read_to_string(&mut text)
        .map_err(|_| invalid("the backup file is not text"))?;
    let line = text.strip_suffix('\n').unwrap_or(&text);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if !line.starts_with(NCRYPTSEC_PREFIX)
        || line.contains(|c: char| c.is_whitespace() || !c.is_ascii_graphic())
    {
        return Err(CommError::Coded(
            StatusCode::BAD_REQUEST,
            "invalid_key",
            "the file is not an ncryptsec backup".to_owned(),
        ));
    }
    Ok(line.to_owned())
}

/// What Hyphae's `identity import` stable error codes mean to us.
/// Matched on the code STRING (exit codes 1 and 3 are shared).
pub(crate) enum ImportFailure {
    /// `npub_exists`: the key is already in the keystore; nothing was written.
    AlreadyPresent {
        message: String,
    },
    Failed(CommError),
}

pub(crate) fn classify_import_failure(
    error: &str,
    message: &str,
    data: Option<Value>,
) -> ImportFailure {
    let coded = |status, code: &'static str, text: &str| {
        ImportFailure::Failed(CommError::Coded(status, code, text.to_owned()))
    };
    match error {
        "npub_exists" => ImportFailure::AlreadyPresent {
            message: message.to_owned(),
        },
        "bad_backup_password" => coded(
            StatusCode::BAD_REQUEST,
            "bad_backup_password",
            "The backup password is incorrect, or the backup is damaged",
        ),
        "invalid_key" => coded(
            StatusCode::BAD_REQUEST,
            "invalid_key",
            "The file is not a valid ncryptsec backup",
        ),
        "nickname_exists" => coded(
            StatusCode::CONFLICT,
            "nickname_exists",
            "An identity with that nickname already exists",
        ),
        "keystore_locked" => coded(
            StatusCode::LOCKED,
            "keystore_locked",
            "The keystore password held by Agent24 was rejected",
        ),
        other => ImportFailure::Failed(map_envelope_failure(other, message, data)),
    }
}

/// The nickname Hyphae names in its `npub_exists` message
/// ("this key is already stored as identity 'NAME'"), if it can be read; the
/// caller must still confirm it against `identity list`.
pub(crate) fn nickname_from_npub_exists(message: &str) -> Option<&str> {
    let rest = message.split_once("identity '")?.1;
    let end = rest.rfind('\'')?;
    Some(&rest[..end])
}

/// stdin for `identity import --password-stdin --ncryptsec-stdin`: keystore
/// password, backup password, the ncryptsec string (fixed order, one per line).
pub(crate) fn import_stdin(
    keystore_password: &Password,
    backup_password: &str,
    ncryptsec: &str,
) -> Result<Password, CommError> {
    check_line_safe(keystore_password)?;
    stdin_lines(&[
        keystore_password.as_bytes(),
        backup_password.as_bytes(),
        ncryptsec.as_bytes(),
    ])
}

/// Test-only: an independent NIP-49 decrypt (never used by the daemon, which
/// leaves all cryptography to Hyphae) plus fixtures.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod testkit {
    use bech32::{Bech32, Hrp};
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use unicode_normalization::UnicodeNormalization;

    /// NIP-49's published test vector (password "nostr").
    pub(crate) const SPEC_NCRYPTSEC: &str = "ncryptsec1qgg9947rlpvqu76pj5ecreduf9jxhselq2nae2kghhvd5g7dgjtcxfqtd67p9m0w57lspw8gsq6yphnm8623nsl8xn9j4jdzz84zm3frztj3z7s35vpzmqf6ksu8r89qk5z2zxfmu5gv8th8wclt0h4p";
    pub(crate) const SPEC_HEX: &str =
        "3501454135014541350145413501453fefb02227e449e57cf4d3a3ce05378683";

    pub(crate) fn decrypt_nip49(ncryptsec: &str, password: &str) -> Option<[u8; 32]> {
        let (hrp, data) = bech32::decode(ncryptsec).ok()?;
        if hrp.as_str() != "ncryptsec" || data.len() != 91 || data[0] != 2 {
            return None;
        }
        let pw: String = password.nfkc().collect();
        let params = scrypt::Params::new(data[1], 8, 1).ok()?;
        let mut key = [0u8; 32];
        scrypt::scrypt(pw.as_bytes(), &data[2..18], &params, &mut key).ok()?;
        let plain = XChaCha20Poly1305::new((&key).into())
            .decrypt(
                XNonce::from_slice(&data[18..42]),
                Payload {
                    msg: &data[43..],
                    aad: &data[42..43],
                },
            )
            .ok()?;
        plain.try_into().ok()
    }

    pub(crate) fn npub_of(secret: &[u8; 32]) -> String {
        let secp = secp256k1::Secp256k1::new();
        let pair = secp256k1::Keypair::from_seckey_slice(&secp, secret).unwrap();
        let pk = pair.x_only_public_key().0.serialize();
        bech32::encode::<Bech32>(Hrp::parse("npub").unwrap(), &pk).unwrap()
    }

    pub(crate) fn sk() -> [u8; 32] {
        hex::decode(SPEC_HEX).unwrap().try_into().unwrap()
    }
    pub(crate) fn npub() -> String {
        npub_of(&sk())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::testkit::*;
    use super::*;

    #[test]
    fn official_vector_decrypts_with_the_test_helper() {
        assert_eq!(
            hex::encode(decrypt_nip49(SPEC_NCRYPTSEC, "nostr").unwrap()),
            SPEC_HEX
        );
        assert!(decrypt_nip49(SPEC_NCRYPTSEC, "wrong").is_none());
    }

    #[test]
    fn backup_password_rule_is_one_line_8_chars_1024_bytes_not_blank() {
        assert!(validate_backup_password("12345678").is_ok());
        assert!(validate_backup_password("pässwörd-ünï").is_ok());
        for bad in [
            "short",
            "        ",
            "a\nb-long-enough",
            "a\rb-long-enough",
            "a\0b-long-enough",
        ] {
            assert!(validate_backup_password(bad).is_err(), "{bad:?}");
        }
        assert!(validate_backup_password(&"a".repeat(1024)).is_ok());
        assert!(validate_backup_password(&"a".repeat(1025)).is_err());
        assert!(validate_backup_password(&"é".repeat(513)).is_err());
    }

    #[test]
    fn restore_password_only_needs_to_fit_a_stdin_line() {
        assert!(validate_restore_password("x").is_ok());
        assert!(validate_restore_password("").is_err());
        assert!(validate_restore_password("a\nb").is_err());
        assert!(validate_restore_password(&"a".repeat(1025)).is_err());
    }

    #[test]
    fn keystore_passwords_with_line_breaks_cannot_be_passed() {
        assert!(check_line_safe(&Password::generate([5u8; 32])).is_ok());
        for bad in [&b"a\nb"[..], b"a\rb", b"a\0b"] {
            assert!(check_line_safe(&Password::new(bad.to_vec()).unwrap()).is_err());
        }
    }

    #[test]
    fn stdin_is_one_value_per_line_in_the_given_order() {
        let p = stdin_lines(&[b"ks", b"backup", b"ncryptsec1x"]).unwrap();
        assert_eq!(p.as_bytes(), b"ks\nbackup\nncryptsec1x\n");
        let p = import_stdin(
            &Password::new(b"ks".to_vec()).unwrap(),
            "backup",
            "ncryptsec1x",
        )
        .unwrap();
        assert_eq!(p.as_bytes(), b"ks\nbackup\nncryptsec1x\n");
    }

    #[test]
    fn import_failures_are_classified_on_the_code_string() {
        let f = |code: &str| classify_import_failure(code, "msg", None);
        assert!(matches!(
            f("npub_exists"),
            ImportFailure::AlreadyPresent { .. }
        ));
        for (code, expect) in [
            ("bad_backup_password", "bad_backup_password"),
            ("invalid_key", "invalid_key"),
            ("nickname_exists", "nickname_exists"),
            ("keystore_locked", "keystore_locked"),
        ] {
            match f(code) {
                ImportFailure::Failed(CommError::Coded(_, c, _)) => assert_eq!(c, expect),
                _ => panic!("{code}"),
            }
        }
        // exit codes are shared (1, 3), so an unknown code never masquerades
        assert!(matches!(
            f("user_error"),
            ImportFailure::Failed(CommError::Invalid(_))
        ));
        assert!(matches!(
            f("auth_error"),
            ImportFailure::Failed(CommError::Locked(_))
        ));
        assert_eq!(
            nickname_from_npub_exists("this key is already stored as identity 'r1'"),
            Some("r1")
        );
        assert_eq!(nickname_from_npub_exists("something else"), None);
    }

    fn sandbox() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".hyphae")).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        (tmp, home, out)
    }

    fn plant(staged: &Staged, content: &str, mode: u32) {
        let path = staged.staging_path();
        std::fs::write(&path, content).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_good_staged_file_is_published_without_a_trace_of_the_staging_name() {
        let (_t, home, out) = sandbox();
        let dest = out.join("a.ncryptsec");
        let staged = Staged::begin(&dest, &home).unwrap();
        plant(&staged, &format!("{SPEC_NCRYPTSEC}\n"), 0o600);
        staged.publish().unwrap();
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            format!("{SPEC_NCRYPTSEC}\n")
        );
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::metadata(&dest).unwrap().nlink(), 1);
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    }

    #[test]
    fn files_that_are_not_what_we_asked_for_are_rejected_and_removed() {
        let (_t, home, out) = sandbox();
        for (content, mode) in [
            ("not a backup\n".to_owned(), 0o600),
            (SPEC_NCRYPTSEC.to_string(), 0o600),    // no newline
            (format!("{SPEC_NCRYPTSEC}\n"), 0o644), // not private
            (format!("{SPEC_NCRYPTSEC}\nextra line\n"), 0o600),
            (String::new(), 0o600),
        ] {
            let dest = out.join("b.ncryptsec");
            let staged = Staged::begin(&dest, &home).unwrap();
            plant(&staged, &content, mode);
            assert!(staged.publish().is_err(), "{content:?} {mode:o}");
            assert!(!dest.exists());
            assert_eq!(
                std::fs::read_dir(&out).unwrap().count(),
                0,
                "staging removed"
            );
        }
    }

    #[test]
    fn a_hard_linked_or_symlinked_staging_file_is_rejected() {
        let (_t, home, out) = sandbox();
        let victim = out.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let staged = Staged::begin(&out.join("c.ncryptsec"), &home).unwrap();
        std::os::unix::fs::symlink(&victim, staged.staging_path()).unwrap();
        assert!(staged.publish().is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        let staged = Staged::begin(&out.join("c.ncryptsec"), &home).unwrap();
        std::fs::write(staged.staging_path(), format!("{SPEC_NCRYPTSEC}\n")).unwrap();
        std::fs::set_permissions(
            staged.staging_path(),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::hard_link(staged.staging_path(), out.join("second-link")).unwrap();
        assert!(staged.publish().is_err(), "nlink != 1");
    }

    #[test]
    fn an_existing_target_is_refused_up_front_and_never_touched() {
        let (_t, home, out) = sandbox();
        let dest = out.join("d.ncryptsec");
        std::fs::write(&dest, "precious").unwrap();
        let err = Staged::begin(&dest, &home).err().unwrap();
        assert!(
            matches!(&err, CommError::Conflict(m, Some(d)) if m.contains("target_exists") && d["reason"] == "target_exists")
        );
        // appearing between begin() and publish(): linkat refuses, target kept
        let dest = out.join("e.ncryptsec");
        let staged = Staged::begin(&dest, &home).unwrap();
        plant(&staged, &format!("{SPEC_NCRYPTSEC}\n"), 0o600);
        std::fs::write(&dest, "raced in").unwrap();
        assert!(matches!(staged.publish(), Err(CommError::Conflict(..))));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "raced in");
        // a dangling symlink at the target counts as existing
        std::os::unix::fs::symlink(out.join("nowhere"), out.join("f.ncryptsec")).unwrap();
        assert!(Staged::begin(&out.join("f.ncryptsec"), &home).is_err());
    }

    #[test]
    fn cleanup_failure_names_the_partial_file_that_may_remain() {
        let (_t, home, out) = sandbox();
        let staged = Staged::begin_with(
            &out.join("g.ncryptsec"),
            &home,
            Faults { fail_cleanup: true },
        )
        .unwrap();
        plant(&staged, "garbage\n", 0o600);
        let path = staged.staging_path();
        let CommError::Upstream(message) = staged.publish().unwrap_err() else {
            panic!()
        };
        assert!(
            message.contains("a partial encrypted file may remain"),
            "{message}"
        );
        assert!(message.contains(path.to_str().unwrap()), "{message}");
        assert!(path.exists());
    }

    #[test]
    fn destination_rules_hold_for_links_homes_and_relative_paths() {
        let (tmp, home, out) = sandbox();
        assert!(Staged::begin(Path::new("relative.txt"), &home).is_err());
        assert!(Staged::begin(&out.join("a").join("b"), &home).is_err());
        assert!(Staged::begin(&home.join("x"), &home).is_err());
        assert!(Staged::begin(&home.join(".hyphae").join("x"), &home).is_err());
        let via = tmp.path().join("via");
        std::os::unix::fs::symlink(home.join(".hyphae"), &via).unwrap();
        assert!(Staged::begin(&via.join("x"), &home).is_err());
        // fail closed when the Hyphae home cannot be identified
        assert!(Staged::begin(&out.join("fresh"), &tmp.path().join("no-such-home")).is_err());
    }

    #[test]
    fn restore_reads_only_a_small_regular_ncryptsec_file() {
        let (_t, _home, out) = sandbox();
        let good = out.join("ok.ncryptsec");
        std::fs::write(&good, format!("{SPEC_NCRYPTSEC}\n")).unwrap();
        assert_eq!(read_backup_file(&good).unwrap(), SPEC_NCRYPTSEC);
        std::fs::write(out.join("crlf"), format!("{SPEC_NCRYPTSEC}\r\n")).unwrap();
        assert_eq!(read_backup_file(&out.join("crlf")).unwrap(), SPEC_NCRYPTSEC);

        for (name, body) in [
            ("plain", "hello world\n".to_owned()),
            (
                "nsec",
                "nsec1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq\n".to_owned(),
            ),
            ("multi", format!("{SPEC_NCRYPTSEC}\nsecond\n")),
            ("spaced", format!("{SPEC_NCRYPTSEC} \n")),
            ("empty", String::new()),
            ("big", format!("ncryptsec1{}", "q".repeat(5000))),
        ] {
            std::fs::write(out.join(name), body).unwrap();
            assert!(read_backup_file(&out.join(name)).is_err(), "{name}");
        }
        std::os::unix::fs::symlink(&good, out.join("link")).unwrap();
        assert!(read_backup_file(&out.join("link")).is_err(), "symlink");
        assert!(read_backup_file(&out).is_err(), "directory");
        assert!(matches!(
            read_backup_file(&out.join("missing")),
            Err(CommError::NotFound(_))
        ));
        assert!(read_backup_file(Path::new("rel")).is_err());
        // a FIFO must be refused, not block
        let fifo = out.join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        assert!(read_backup_file(&fifo).is_err());
    }
}
