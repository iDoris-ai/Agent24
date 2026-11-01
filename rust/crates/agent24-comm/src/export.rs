//! `POST /comm/identity/export`: write a password-encrypted (NIP-49
//! `ncryptsec`) backup of an identity's private key to a user-chosen file.
//! The plaintext key never reaches a renderer, a log, an HTTP response, a
//! temp file or the disk.
//!
//! Hyphae v0.27.2's `identity export` has no `--password-stdin`: it only
//! reads the keystore password from an interactive terminal and refuses in
//! JSON mode. So this module runs it on a private pseudo-terminal, answers
//! the password prompt from the keychain-held password, reads the key out of
//! a bounded, zeroized buffer, proves it belongs to the identity (derives the
//! public key and compares it with `identity list`), encrypts it with the
//! user's backup password and creates the destination with `O_EXCL`.
//! When Hyphae gains `--password-stdin` for export, [`export_secret_key`] is
//! the only function that has to change.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bech32::primitives::decode::CheckedHrpstring;
use bech32::{Bech32, Hrp};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::pty::{grantpt, ptsname, unlockpt};
use rustix::termios::LocalModes;
use serde_json::json;
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use crate::error::CommError;
use crate::password::Password;
use crate::runner::{HyphaeRunner, kill_process_group_best_effort, lock_spawn_gate};

const PROMPT: &[u8] = b"assword:";
/// The whole transcript of a successful export is ~300 bytes; this is the hard
/// bound of the preallocated buffer, which therefore never reallocates.
const READ_CAP: usize = 8 * 1024;
const EXPORT_TIMEOUT: Duration = Duration::from_secs(30);
/// Hyphae turns echo off right after printing the prompt. The password is only
/// written once the pty really reports ECHO off (never on a guess), and never
/// at all if that takes longer than this.
const ECHO_OFF_WAIT: Duration = Duration::from_secs(5);
/// A canonical-mode tty line holds at most MAX_CANON (1024) bytes, control bytes
/// are interpreted, and the keystore password is one line: refuse what the line
/// discipline would silently mangle.
const TTY_LINE_MAX: usize = 1000;

/// NIP-49 defaults: scrypt N=2^16, r=8, p=1; key-security byte 0x02 ("unknown":
/// the key has been handled by software that cannot attest to its history).
pub(crate) const NIP49_LOG_N: u8 = 16;
const NIP49_VERSION: u8 = 0x02;
const NIP49_KEY_SECURITY: u8 = 0x02;
pub(crate) const BACKUP_PASSWORD_MIN_CHARS: usize = 8;
pub(crate) const BACKUP_PASSWORD_MAX_BYTES: usize = 1024;

pub(crate) type SecretKey = Zeroizing<[u8; 32]>;

fn upstream(msg: &str) -> CommError {
    CommError::Upstream(msg.to_owned())
}

/// The one rule for the backup password, mirrored in the desktop main process
/// and OpenAPI: at least 8 characters, at most 1024 UTF-8 bytes, a single line
/// (no CR, LF or NUL: Hyphae's import protocol reads one value per stdin line)
/// and not whitespace only.
pub(crate) fn validate_backup_password(password: &str) -> Result<(), CommError> {
    let bad = |m: &str| Err(CommError::Invalid(m.to_owned()));
    if password.chars().count() < BACKUP_PASSWORD_MIN_CHARS {
        return bad("backup password must be at least 8 characters");
    }
    if password.len() > BACKUP_PASSWORD_MAX_BYTES {
        return bad("backup password must be at most 1024 bytes");
    }
    if password.contains(['\r', '\n', '\0']) {
        return bad("backup password must not contain line breaks or NUL");
    }
    if password.trim().is_empty() {
        return bad("backup password must not be only whitespace");
    }
    Ok(())
}

/// The keychain password is typed to a terminal; refuse one the tty line
/// discipline would alter (generated passwords are 43 base64url characters).
fn check_typable(password: &Password) -> Result<(), CommError> {
    let pw = password.as_bytes();
    if pw.len() >= TTY_LINE_MAX || pw.iter().any(|b| *b < 0x20 || *b == 0x7f) {
        return Err(CommError::Invalid(
            "the keystore password cannot be entered through a terminal (control characters or \
             1000+ bytes); export is unavailable for this keystore"
                .to_owned(),
        ));
    }
    Ok(())
}

/// A pty pair that is close-on-exec from the instant it exists. The master is
/// opened with `open("/dev/ptmx", O_RDWR|O_NOCTTY|O_CLOEXEC)` (what
/// `posix_openpt` does, but with the flag applied atomically: Apple's
/// `posix_openpt` has no `O_CLOEXEC`, and `openpt` + a later `fcntl` leaves a
/// window in which ANY fork/exec in this process, from any crate, would
/// inherit the master and could read the key). There is no such window here
/// and nothing relies on a lock. The slave is opened `O_CLOEXEC` too; the
/// exporting child gets it only as its dup2'd stdio (dup2 clears the flag on
/// the target descriptors).
fn open_pty() -> Result<(OwnedFd, OwnedFd), CommError> {
    let io_err = |_| upstream("could not allocate a terminal for identity export");
    let master = rustix::fs::open(
        "/dev/ptmx",
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io_err)?;
    grantpt(&master).map_err(io_err)?;
    unlockpt(&master).map_err(io_err)?;
    let slave_name = ptsname(&master, Vec::new()).map_err(io_err)?;
    let slave = rustix::fs::open(
        slave_name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io_err)?;
    Ok((master, slave))
}

// ---------------------------------------------------------------------
// public-key helpers
// ---------------------------------------------------------------------

/// The 32-byte x-only public key inside an `npub1…` string.
pub(crate) fn npub_to_pubkey(npub: &str) -> Option<[u8; 32]> {
    let (hrp, data) = bech32::decode(npub).ok()?;
    if hrp.as_str() != "npub" {
        return None;
    }
    <[u8; 32]>::try_from(data.as_slice()).ok()
}

fn pubkey_of(secret: &[u8; 32]) -> Option<[u8; 32]> {
    let secp = secp256k1::Secp256k1::new();
    let mut pair = secp256k1::Keypair::from_seckey_slice(&secp, secret).ok()?;
    let out = pair.x_only_public_key().0.serialize();
    pair.non_secure_erase();
    Some(out)
}

// ---------------------------------------------------------------------
// child process + pty
// ---------------------------------------------------------------------

/// Owns the exporting child. Dropping it kills the whole process group and
/// reaps it, so no exit path (timeout, cancellation, panic, early return) can
/// leave a Hyphae process behind holding the password.
struct ChildGuard {
    child: Option<std::process::Child>,
}

impl ChildGuard {
    fn try_wait(&mut self) -> Result<Option<ExitStatus>, CommError> {
        match self.child.as_mut() {
            Some(c) => c
                .try_wait()
                .map_err(|_| upstream("identity export wait failed")),
            None => Err(upstream("identity export already finished")),
        }
    }

    fn kill(&mut self) {
        if let Some(c) = self.child.as_mut() {
            if matches!(c.try_wait(), Ok(None)) {
                kill_process_group_best_effort(c.id());
                let _ = c.kill();
            }
            let _ = c.wait();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Reads the pty until EOF: waits for the password prompt, answers it, and
/// collects the rest into a fixed-capacity buffer. `cancel` is checked before
/// anything is read or written, so a task that only starts running after the
/// caller gave up never touches the password.
fn drive(
    master: OwnedFd,
    password: &Password,
    cancel: &AtomicBool,
    echo_wait: Duration,
) -> Result<Zeroizing<Vec<u8>>, CommError> {
    if cancel.load(Ordering::SeqCst) {
        return Err(CommError::Timeout);
    }
    let mut master = File::from(master);
    let mut out: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(READ_CAP));
    let mut chunk: Zeroizing<[u8; 256]> = Zeroizing::new([0u8; 256]);
    let mut answered = false;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CommError::Timeout);
        }
        let n = match master.read(&mut chunk[..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if out.len() + n > READ_CAP {
            return Err(upstream("identity export output was too large"));
        }
        out.extend_from_slice(&chunk[..n]);
        if !answered && out.windows(PROMPT.len()).any(|w| w == PROMPT) {
            // Wait for the terminal to say echo is off; abort (writing nothing)
            // if it never does.
            let until = Instant::now() + echo_wait;
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return Err(CommError::Timeout);
                }
                match rustix::termios::tcgetattr(&master) {
                    Ok(t) if !t.local_modes.contains(LocalModes::ECHO) => break,
                    _ if Instant::now() >= until => {
                        return Err(upstream(
                            "the terminal never turned echo off; the password was not sent",
                        ));
                    }
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            let pw = password.as_bytes();
            let mut line: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(pw.len() + 1));
            line.extend_from_slice(pw);
            line.push(b'\n');
            if master.write_all(&line).is_err() {
                break;
            }
            answered = true;
        }
    }
    if !answered {
        return Err(upstream(
            "identity export did not ask for the keystore password",
        ));
    }
    Ok(out)
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| {
        w.iter()
            .zip(needle)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

/// Runs `hyphae identity export --nickname <nickname>` on a pty and returns
/// the private key, verified to derive `expected_pubkey`.
pub(crate) async fn export_secret_key(
    runner: &HyphaeRunner,
    nickname: &str,
    password: Password,
    expected_pubkey: &[u8; 32],
) -> Result<SecretKey, CommError> {
    export_secret_key_within(
        runner,
        nickname,
        password,
        expected_pubkey,
        EXPORT_TIMEOUT,
        ECHO_OFF_WAIT,
    )
    .await
}

pub(crate) async fn export_secret_key_within(
    runner: &HyphaeRunner,
    nickname: &str,
    password: Password,
    expected_pubkey: &[u8; 32],
    limit: Duration,
    echo_wait: Duration,
) -> Result<SecretKey, CommError> {
    check_typable(&password)?;
    let (master, slave) = open_pty()?;

    // The child is spawned HERE, on the caller's task, and owned by `guard`
    // from the instant it exists: there is no window in which a process runs
    // without an owner that will kill it, and no queued task that can start
    // one later.
    let mut command =
        runner.interactive_command(&["identity", "export", &format!("--nickname={nickname}")]);
    let child = {
        let stdin = slave
            .try_clone()
            .map_err(|_| upstream("pty setup failed"))?;
        let stdout = slave
            .try_clone()
            .map_err(|_| upstream("pty setup failed"))?;
        command.stdin(stdin).stdout(stdout).stderr(slave);
        let spawned = {
            let _gate = lock_spawn_gate();
            command.spawn()
        }
        .map_err(|e| CommError::Upstream(format!("failed to spawn hyphae: {e}")));
        // Release the parent's copies of the slave side so the master sees EOF
        // when the child exits.
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        spawned?
    };
    drop(command);
    let mut guard = ChildGuard { child: Some(child) };

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_task = Arc::clone(&cancel);
    let deadline = Instant::now() + limit;
    let session = async {
        let out = tokio::task::spawn_blocking(move || {
            drive(master, &password, &cancel_for_task, echo_wait)
        })
        .await
        .map_err(|_| upstream("identity export task failed"))??;
        let status = loop {
            if let Some(status) = guard.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                return Err(CommError::Timeout);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        if !status.success() {
            // Classified on bounded byte matches; the transcript is never
            // copied into a String and never forwarded.
            // Only what Hyphae printed AFTER the prompt counts: the prompt
            // itself says "Keystore password" and would match everything.
            let reply = out
                .windows(PROMPT.len())
                .position(|w| w == PROMPT)
                .map_or(&out[..], |i| &out[i + PROMPT.len()..]);
            return Err(
                if contains_ci(reply, b"incorrect password")
                    || contains_ci(reply, b"unlock keystore")
                {
                    CommError::Locked("password verification failed".to_owned())
                } else if contains_ci(reply, b"not found") {
                    CommError::NotFound("identity not found".to_owned())
                } else {
                    CommError::Upstream(format!(
                        "hyphae identity export failed (exit {})",
                        status
                            .code()
                            .map_or_else(|| "signal".to_owned(), |c| c.to_string())
                    ))
                },
            );
        }
        parse_export_output(&out, expected_pubkey)
    };
    match tokio::time::timeout(limit, session).await {
        Ok(result) => result,
        Err(_elapsed) => {
            // Tell a not-yet-started reader to stand down, then kill the
            // group: that closes the slave side and unblocks a running reader.
            cancel.store(true, Ordering::SeqCst);
            guard.kill();
            Err(CommError::Timeout)
        }
    }
}

// ---------------------------------------------------------------------
// output parsing (bytes only; no String copy of anything secret)
// ---------------------------------------------------------------------

fn field<'a>(line: &'a [u8], label: &[u8]) -> Option<&'a [u8]> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let rest = line.strip_prefix(label)?;
    let start = rest
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(rest.len());
    let rest = &rest[start..];
    let end = rest
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    Some(&rest[..end])
}

/// `nsec1…` → 32 key bytes, checksum verified, straight into a zeroizing array.
fn decode_nsec(token: &[u8]) -> Option<SecretKey> {
    if token.len() != 63
        || !token
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return None;
    }
    let text = std::str::from_utf8(token).ok()?;
    let checked = CheckedHrpstring::new::<Bech32>(text).ok()?;
    if checked.hrp().as_str() != "nsec" {
        return None;
    }
    let mut key: SecretKey = Zeroizing::new([0u8; 32]);
    let mut n = 0usize;
    for byte in checked.byte_iter() {
        *key.get_mut(n)? = byte;
        n += 1;
    }
    (n == 32).then_some(key)
}

/// Strict parse of the transcript: each of `Npub:` / `Nsec:` exactly once, the
/// key's checksum valid, and the key must derive the identity's public key
/// (`expected_pubkey`, from `identity list`, not from this text).
pub(crate) fn parse_export_output(
    raw: &[u8],
    expected_pubkey: &[u8; 32],
) -> Result<SecretKey, CommError> {
    let bad = || upstream("identity export output was not recognised");
    let mut nsec: Option<SecretKey> = None;
    let mut npub_seen = false;
    let mut nsec_seen = false;
    for line in raw.split(|b| *b == b'\n') {
        if let Some(token) = field(line, b"Nsec:") {
            if nsec_seen {
                return Err(bad());
            }
            nsec_seen = true;
            nsec = decode_nsec(token);
        } else if let Some(token) = field(line, b"Npub:") {
            if npub_seen {
                return Err(bad());
            }
            npub_seen = true;
            let text = std::str::from_utf8(token).map_err(|_| bad())?;
            if npub_to_pubkey(text).as_ref() != Some(expected_pubkey) {
                return Err(bad());
            }
        }
    }
    let key = nsec.ok_or_else(bad)?;
    if !npub_seen || pubkey_of(&key).as_ref() != Some(expected_pubkey) {
        return Err(bad());
    }
    Ok(key)
}

// ---------------------------------------------------------------------
// NIP-49
// ---------------------------------------------------------------------

/// NFKC can lengthen text ("Ⅷ" is 3 bytes, "VIII" is 4), so the output size is
/// computed first and allocated exactly: the buffer never grows while it holds
/// the password, which would leave an unzeroed copy in the old allocation.
fn nfkc(password: &str) -> Zeroizing<String> {
    let needed: usize = password.nfkc().map(char::len_utf8).sum();
    let mut out = Zeroizing::new(String::with_capacity(needed));
    out.extend(password.nfkc());
    out
}

fn nip49_key(password: &str, salt: &[u8], log_n: u8) -> Result<Zeroizing<[u8; 32]>, CommError> {
    let params =
        scrypt::Params::new(log_n, 8, 1).map_err(|_| upstream("invalid scrypt parameters"))?;
    let mut key: Zeroizing<[u8; 32]> = Zeroizing::new([0u8; 32]);
    scrypt::scrypt(nfkc(password).as_bytes(), salt, &params, &mut key[..])
        .map_err(|_| upstream("key derivation failed"))?;
    Ok(key)
}

/// NIP-49: `ncryptsec1…` = bech32(0x02 | log_n | salt(16) | nonce(24) |
/// key_security | XChaCha20-Poly1305(key, nonce, aad = key_security, sk)).
pub(crate) fn encrypt_nip49(
    secret: &[u8; 32],
    password: &str,
    log_n: u8,
) -> Result<String, CommError> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut salt);
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut nonce);
    let key = nip49_key(password, &salt, log_n)?;
    let cipher = XChaCha20Poly1305::new((&*key).into());
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: secret,
                aad: &[NIP49_KEY_SECURITY],
            },
        )
        .map_err(|_| upstream("encryption failed"))?;
    let mut data = Vec::with_capacity(2 + 16 + 24 + 1 + ciphertext.len());
    data.extend_from_slice(&[NIP49_VERSION, log_n]);
    data.extend_from_slice(&salt);
    data.extend_from_slice(&nonce);
    data.push(NIP49_KEY_SECURITY);
    data.extend_from_slice(&ciphertext);
    let hrp = Hrp::parse("ncryptsec").map_err(|_| upstream("bech32 hrp"))?;
    bech32::encode::<Bech32>(hrp, &data).map_err(|_| upstream("bech32 encoding failed"))
}

#[cfg(test)]
pub(crate) fn decrypt_nip49(ncryptsec: &str, password: &str) -> Result<SecretKey, CommError> {
    let (hrp, data) = bech32::decode(ncryptsec).map_err(|_| upstream("bech32"))?;
    if hrp.as_str() != "ncryptsec" || data.len() != 91 || data[0] != NIP49_VERSION {
        return Err(upstream("not a NIP-49 v2 key"));
    }
    let key = nip49_key(password, &data[2..18], data[1])?;
    let plain = XChaCha20Poly1305::new((&*key).into())
        .decrypt(
            XNonce::from_slice(&data[18..42]),
            Payload {
                msg: &data[43..],
                aad: &data[42..43],
            },
        )
        .map_err(|_| CommError::Locked("wrong backup password".to_owned()))?;
    let mut out: SecretKey = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&plain);
    Ok(out)
}

// ---------------------------------------------------------------------
// destination file
// ---------------------------------------------------------------------

fn target_exists() -> CommError {
    CommError::Conflict(
        "target_exists: the destination already exists; choose another file name".to_owned(),
        Some(json!({"reason": "target_exists"})),
    )
}

struct Destination {
    parent: OwnedFd,
    name: std::ffi::OsString,
}

/// Opens the destination's parent directory ONCE and keeps the descriptor:
/// every later step is relative to it, so replacing a path component after
/// this point cannot redirect the write. Refuses a parent that is, or lies
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
        .ok_or_else(|| CommError::Invalid("path must name a file".to_owned()))?;
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let parent = rustix::fs::open(parent_path, dir_flags, Mode::empty())
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
    Ok(Destination { parent, name })
}

/// Cheap early refusal (before the slow pty + scrypt work). The authoritative
/// check is the `O_EXCL` create in [`write_backup_file`].
pub(crate) fn precheck_destination(dest: &Path, hyphae_home: &Path) -> Result<(), CommError> {
    let d = open_destination(dest, hyphae_home)?;
    match rustix::fs::statat(&d.parent, d.name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Err(target_exists()),
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(_) => Err(CommError::Invalid(
            "cannot inspect the destination".to_owned(),
        )),
    }
}

#[derive(Default)]
struct Faults {
    fail_write: bool,
    fail_cleanup: bool,
}

/// Writes `text` + newline to a NEW file `dest`, never replacing anything.
///
/// The ciphertext goes to a random staging name in the same directory
/// (`O_CREAT|O_EXCL|O_NOFOLLOW`, mode 0600, relative to the directory
/// descriptor), is fsynced, and is only then published under the final name
/// with `linkat` (which fails with EEXIST rather than replace anything,
/// including a dangling symlink) followed by removing the staging name. A
/// reader of the final name therefore never sees a partial file. If a failure
/// cannot be cleaned up, the error says a partial encrypted file may remain
/// and names it.
pub(crate) fn write_backup_file(
    dest: &Path,
    hyphae_home: &Path,
    text: &str,
) -> Result<(), CommError> {
    write_backup_file_with(dest, hyphae_home, text, &Faults::default())
}

fn write_backup_file_with(
    dest: &Path,
    hyphae_home: &Path,
    text: &str,
    faults: &Faults,
) -> Result<(), CommError> {
    let d = open_destination(dest, hyphae_home)?;
    let mut nonce = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut nonce);
    let staging_name =
        std::ffi::OsString::from(format!(".agent24-backup-{}.tmp", hex::encode(nonce)));
    let staging_path = dest
        .parent()
        .map_or_else(|| staging_name.clone().into(), |p| p.join(&staging_name));

    let fd = rustix::fs::openat(
        &d.parent,
        staging_name.as_os_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|_| CommError::Invalid("cannot create the destination file".to_owned()))?;

    // Remove the staging file; if that fails, say exactly what may remain.
    let discard = |why: &str| -> CommError {
        let removed = !faults.fail_cleanup
            && rustix::fs::unlinkat(&d.parent, staging_name.as_os_str(), AtFlags::empty()).is_ok();
        if removed {
            upstream(why)
        } else {
            CommError::Upstream(format!(
                "{why}; a partial encrypted file may remain at {}",
                staging_path.display()
            ))
        }
    };

    // Belt and braces against a hostile umask or filesystem: it is our inode,
    // so tighten it before a single byte is written.
    let ok = rustix::fs::fchmod(&fd, Mode::RUSR | Mode::WUSR).is_ok()
        && rustix::fs::fstat(&fd)
            .is_ok_and(|st| st.st_uid == rustix::process::geteuid().as_raw() && st.st_nlink == 1);
    if !ok {
        return Err(discard("the new backup file is not private to this user"));
    }
    let mut file = File::from(fd);
    let written = if faults.fail_write {
        Err(std::io::Error::other("injected write failure"))
    } else {
        file.write_all(text.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
    };
    drop(file);
    if written.is_err() {
        return Err(discard("writing the backup file failed"));
    }

    // Publish without overwriting: link fails with EEXIST if anything is there.
    match rustix::fs::linkat(
        &d.parent,
        staging_name.as_os_str(),
        &d.parent,
        d.name.as_os_str(),
        AtFlags::empty(),
    ) {
        Ok(()) => {}
        Err(e) if e == rustix::io::Errno::EXIST => {
            let cleanup = discard("target_exists");
            return Err(match cleanup {
                CommError::Upstream(m) if m.contains("may remain") => CommError::Upstream(m),
                _ => target_exists(),
            });
        }
        Err(_) => return Err(discard("publishing the backup file failed")),
    }
    // Published and complete; the staging name is now just a second link.
    if faults.fail_cleanup
        || rustix::fs::unlinkat(&d.parent, staging_name.as_os_str(), AtFlags::empty()).is_err()
    {
        tracing::warn!(
            path = %staging_path.display(),
            "backup published but its staging link could not be removed"
        );
    }
    Ok(())
}

/// Shared by this module's tests and the router's.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod fixtures {
    pub(crate) const SK_HEX: &str =
        "3501454135014541350145413501453fefb02227e449e57cf4d3a3ce05378683";
    pub(crate) fn sk() -> [u8; 32] {
        hex::decode(SK_HEX).unwrap().try_into().unwrap()
    }
    pub(crate) fn nsec() -> String {
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), &sk()).unwrap()
    }
    pub(crate) fn npub() -> String {
        let pk = super::pubkey_of(&sk()).unwrap();
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("npub").unwrap(), &pk).unwrap()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use rustix::io::FdFlags;

    use super::*;

    /// NIP-49's published test vector.
    const SPEC_NCRYPTSEC: &str = "ncryptsec1qgg9947rlpvqu76pj5ecreduf9jxhselq2nae2kghhvd5g7dgjtcxfqtd67p9m0w57lspw8gsq6yphnm8623nsl8xn9j4jdzz84zm3frztj3z7s35vpzmqf6ksu8r89qk5z2zxfmu5gv8th8wclt0h4p";
    const SPEC_HEX: &str = "3501454135014541350145413501453fefb02227e449e57cf4d3a3ce05378683";

    fn encode_nsec(key: &[u8; 32]) -> String {
        bech32::encode::<Bech32>(Hrp::parse("nsec").unwrap(), key).unwrap()
    }
    fn encode_npub(key: &[u8; 32]) -> String {
        bech32::encode::<Bech32>(Hrp::parse("npub").unwrap(), &pubkey_of(key).unwrap()).unwrap()
    }

    #[test]
    fn decrypts_the_official_nip49_vector() {
        let key = decrypt_nip49(SPEC_NCRYPTSEC, "nostr").unwrap();
        assert_eq!(hex::encode(*key), SPEC_HEX);
        assert!(matches!(
            decrypt_nip49(SPEC_NCRYPTSEC, "wrong"),
            Err(CommError::Locked(_))
        ));
    }

    #[test]
    fn nfkc_normalises_the_password_like_the_spec_requires() {
        // U+FB01 (fi ligature) normalises to "fi" under NFKC.
        let a = encrypt_nip49(&[7u8; 32], "ﬁrst-password", 4).unwrap();
        assert_eq!(&*decrypt_nip49(&a, "first-password").unwrap(), &[7u8; 32]);
    }

    #[test]
    fn encrypted_backup_round_trips_to_the_same_identity() {
        let sk: [u8; 32] = hex::decode(SPEC_HEX).unwrap().try_into().unwrap();
        let text = encrypt_nip49(&sk, "correct horse", 8).unwrap();
        assert!(text.starts_with("ncryptsec1"));
        assert!(!text.contains(&encode_nsec(&sk)));
        let back = decrypt_nip49(&text, "correct horse").unwrap();
        assert_eq!(encode_npub(&back), encode_npub(&sk));
        // fresh salt + nonce every time
        assert_ne!(text, encrypt_nip49(&sk, "correct horse", 8).unwrap());
    }

    fn transcript(nsec: &str, npub: &str) -> Vec<u8> {
        format!("Keystore password: \r\nIdentity: t1\r\nNpub:     {npub}\r\nNsec:     {nsec}\r\n")
            .into_bytes()
    }

    #[test]
    fn parser_accepts_only_a_valid_key_for_the_expected_identity() {
        let sk: [u8; 32] = hex::decode(SPEC_HEX).unwrap().try_into().unwrap();
        let pk = pubkey_of(&sk).unwrap();
        let ok = transcript(&encode_nsec(&sk), &encode_npub(&sk));
        assert_eq!(&*parse_export_output(&ok, &pk).unwrap(), &sk);

        // wrong identity
        let other = [9u8; 32];
        assert!(parse_export_output(&ok, &pubkey_of(&other).unwrap()).is_err());
        // corrupted checksum
        let mut bad = encode_nsec(&sk);
        bad.replace_range(bad.len() - 1.., if bad.ends_with('q') { "p" } else { "q" });
        assert!(parse_export_output(&transcript(&bad, &encode_npub(&sk)), &pk).is_err());
        // right shape, but a key that is not this identity's
        let forged = transcript(&encode_nsec(&other), &encode_npub(&sk));
        assert!(parse_export_output(&forged, &pk).is_err());
        // duplicated fields (what a newline in a nickname used to forge)
        let mut dup = transcript(&encode_nsec(&other), &encode_npub(&sk));
        dup.extend_from_slice(format!("Nsec:     {}\r\n", encode_nsec(&sk)).as_bytes());
        assert!(parse_export_output(&dup, &pk).is_err());
        assert!(parse_export_output(b"Nsec: not-a-key\r\n", &pk).is_err());
    }

    #[test]
    fn cancelled_reader_never_touches_the_password() {
        // The master end is a plain pty nobody writes to: if `drive` did not
        // check `cancel` first it would block forever in read().
        let (master, _slave) = open_pty().unwrap();
        let cancel = AtomicBool::new(true);
        let password = Password::new(b"keychain-password".to_vec()).unwrap();
        let started = Instant::now();
        let result = drive(master, &password, &cancel, Duration::from_secs(1));
        assert!(matches!(result, Err(CommError::Timeout)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn the_pty_master_is_close_on_exec_so_no_child_can_inherit_it() {
        let (master, slave) = open_pty().unwrap();
        for fd in [&master, &slave] {
            assert!(
                rustix::io::fcntl_getfd(fd)
                    .unwrap()
                    .contains(FdFlags::CLOEXEC)
            );
        }
    }

    #[test]
    fn typable_keychain_passwords_only() {
        // what create_identity generates
        let generated = Password::generate([5u8; 32]);
        assert!(check_typable(&generated).is_ok());
        for bad in [
            &b"has\ttab-and-more"[..],
            b"new\nline",
            b"nul\0byte",
            b"del\x7fchar",
        ] {
            assert!(check_typable(&Password::new(bad.to_vec()).unwrap()).is_err());
        }
        assert!(check_typable(&Password::new(vec![b'a'; 1000]).unwrap()).is_err());
        assert!(check_typable(&Password::new(vec![b'a'; 999]).unwrap()).is_ok());
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
        // 1024 bytes is measured in UTF-8, not characters
        assert!(validate_backup_password(&"é".repeat(513)).is_err());
    }

    #[test]
    fn nfkc_allocates_exactly_for_expanding_characters() {
        // "Ⅷ" (U+2167, 3 bytes) normalises to "VIII" (4 bytes): 24 -> 32 bytes.
        let input = "Ⅷ".repeat(8);
        assert_eq!(input.len(), 24);
        let out = nfkc(&input);
        assert_eq!(out.as_str(), "VIII".repeat(8));
        assert_eq!(out.len(), 32);
        assert_eq!(out.capacity(), 32, "the buffer must never have been grown");
        let enc = encrypt_nip49(&[3u8; 32], &input, 4).unwrap();
        assert_eq!(
            &*decrypt_nip49(&enc, &"VIII".repeat(8)).unwrap(),
            &[3u8; 32]
        );
    }

    #[test]
    fn the_pty_is_allocated_close_on_exec_without_the_non_atomic_openpt() {
        let (master, slave) = open_pty().unwrap();
        for fd in [&master, &slave] {
            assert!(
                rustix::io::fcntl_getfd(fd)
                    .unwrap()
                    .contains(FdFlags::CLOEXEC)
            );
        }
        // Structural guard: production code must not call `openpt` (open then
        // fcntl leaves a window in which a fork anywhere in the process
        // inherits the master).
        let source = include_str!("export.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert!(
            !production.contains(concat!("open", "pt(")),
            "use open_pty()"
        );
        assert!(production.contains("/dev/ptmx"));
    }

    #[test]
    fn publishing_leaves_no_staging_file_and_never_exposes_a_partial_target() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        write_backup_file(&out.join("a.ncryptsec"), &home, "ncryptsec1abc").unwrap();
        let names: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("a.ncryptsec")]);
        // target_exists also leaves nothing behind
        assert!(matches!(
            write_backup_file(&out.join("a.ncryptsec"), &home, "zzz"),
            Err(CommError::Conflict(..))
        ));
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    }

    #[test]
    fn a_failed_write_is_cleaned_up_or_reports_the_remaining_partial_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let dest = out.join("b.ncryptsec");

        let err = write_backup_file_with(
            &dest,
            &home,
            "x",
            &Faults {
                fail_write: true,
                fail_cleanup: false,
            },
        )
        .unwrap_err();
        assert!(
            matches!(&err, CommError::Upstream(m) if !m.contains("may remain")),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_dir(&out).unwrap().count(),
            0,
            "staging removed, target never created"
        );

        let err = write_backup_file_with(
            &dest,
            &home,
            "x",
            &Faults {
                fail_write: true,
                fail_cleanup: true,
            },
        )
        .unwrap_err();
        let leftovers: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(leftovers.len(), 1);
        assert!(!dest.exists());
        let CommError::Upstream(message) = err else {
            panic!("expected upstream")
        };
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
    fn creates_new_0600_files_and_refuses_everything_that_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".hyphae")).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let dest = out.join("k.ncryptsec");
        write_backup_file(&dest, &home, "ncryptsec1abc").unwrap();
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "ncryptsec1abc\n");

        // second attempt: target_exists, content untouched
        let err = write_backup_file(&dest, &home, "ncryptsec1zzz").unwrap_err();
        assert!(
            matches!(&err, CommError::Conflict(m, Some(d)) if m.contains("target_exists") && d["reason"] == "target_exists")
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "ncryptsec1abc\n");
        assert!(precheck_destination(&dest, &home).is_err());

        // a pre-created, attacker-writable file is never reused
        let planted = out.join("planted");
        std::fs::write(&planted, "mine").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(write_backup_file(&planted, &home, "ncryptsec1abc").is_err());
        assert_eq!(std::fs::read_to_string(&planted).unwrap(), "mine");
    }

    #[test]
    fn refuses_symlinks_hard_links_relative_paths_and_the_hyphae_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".hyphae")).unwrap();
        let keystore = home.join(".hyphae").join("keystore.json");
        std::fs::write(&keystore, "keep").unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();

        // dangling and live symlinks
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), out.join("dangling")).unwrap();
        std::os::unix::fs::symlink(&keystore, out.join("live")).unwrap();
        assert!(write_backup_file(&out.join("dangling"), &home, "x").is_err());
        assert!(write_backup_file(&out.join("live"), &home, "x").is_err());
        // a hard link to the keystore sitting outside the data dir
        std::fs::hard_link(&keystore, out.join("hard")).unwrap();
        assert!(write_backup_file(&out.join("hard"), &home, "x").is_err());
        assert_eq!(std::fs::read_to_string(&keystore).unwrap(), "keep");
        assert_eq!(std::fs::metadata(&keystore).unwrap().nlink(), 2);

        // inside the Hyphae home, also through a symlinked parent
        assert!(write_backup_file(&home.join(".hyphae").join("new"), &home, "x").is_err());
        assert!(write_backup_file(&home.join("new"), &home, "x").is_err());
        let via = tmp.path().join("via");
        std::os::unix::fs::symlink(home.join(".hyphae"), &via).unwrap();
        assert!(write_backup_file(&via.join("new"), &home, "x").is_err());
        assert!(!home.join(".hyphae").join("new").exists());

        assert!(write_backup_file(Path::new("relative.txt"), &home, "x").is_err());
        // fail closed: a Hyphae home that cannot be identified refuses the write
        assert!(
            write_backup_file(&out.join("fresh"), &tmp.path().join("no-such-home"), "x").is_err()
        );
        assert!(!out.join("fresh").exists());
        assert!(write_backup_file(&out.join("a").join("b"), &home, "x").is_err());
    }
}
