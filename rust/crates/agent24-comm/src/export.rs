//! Private-key backup and restore on top of Hyphae's non-interactive
//! `identity export --password-stdin --format ncryptsec` and
//! `identity import --password-stdin --ncryptsec-stdin` (Hyphae >= v0.28.0).
//!
//! Hyphae does the encrypting (NIP-49: scrypt + XChaCha20-Poly1305) and, on
//! restore, the decrypting. This module adds what only the daemon can:
//!
//! - **The daemon, not Hyphae, writes the file.** Export is run WITHOUT
//!   `--out`, so Hyphae returns `{nickname, npub, ncryptsec}` on stdout: that
//!   is ciphertext, and Hyphae never prints the plaintext key. (The daemon does
//!   briefly decrypt the backup itself to verify it, see below.) The daemon
//!   validates it strictly (bech32 checksum, HRP, payload length, version,
//!   log_n), proves it opens with the user's backup password and yields the
//!   identity's npub (so a backup that would not restore is never written),
//!   and only then creates the file itself, relative to a directory descriptor
//!   pinned *before* the export: `openat(O_CREAT|O_EXCL|O_NOFOLLOW, 0600)` to a
//!   random staging name, write, `fsync`, `linkat` to the final name (which
//!   fails rather than replace anything), unlink the staging name, `fsync` the
//!   directory. There is no window between "check" and "use" in which a path
//!   component or the file itself could be swapped: nothing is checked after
//!   the fact, the daemon creates what it writes.
//! - The "destination is outside the Hyphae home" check, decided from the
//!   directory descriptor's identity rather than a path string.
//! - A capability probe: older Hyphae (v0.27.x) lacks the flags, and the answer
//!   is a clear `hyphae_too_old` instead of a confusing usage error.
//!
//! **What is and is not erased.** To prove a backup restores, the daemon
//! decrypts it once with the user's backup password and derives the public key.
//! Everything it owns that holds the key or a password (the decrypted key, the
//! derived key, the normalised password, the stdin and stdout buffers) is
//! zero-on-drop and allocated at its final size so it never moves. NOT erased:
//! scrypt 0.12's internal work buffers (its `b`/`v`/`t` vectors), the JSON
//! request body held by axum/serde, and library-internal copies in
//! secp256k1/bech32/chacha20poly1305. The crate is not replaced. Non-zeroized
//! remnants can persist in freed memory until it is overwritten, so this is a
//! reduction, not a guarantee. Disabling core dumps for the daemon and its
//! children (agent24d `harden_process`) removes only ONE path to disk; swap and
//! a debugger or memory read by a same-user process are unaffected.
//!
//! Known Hyphae behaviours this relies on or works around: a backup made with
//! an EMPTY password cannot be imported (our >= 8 character rule prevents it),
//! and an unencrypted keystore would make Hyphae read the first stdin line as
//! the backup password (the in-daemon round-trip check catches that: the
//! result would not open with the user's password).

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::StatusCode;
use bech32::{Bech32, Hrp};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use crate::error::{CommError, map_envelope_failure, map_runner_error};
use crate::password::Password;
use crate::runner::{Envelope, HyphaeRunner, Invocation, RunnerError, hold_until_reaped};

pub(crate) const BACKUP_PASSWORD_MIN_CHARS: usize = 8;
pub(crate) const BACKUP_PASSWORD_MAX_BYTES: usize = 1024;
/// A NIP-49 string is ~163 bytes; anything near this is not a backup.
pub(crate) const BACKUP_FILE_MAX_BYTES: u64 = 4096;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
/// Hyphae's `{nickname, npub, ncryptsec}` line is 8 KiB at most. This is the
/// hard size of the preallocated stdout/stderr buffers, which therefore never
/// reallocate. It suffices because the nickname contract (<= 512 bytes, no
/// control or format characters, so at most `"` and `\` need escaping and the
/// JSON form is <= ~1 KiB) plus the 63-byte npub and the <= 200-byte ncryptsec
/// come to ~1.4 KiB. A name that is already longer is refused up front
/// (`nickname_too_long`) rather than failing here.
const EXPORT_OUTPUT_CAP: usize = 8 * 1024;
/// NIP-49: version 0x02, 1 <= log_n <= 19 (scrypt r = 8: 2^19 is ~512 MiB;
/// Hyphae >= 0.28.1 rejects log_n 0 as `invalid_key`, so do we).
const NIP49_MAX_LOG_N: u8 = 19;
const NIP49_PAYLOAD_LEN: usize = 91;
const NCRYPTSEC_MAX_LEN: usize = 200;
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
// NIP-49 (decrypt + validate only: Hyphae encrypts)
// ---------------------------------------------------------------------

pub(crate) type SecretKey = Zeroizing<[u8; 32]>;

/// Strict structural check of an `ncryptsec1…` string: lowercase bech32 with a
/// valid checksum and HRP `ncryptsec`, a 91-byte payload, version 0x02,
/// log_n <= 19 and a key-security byte of 0, 1 or 2. Returns the payload.
/// (Ciphertext, not a secret.)
pub(crate) fn validate_ncryptsec(text: &str) -> Result<Vec<u8>, CommError> {
    let bad = || upstream("the backup Hyphae returned is not a valid ncryptsec");
    if text.len() > NCRYPTSEC_MAX_LEN
        || !text.starts_with(NCRYPTSEC_PREFIX)
        || !text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(bad());
    }
    let (hrp, data) = bech32::decode(text).map_err(|_| bad())?;
    if hrp.as_str() != "ncryptsec"
        || data.len() != NIP49_PAYLOAD_LEN
        || data[0] != 0x02
        || data[1] == 0
        || data[1] > NIP49_MAX_LOG_N
        || data[42] > 2
    {
        return Err(bad());
    }
    Ok(data)
}

/// NFKC can lengthen text ("Ⅷ" is 3 bytes, "VIII" is 4), so the output size is
/// computed first and allocated exactly: the buffer never grows while it holds
/// the password, which would leave an unzeroed copy in the old allocation.
fn nfkc(password: &str) -> Zeroizing<String> {
    let needed: usize = password.nfkc().map(char::len_utf8).sum();
    let mut out = Zeroizing::new(String::with_capacity(needed));
    out.extend(password.nfkc());
    out
}

/// NIP-49 decrypt. `Err(None)`-style failures are all "does not open": a wrong
/// password and a damaged string are indistinguishable by design. The AEAD tag
/// comparison is constant-time (chacha20poly1305 / poly1305 crates).
pub(crate) fn decrypt_nip49(ncryptsec: &str, password: &str) -> Result<SecretKey, CommError> {
    let data = validate_ncryptsec(ncryptsec)?;
    let no_open = || upstream("the backup does not open with the backup password");
    let params = scrypt::Params::new(data[1], 8, 1).map_err(|_| no_open())?;
    let mut key: Zeroizing<[u8; 32]> = Zeroizing::new([0u8; 32]);
    scrypt::scrypt(
        nfkc(password).as_bytes(),
        &data[2..18],
        &params,
        &mut key[..],
    )
    .map_err(|_| no_open())?;
    let plain = Zeroizing::new(
        XChaCha20Poly1305::new((&*key).into())
            .decrypt(
                XNonce::from_slice(&data[18..42]),
                Payload {
                    msg: &data[43..],
                    aad: &data[42..43],
                },
            )
            .map_err(|_| no_open())?,
    );
    let mut out: SecretKey = Zeroizing::new([0u8; 32]);
    if plain.len() != 32 {
        return Err(no_open());
    }
    out.copy_from_slice(&plain);
    Ok(out)
}

/// The `npub1…` of a secret key (the derivation is erased afterwards).
pub(crate) fn npub_of_secret(secret: &[u8; 32]) -> Option<String> {
    let secp = secp256k1::Secp256k1::new();
    let mut pair = secp256k1::Keypair::from_seckey_slice(&secp, secret).ok()?;
    let pk = pair.x_only_public_key().0.serialize();
    pair.non_secure_erase();
    bech32::encode::<Bech32>(Hrp::parse("npub").ok()?, &pk).ok()
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
/// free, plus the random staging name the daemon will create.
pub(crate) struct Staged {
    dest: Destination,
    staging_name: OsString,
    faults: Faults,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Faults {
    pub fail_cleanup: bool,
    pub fail_write: bool,
    pub fail_dir_sync: bool,
}

/// A published backup, plus anything that could not be confirmed afterwards.
/// The target is NEVER rolled back for these: the file is complete and valid.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PublishOutcome {
    /// The staging link could not be removed: a second link to the same
    /// ciphertext remains at this path.
    pub cleanup_failed: Option<PathBuf>,
    /// The directory could not be fsynced, so the new entry may not survive a
    /// power loss yet.
    pub durability_unconfirmed: bool,
}

impl PublishOutcome {
    pub(crate) fn warnings_json(&self) -> Option<Value> {
        let mut out = Vec::new();
        if let Some(path) = &self.cleanup_failed {
            out.push(json!({"code": "cleanup_failed", "path": path.display().to_string()}));
        }
        if self.durability_unconfirmed {
            out.push(json!({"code": "durability_unconfirmed"}));
        }
        (!out.is_empty()).then_some(Value::Array(out))
    }
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

    fn staging_path(&self) -> PathBuf {
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
    fn abandon(&self, why: &str) -> CommError {
        let gone = rustix::fs::statat(
            &self.dest.parent,
            self.staging_name.as_os_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .is_err();
        if gone || self.unlink_staging() {
            upstream(why)
        } else {
            CommError::Upstream(format!(
                "{why}; a partial encrypted file may remain at {}",
                self.staging_path().display()
            ))
        }
    }

    /// Create the staging file (`O_EXCL|O_NOFOLLOW`, 0600, our inode), write
    /// `text` + newline, fsync, publish under the final name with `linkat`
    /// (never replacing anything), remove the staging name and fsync the
    /// directory.
    pub(crate) fn write_and_publish(self, text: &str) -> Result<PublishOutcome, CommError> {
        let fd = rustix::fs::openat(
            &self.dest.parent,
            self.staging_name.as_os_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| CommError::Invalid("cannot create the destination file".to_owned()))?;
        // Belt and braces against a hostile umask or filesystem: it is our
        // inode, so tighten it before a single byte is written.
        let ok = rustix::fs::fchmod(&fd, Mode::RUSR | Mode::WUSR).is_ok()
            && rustix::fs::fstat(&fd).is_ok_and(|st| {
                st.st_uid == rustix::process::geteuid().as_raw() && st.st_nlink == 1
            });
        if !ok {
            return Err(self.abandon("the new backup file is not private to this user"));
        }
        let mut file = File::from(fd);
        let written = if self.faults.fail_write {
            Err(std::io::Error::other("injected write failure"))
        } else {
            file.write_all(text.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.sync_all())
        };
        drop(file);
        if written.is_err() {
            return Err(self.abandon("writing the backup file failed"));
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
        // Neither of the two follow-ups below rolls the target back: they are
        // reported to the caller instead.
        let mut outcome = PublishOutcome::default();
        if !self.unlink_staging() {
            tracing::warn!(
                path = %self.staging_path().display(),
                "backup published but its staging link could not be removed"
            );
            outcome.cleanup_failed = Some(self.staging_path());
        }
        // Make the new directory entry durable too.
        if self.faults.fail_dir_sync || rustix::fs::fsync(&self.dest.parent).is_err() {
            tracing::warn!("backup published but the directory could not be synced");
            outcome.durability_unconfirmed = true;
        }
        Ok(outcome)
    }
}

// ---------------------------------------------------------------------
// export
// ---------------------------------------------------------------------

/// `hyphae identity export --nickname=N --format ncryptsec --password-stdin`
/// with stdin `keystore-password\nbackup-password\n` (no `--out`: the
/// ciphertext comes back on stdout). The daemon validates it, proves it opens
/// with `backup_password` and yields `expected_npub`, and only then writes it
/// to `staged`.
pub(crate) async fn export_identity_backup(
    runner: &HyphaeRunner,
    keystore_password: Password,
    nickname: &str,
    expected_npub: &str,
    backup_password: &str,
    staged: Staged,
    gate: tokio::sync::OwnedMutexGuard<()>,
) -> Result<PublishOutcome, CommError> {
    check_line_safe(&keystore_password)?;
    let stdin = stdin_lines(&[keystore_password.as_bytes(), backup_password.as_bytes()])?;
    drop(keystore_password);
    let argv: Vec<OsString> = vec![
        "identity".into(),
        "export".into(),
        format!("--nickname={nickname}").into(),
        "--format".into(),
        "ncryptsec".into(),
    ];
    let result = runner
        .run_capped(
            Invocation {
                args: argv,
                password: Some(stdin),
                timeout: Some(OPERATION_TIMEOUT),
            },
            EXPORT_OUTPUT_CAP,
            true,
        )
        .await;
    let data = match result {
        Ok(Envelope::Ok { data }) => data,
        Ok(Envelope::Failed {
            error,
            message,
            data,
            ..
        }) => {
            return Err(if message.contains("not found") {
                CommError::NotFound("identity not found".to_owned())
            } else if error == "user_error" {
                // Hyphae >= 0.28.1 answers a `user_error` WITHOUT reading stdin
                // for an unencrypted keystore (we only ever have an encrypted
                // one, so this is unexpected) -- say what it means.
                CommError::Invalid(
                    "Hyphae refused the export: the keystore must be encrypted for a password-protected backup"
                        .to_owned(),
                )
            } else {
                map_envelope_failure(&error, &message, data)
            });
        }
        Err(e) => {
            // Hyphae could not be confirmed dead: keep the one-export-at-a-time
            // gate until it is, so no second export starts meanwhile.
            if let RunnerError::StillTerminating(reaped) = &e {
                hold_until_reaped(gate, reaped.clone());
            }
            return Err(map_runner_error(e));
        }
    };
    // `gate` stays held to the end of this function: verify and publish (scrypt,
    // secret buffers) must not overlap with a second export.
    let _gate = gate;
    let ncryptsec = data
        .get("ncryptsec")
        .and_then(Value::as_str)
        .filter(|_| data.get("npub").and_then(Value::as_str) == Some(expected_npub))
        .ok_or_else(|| upstream("Hyphae reported an unexpected export result"))?
        .to_owned();
    validate_ncryptsec(&ncryptsec)?;

    // Round trip: the backup must open with THE USER'S password and be this
    // identity's key. This also catches an unencrypted keystore (Hyphae would
    // have taken our first stdin line as the backup password). scrypt is
    // CPU-bound, so keep it off the async workers.
    let (check, pw) = (
        ncryptsec.clone(),
        Zeroizing::new(backup_password.to_owned()),
    );
    let expected = expected_npub.to_owned();
    tokio::task::spawn_blocking(move || -> Result<(), CommError> {
        let key = decrypt_nip49(&check, &pw)?;
        if npub_of_secret(&key).as_deref() == Some(expected.as_str()) {
            Ok(())
        } else {
            Err(upstream(
                "the backup does not belong to this identity; nothing was written",
            ))
        }
    })
    .await
    .map_err(|_| upstream("backup verification task failed"))??;

    staged.write_and_publish(&ncryptsec)
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
        // Hyphae's release notes: if another process changed the keystore
        // password, import answers a plain `auth_error` instead of
        // `keystore_locked`. Same meaning for us.
        "keystore_locked" | "auth_error" => coded(
            StatusCode::LOCKED,
            "keystore_locked",
            "The keystore password held by Agent24 was rejected (it may have been changed by another program). Hyphae also answers this when the backup password is wrong as well",
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

/// Test-only fixtures. `encrypt_nip49` exists only to MAKE fake Hyphae output;
/// verification always goes through the production decrypt, the official NIP-49
/// vector, the real Hyphae binary and an independent Python implementation.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod testkit {
    use super::*;

    /// NIP-49's published test vector (password "nostr").
    pub(crate) const SPEC_NCRYPTSEC: &str = "ncryptsec1qgg9947rlpvqu76pj5ecreduf9jxhselq2nae2kghhvd5g7dgjtcxfqtd67p9m0w57lspw8gsq6yphnm8623nsl8xn9j4jdzz84zm3frztj3z7s35vpzmqf6ksu8r89qk5z2zxfmu5gv8th8wclt0h4p";
    pub(crate) const SPEC_HEX: &str =
        "3501454135014541350145413501453fefb02227e449e57cf4d3a3ce05378683";

    pub(crate) fn sk() -> [u8; 32] {
        hex::decode(SPEC_HEX).unwrap().try_into().unwrap()
    }
    pub(crate) fn npub() -> String {
        npub_of_secret(&sk()).unwrap()
    }

    /// Test-only NIP-49 encrypt (log_n kept tiny so tests stay fast).
    pub(crate) fn encrypt_nip49(secret: &[u8; 32], password: &str, log_n: u8) -> String {
        let (salt, nonce) = ([9u8; 16], [7u8; 24]);
        let params = scrypt::Params::new(log_n, 8, 1).unwrap();
        let mut key = [0u8; 32];
        scrypt::scrypt(nfkc(password).as_bytes(), &salt, &params, &mut key).unwrap();
        let ct = XChaCha20Poly1305::new((&key).into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: secret,
                    aad: &[2],
                },
            )
            .unwrap();
        let mut data = vec![0x02, log_n];
        data.extend_from_slice(&salt);
        data.extend_from_slice(&nonce);
        data.push(2);
        data.extend_from_slice(&ct);
        bech32::encode::<Bech32>(Hrp::parse("ncryptsec").unwrap(), &data).unwrap()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::testkit::*;
    use super::*;

    #[test]
    fn the_official_nip49_vector_opens_with_the_production_decrypt() {
        let key = decrypt_nip49(SPEC_NCRYPTSEC, "nostr").unwrap();
        assert_eq!(hex::encode(*key), SPEC_HEX);
        assert_eq!(npub_of_secret(&key).unwrap(), npub());
        assert!(decrypt_nip49(SPEC_NCRYPTSEC, "wrong").is_err());
        assert!(decrypt_nip49(SPEC_NCRYPTSEC, "Nostr").is_err());
    }

    #[test]
    fn a_made_up_backup_round_trips_and_nfkc_equivalent_passwords_agree() {
        let text = encrypt_nip49(&sk(), "correct horse", 4);
        assert!(text.starts_with("ncryptsec1"));
        assert_eq!(*decrypt_nip49(&text, "correct horse").unwrap(), sk());
        // U+FB01 (fi ligature) normalises to "fi" under NFKC, as NIP-49 requires
        let t = encrypt_nip49(&sk(), "ﬁrst-password", 4);
        assert_eq!(*decrypt_nip49(&t, "first-password").unwrap(), sk());
    }

    #[test]
    fn nfkc_allocates_exactly_for_expanding_characters() {
        // "Ⅷ" (U+2167, 3 bytes) normalises to "VIII" (4 bytes): 24 -> 32 bytes.
        let input = "Ⅷ".repeat(8);
        assert_eq!(input.len(), 24);
        let out = nfkc(&input);
        assert_eq!(out.as_str(), "VIII".repeat(8));
        assert_eq!(out.capacity(), 32, "the buffer must never have been grown");
        let enc = encrypt_nip49(&sk(), &input, 4);
        assert!(decrypt_nip49(&enc, &"VIII".repeat(8)).is_ok());
    }

    #[test]
    fn ncryptsec_validation_is_strict() {
        let good = encrypt_nip49(&sk(), "validpassword", 4);
        assert!(validate_ncryptsec(&good).is_ok());
        assert!(validate_ncryptsec(SPEC_NCRYPTSEC).is_ok());
        let flip_last = |t: &str| {
            let mut c: Vec<u8> = t.bytes().collect();
            let last = c.len() - 1;
            c[last] = if c[last] == b'q' { b'p' } else { b'q' };
            String::from_utf8(c).unwrap()
        };
        let reencode =
            |data: &[u8]| bech32::encode::<Bech32>(Hrp::parse("ncryptsec").unwrap(), data).unwrap();
        let payload = validate_ncryptsec(&good).unwrap();
        let tweak = |i: usize, v: u8| {
            let mut d = payload.clone();
            d[i] = v;
            reencode(&d)
        };
        for (name, bad) in [
            ("bad checksum", flip_last(&good)),
            ("upper case", good.to_uppercase()),
            (
                "wrong hrp",
                bech32::encode::<Bech32>(Hrp::parse("nsec").unwrap(), &payload).unwrap(),
            ),
            ("truncated", reencode(&payload[..90])),
            ("too long", reencode(&[payload.clone(), vec![0]].concat())),
            ("version 1", tweak(0, 1)),
            ("log_n 0", tweak(1, 0)),
            ("log_n 20", tweak(1, 20)),
            ("key security 3", tweak(42, 3)),
            ("not bech32", "ncryptsec1!!!".to_owned()),
            ("empty", String::new()),
            ("oversized", format!("ncryptsec1{}", "q".repeat(300))),
            ("trailing space", format!("{good} ")),
        ] {
            assert!(validate_ncryptsec(&bad).is_err(), "{name}");
        }
        // log_n 1 and 19 are the allowed floor and ceiling
        assert!(validate_ncryptsec(&tweak(1, 19)).is_ok());
        assert!(validate_ncryptsec(&tweak(1, 1)).is_ok());
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
            // Hyphae's release notes: a keystore password changed by another
            // process surfaces as a plain auth_error on import
            ("auth_error", "keystore_locked"),
        ] {
            match f(code) {
                ImportFailure::Failed(CommError::Coded(_, c, _)) => assert_eq!(c, expect, "{code}"),
                _ => panic!("{code}"),
            }
        }
        assert!(matches!(
            f("user_error"),
            ImportFailure::Failed(CommError::Invalid(_))
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

    #[test]
    fn the_daemon_creates_a_0600_single_link_file_and_leaves_no_staging_name() {
        let (_t, home, out) = sandbox();
        let dest = out.join("a.ncryptsec");
        Staged::begin(&dest, &home)
            .unwrap()
            .write_and_publish(SPEC_NCRYPTSEC)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            format!("{SPEC_NCRYPTSEC}\n")
        );
        let meta = std::fs::metadata(&dest).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.nlink(), 1);
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    }

    #[test]
    fn follow_up_failures_after_publishing_are_reported_not_rolled_back() {
        let (_t, home, out) = sandbox();
        let published = |name: &str, faults: Faults| {
            let dest = out.join(name);
            let staged = Staged::begin_with(&dest, &home, faults).unwrap();
            let outcome = staged.write_and_publish(SPEC_NCRYPTSEC).unwrap();
            // the target is complete and stays, whatever the follow-ups did
            assert_eq!(
                std::fs::read_to_string(&dest).unwrap(),
                format!("{SPEC_NCRYPTSEC}\n")
            );
            (dest, outcome)
        };

        let (_, clean) = published("a.ncryptsec", Faults::default());
        assert_eq!(clean, PublishOutcome::default());
        assert!(clean.warnings_json().is_none());

        // the staging link cannot be removed: success + cleanup_failed naming the path
        let (dest, o) = published(
            "b.ncryptsec",
            Faults {
                fail_cleanup: true,
                ..Faults::default()
            },
        );
        let leftover = o.cleanup_failed.clone().expect("cleanup_failed");
        assert!(leftover.to_string_lossy().contains(".agent24-backup-"));
        assert!(
            leftover.exists(),
            "the remaining staging link is where the warning says"
        );
        assert!(!o.durability_unconfirmed);
        assert_eq!(
            std::fs::metadata(&dest).unwrap().nlink(),
            2,
            "the second link is the staging name"
        );
        let w = o.warnings_json().unwrap();
        assert_eq!(w[0]["code"], "cleanup_failed");
        assert_eq!(w[0]["path"], leftover.display().to_string().as_str());

        // the directory cannot be fsynced: success + durability_unconfirmed only
        let (_, o) = published(
            "c.ncryptsec",
            Faults {
                fail_dir_sync: true,
                ..Faults::default()
            },
        );
        assert!(o.cleanup_failed.is_none() && o.durability_unconfirmed);
        assert_eq!(
            o.warnings_json().unwrap()[0]["code"],
            "durability_unconfirmed"
        );

        // both at once
        let (_, o) = published(
            "d.ncryptsec",
            Faults {
                fail_cleanup: true,
                fail_dir_sync: true,
                fail_write: false,
            },
        );
        assert!(o.cleanup_failed.is_some() && o.durability_unconfirmed);
        assert_eq!(o.warnings_json().unwrap().as_array().unwrap().len(), 2);
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
        // appearing between begin() and publish(): linkat refuses, target kept, staging removed
        let dest = out.join("e.ncryptsec");
        let staged = Staged::begin(&dest, &home).unwrap();
        std::fs::write(&dest, "raced in").unwrap();
        assert!(matches!(
            staged.write_and_publish(SPEC_NCRYPTSEC),
            Err(CommError::Conflict(..))
        ));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "raced in");
        assert!(!std::fs::read_dir(&out).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".agent24")
        }));
        // a dangling symlink at the target counts as existing
        std::os::unix::fs::symlink(out.join("nowhere"), out.join("f.ncryptsec")).unwrap();
        assert!(Staged::begin(&out.join("f.ncryptsec"), &home).is_err());
    }

    #[test]
    fn a_failed_write_is_cleaned_up_or_names_the_remaining_partial_file() {
        let (_t, home, out) = sandbox();
        let dest = out.join("g.ncryptsec");
        let staged = Staged::begin_with(
            &dest,
            &home,
            Faults {
                fail_write: true,
                ..Faults::default()
            },
        )
        .unwrap();
        assert!(
            matches!(staged.write_and_publish("x").unwrap_err(), CommError::Upstream(m) if !m.contains("may remain"))
        );
        assert_eq!(
            std::fs::read_dir(&out).unwrap().count(),
            0,
            "staging removed, target never created"
        );

        let staged = Staged::begin_with(
            &dest,
            &home,
            Faults {
                fail_write: true,
                fail_cleanup: true,
                ..Faults::default()
            },
        )
        .unwrap();
        let CommError::Upstream(message) = staged.write_and_publish("x").unwrap_err() else {
            panic!()
        };
        let leftovers: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(leftovers.len(), 1);
        assert!(!dest.exists());
        assert!(
            message.contains("a partial encrypted file may remain"),
            "{message}"
        );
        assert!(
            message.contains(leftovers[0].to_str().unwrap()),
            "{message}"
        );
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
