//! `POST /comm/identity/export`: write an identity's private key (`nsec`) to
//! a user-chosen file, without the key ever reaching a renderer, a log, an
//! HTTP response or a temp file.
//!
//! Hyphae v0.27.2's `identity export` has no `--password-stdin`: it only
//! reads the keystore password from an interactive terminal and refuses in
//! JSON mode. So this module runs it on a private pseudo-terminal, answers
//! the password prompt from the keychain-held password, reads the key out of
//! the pty buffer (zeroized on drop) and writes it straight to the
//! destination with mode 0600. When Hyphae gains `--password-stdin` for
//! export, [`export_nsec`] is the only function that has to change.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use zeroize::Zeroizing;

use crate::error::CommError;
use crate::password::Password;
use crate::runner::{HyphaeRunner, kill_process_group_best_effort};

const PROMPT: &[u8] = b"assword:";
const READ_CAP: usize = 64 * 1024;
const EXPORT_TIMEOUT: Duration = Duration::from_secs(30);
/// Hyphae switches the tty to no-echo right after printing the prompt; a short
/// pause keeps the password from being echoed into the (discarded) pty buffer.
const PROMPT_SETTLE: Duration = Duration::from_millis(150);

pub(crate) struct ExportedKey {
    pub npub: String,
    pub nsec: Zeroizing<String>,
}

fn upstream(msg: &str) -> CommError {
    CommError::Upstream(msg.to_owned())
}

/// Runs `hyphae identity export --nickname <nickname>` on a pty.
pub(crate) async fn export_nsec(
    runner: &HyphaeRunner,
    nickname: &str,
    password: Password,
) -> Result<ExportedKey, CommError> {
    let mut command = runner.interactive_command(&["identity", "export", "--nickname", nickname]);
    let pid = Arc::new(AtomicU32::new(0));
    let pid_for_task = Arc::clone(&pid);
    let task =
        tokio::task::spawn_blocking(move || run_on_pty(&mut command, &password, &pid_for_task));
    match tokio::time::timeout(EXPORT_TIMEOUT, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(_join)) => Err(upstream("identity export task failed")),
        Err(_elapsed) => {
            // Killing the group closes the slave side, which unblocks the
            // reader thread so it can drop the password and buffers.
            let raw = pid.load(Ordering::SeqCst);
            if raw != 0 {
                kill_process_group_best_effort(raw);
            }
            Err(CommError::Timeout)
        }
    }
}

fn run_on_pty(
    command: &mut std::process::Command,
    password: &Password,
    pid_slot: &AtomicU32,
) -> Result<ExportedKey, CommError> {
    let io_err = |_| upstream("could not allocate a terminal for identity export");
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).map_err(io_err)?;
    grantpt(&master).map_err(io_err)?;
    unlockpt(&master).map_err(io_err)?;
    let slave_name = ptsname(&master, Vec::new()).map_err(io_err)?;
    let slave = rustix::fs::open(
        slave_name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY,
        Mode::empty(),
    )
    .map_err(io_err)?;

    let mut child = {
        let stdin = slave
            .try_clone()
            .map_err(|_| upstream("pty setup failed"))?;
        let stdout = slave
            .try_clone()
            .map_err(|_| upstream("pty setup failed"))?;
        command.stdin(stdin).stdout(stdout).stderr(slave);
        let spawned = command
            .spawn()
            .map_err(|e| CommError::Upstream(format!("failed to spawn hyphae: {e}")));
        // Drop the parent's copies of the slave side so the master sees EOF
        // when the child exits.
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        spawned?
    };
    pid_slot.store(child.id(), Ordering::SeqCst);

    let mut master = File::from(master);
    let mut out: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::new());
    let mut chunk = [0u8; 1024];

    // Phase 1: wait for the prompt, then answer it.
    let mut answered = false;
    loop {
        match master.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.len() > READ_CAP {
                    break;
                }
                if !answered && out.windows(PROMPT.len()).any(|w| w == PROMPT) {
                    std::thread::sleep(PROMPT_SETTLE);
                    let mut line = Zeroizing::new(password.as_bytes().to_vec());
                    line.push(b'\n');
                    if master.write_all(&line).is_err() {
                        break;
                    }
                    answered = true;
                }
            }
        }
    }
    let status = child
        .wait()
        .map_err(|_| upstream("identity export wait failed"))?;
    if !answered {
        return Err(upstream(
            "identity export did not ask for the keystore password",
        ));
    }
    if !status.success() {
        // The failure text can only be an error message (the key is printed on
        // success only), but it is still not forwarded: callers get a fixed
        // classification.
        let text = String::from_utf8_lossy(&out).to_ascii_lowercase();
        return Err(if text.contains("password") || text.contains("keystore") {
            CommError::Locked("password verification failed".to_owned())
        } else if text.contains("not found") || text.contains("no identity") {
            CommError::NotFound("identity not found".to_owned())
        } else {
            CommError::Upstream(format!(
                "hyphae identity export failed (exit {})",
                status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |c| c.to_string())
            ))
        });
    }
    parse_export_output(&out)
}

fn token_after<'a>(text: &'a str, label: &str) -> Option<&'a str> {
    text.lines()
        .map(|l| l.trim())
        .find_map(|l| l.strip_prefix(label))
        .map(str::trim)
        .and_then(|rest| rest.split_whitespace().next())
}

fn parse_export_output(raw: &[u8]) -> Result<ExportedKey, CommError> {
    let text = Zeroizing::new(String::from_utf8_lossy(raw).replace('\r', ""));
    let nsec = token_after(&text, "Nsec:").filter(|t| {
        t.starts_with("nsec1")
            && t.len() >= 20
            && t.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    });
    let npub = token_after(&text, "Npub:").filter(|t| crate::npub::is_valid_npub(t));
    match (nsec, npub) {
        (Some(nsec), Some(npub)) => Ok(ExportedKey {
            npub: npub.to_owned(),
            nsec: Zeroizing::new(nsec.to_owned()),
        }),
        _ => Err(upstream("identity export output was not recognised")),
    }
}

/// Writes `nsec` + newline to `dest` with mode 0600, never following a
/// symlink, never through a temp file. `dest` must be absolute, its parent an
/// existing directory outside `hyphae_home` (so the keystore cannot be
/// overwritten).
pub(crate) fn write_secret_file(
    dest: &Path,
    hyphae_home: &Path,
    nsec: &str,
) -> Result<(), CommError> {
    if !dest.is_absolute() || dest.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(CommError::Invalid("path must be absolute".to_owned()));
    }
    let name = dest
        .file_name()
        .ok_or_else(|| CommError::Invalid("path must name a file".to_owned()))?;
    let parent = dest
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .filter(|p| p.is_dir())
        .ok_or_else(|| CommError::Invalid("the destination folder does not exist".to_owned()))?;
    let home = hyphae_home
        .canonicalize()
        .unwrap_or_else(|_| hyphae_home.to_path_buf());
    if parent.starts_with(&home) {
        return Err(CommError::Invalid(
            "the destination must be outside the Hyphae data directory".to_owned(),
        ));
    }
    let target = parent.join(name);

    let (mut file, created) = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(&target)
    {
        Ok(f) => (f, true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let f = OpenOptions::new()
                .write(true)
                .custom_flags(OFlags::NOFOLLOW.bits() as i32)
                .open(&target)
                .map_err(|_| CommError::Invalid("cannot overwrite the destination".to_owned()))?;
            let meta = f
                .metadata()
                .map_err(|_| upstream("cannot stat destination"))?;
            if !meta.is_file() {
                return Err(CommError::Invalid(
                    "the destination is not a regular file".to_owned(),
                ));
            }
            // Tighten before any secret byte is written, then truncate.
            f.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| upstream("cannot restrict destination permissions"))?;
            f.set_len(0)
                .map_err(|_| upstream("cannot truncate destination"))?;
            (f, false)
        }
        Err(_) => {
            return Err(CommError::Invalid(
                "cannot create the destination file".to_owned(),
            ));
        }
    };
    let mut line = Zeroizing::new(String::with_capacity(nsec.len() + 1));
    line.push_str(nsec);
    line.push('\n');
    let result = file
        .write_all(line.as_bytes())
        .and_then(|()| file.sync_all());
    if result.is_err() {
        drop(file);
        if created {
            let _ = std::fs::remove_file(&target);
        }
        return Err(upstream("writing the backup file failed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const NSEC: &str = "nsec1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";

    #[test]
    fn writes_0600_and_refuses_the_hyphae_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".hyphae")).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let dest = out.join("k.txt");
        write_secret_file(&dest, &home, NSEC).unwrap();
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), format!("{NSEC}\n"));
        assert!(
            write_secret_file(&home.join(".hyphae").join("keystore.json"), &home, NSEC).is_err()
        );
        assert!(write_secret_file(Path::new("relative.txt"), &home, NSEC).is_err());
    }

    #[test]
    fn overwrite_tightens_permissions_and_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("k.txt");
        std::fs::write(&dest, "old content that is longer than the new one........").unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_secret_file(&dest, &tmp.path().join("elsewhere"), NSEC).unwrap();
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), format!("{NSEC}\n"));
    }

    #[test]
    fn refuses_symlink_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("victim");
        std::fs::write(&target, "keep").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(write_secret_file(&link, &tmp.path().join("elsewhere"), NSEC).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }

    #[test]
    fn parses_hyphae_output_and_rejects_garbage() {
        let npub = "npub194veg99mv8vxrw9xe3c2t8kegm3f5j0df8e238mmwhtx2y3q4xmsgpk4jv";
        let raw = format!(
            "Keystore password: \r\nIdentity: t1\r\nNpub:     {npub}\r\nNsec:     {NSEC}\r\n"
        );
        let key = parse_export_output(raw.as_bytes()).unwrap();
        assert_eq!(key.npub, npub);
        assert_eq!(key.nsec.as_str(), NSEC);
        assert!(parse_export_output(b"Nsec: not-a-key\r\n").is_err());
    }
}
