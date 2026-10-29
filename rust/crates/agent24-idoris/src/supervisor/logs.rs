//! Bounded pipe drains and a bounded, lossy queue to a blocking disk writer.
//! Default output is controlled lifecycle events only. Diagnostic output is
//! opt-in, sanitized before entering the queue, and never used for readiness.

use super::identity::{private_dir, private_open};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{ChildStderr, ChildStdout},
    sync::mpsc,
    task::JoinHandle,
};
use zeroize::Zeroizing;

const MAX_LINE: usize = 8192;
const LOG_BYTES: u64 = 10 * 1024 * 1024;
const LOG_FILES: usize = 5;

struct Secrets {
    admin: Arc<Zeroizing<String>>,
    omlx: Option<Arc<Zeroizing<String>>>,
}
impl Secrets {
    fn redact(&self, text: &str) -> String {
        let mut value = text.replace(self.admin.as_str(), "[REDACTED]");
        if let Some(key) = &self.omlx {
            value = value.replace(key.as_str(), "[REDACTED]");
        }
        while let Some(start) = value.find("idk_") {
            let end = value[start..]
                .find(char::is_whitespace)
                .map_or(value.len(), |offset| start + offset);
            value.replace_range(start..end, "[REDACTED]");
        }
        value
    }
}

pub(super) struct Output {
    secrets: Arc<Secrets>,
    sender: Option<mpsc::Sender<String>>,
    readers: Vec<JoinHandle<()>>,
    writer: Option<JoinHandle<io::Result<()>>>,
    diagnostics: bool,
}
impl Output {
    pub fn new(
        directory: &Path,
        admin: Arc<Zeroizing<String>>,
        omlx: Option<Arc<Zeroizing<String>>>,
        diagnostics: bool,
    ) -> io::Result<Self> {
        let mut log = RotatingLog::new(directory, LOG_BYTES, LOG_FILES)?;
        let (sender, mut receiver) = mpsc::channel::<String>(64);
        let writer = tokio::task::spawn_blocking(move || {
            while let Some(line) = receiver.blocking_recv() {
                log.write(line.as_bytes())?;
            }
            Ok(())
        });
        let output = Self {
            secrets: Arc::new(Secrets { admin, omlx }),
            sender: Some(sender),
            readers: Vec::new(),
            writer: Some(writer),
            diagnostics,
        };
        output.event("starting");
        Ok(output)
    }
    pub fn omlx_key(&self) -> Option<&str> {
        self.secrets.omlx.as_ref().map(|key| key.as_str())
    }
    pub fn event(&self, phase: &str) {
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(format!("idoris: {phase}\n"));
        }
    }
    pub fn attach(&mut self, stdout: Option<ChildStdout>, stderr: Option<ChildStderr>) {
        if let Some(pipe) = stdout {
            self.reader(pipe);
        }
        if let Some(pipe) = stderr {
            self.reader(pipe);
        }
    }
    fn reader(&mut self, pipe: impl AsyncRead + Unpin + Send + 'static) {
        if let Some(sender) = &self.sender {
            self.readers.push(tokio::spawn(drain(
                pipe,
                sender.clone(),
                self.secrets.clone(),
                self.diagnostics,
            )));
        }
    }
    pub async fn finish(&mut self) {
        for reader in self.readers.drain(..) {
            // A malicious descendant can retain a pipe; logging must not hold
            // process shutdown hostage. No buffered partial secret is emitted.
            let mut reader = reader;
            if tokio::time::timeout(std::time::Duration::from_secs(1), &mut reader)
                .await
                .is_err()
            {
                reader.abort();
                let _ = reader.await;
            }
        }
        self.event("stopped");
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.await;
        }
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}

async fn drain(
    mut pipe: impl AsyncRead + Unpin,
    sender: mpsc::Sender<String>,
    secrets: Arc<Secrets>,
    diagnostics: bool,
) {
    let mut chunk = Zeroizing::new([0_u8; 2048]);
    let mut line = Zeroizing::new(Vec::with_capacity(MAX_LINE));
    let mut overflow = false;
    loop {
        let count = match pipe.read(chunk.as_mut()).await {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        for &byte in &chunk[..count] {
            if byte == b'\n' {
                if diagnostics && !overflow {
                    let value = String::from_utf8_lossy(&line);
                    // Control bytes can conceal credentials across terminal
                    // escapes; discard such records instead of rendering them.
                    if !value.chars().any(|ch| ch.is_control() && ch != '\t') {
                        let _ = sender.try_send(format!("{}\n", secrets.redact(&value)));
                    }
                }
                line.clear();
                overflow = false;
            } else if line.len() < MAX_LINE && !overflow {
                line.push(byte);
            } else {
                line.clear();
                overflow = true;
            }
        }
    }
    // Partial final lines are discarded, never sliced across a secret.
}

struct RotatingLog {
    directory: PathBuf,
    file: File,
    size: u64,
    limit: u64,
    files: usize,
}
impl RotatingLog {
    fn new(directory: &Path, limit: u64, files: usize) -> io::Result<Self> {
        private_dir(directory)?;
        let file = private_open(&directory.join("idoris.log"))?;
        let size = file.metadata()?.len();
        // private_open is read/write, so seek explicitly for append semantics.
        use std::io::{Seek, SeekFrom};
        let mut file = file;
        file.seek(SeekFrom::End(0))?;
        Ok(Self {
            directory: directory.into(),
            file,
            size,
            limit,
            files,
        })
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.size + bytes.len() as u64 > self.limit {
            for number in (1..self.files).rev() {
                let from = if number == 1 {
                    self.directory.join("idoris.log")
                } else {
                    self.directory.join(format!("idoris.log.{}", number - 1))
                };
                let to = self.directory.join(format!("idoris.log.{number}"));
                match fs::rename(from, to) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            self.file = private_open(&self.directory.join("idoris.log"))?;
            self.file.set_len(0)?;
            self.size = 0;
        }
        self.file.write_all(bytes)?;
        self.size += bytes.len() as u64;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn logs_rotate_five_private_files_and_redact_all_secrets() {
        let secrets = Secrets {
            admin: Arc::new(Zeroizing::new("a".repeat(64))),
            omlx: Some(Arc::new(Zeroizing::new("upstream-secret".into()))),
        };
        let clean = secrets.redact(&format!(
            "{} upstream-secret idk_private-key end",
            "a".repeat(64)
        ));
        assert_eq!(clean, "[REDACTED] [REDACTED] [REDACTED] end");
        let directory = tempfile::tempdir().unwrap();
        let mut log = RotatingLog::new(directory.path(), 32, 5).unwrap();
        for _ in 0..20 {
            log.write(b"sanitized diagnostic output\n").unwrap();
        }
        let entries: Vec<_> = fs::read_dir(directory.path()).unwrap().collect();
        assert_eq!(entries.len(), 5);
        for entry in entries {
            let metadata = entry.unwrap().metadata().unwrap();
            assert!(metadata.len() <= 32);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }
}
