//! Bounded, rotating log of supervisor lifecycle events.

use super::identity::{private_dir, private_open};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};
use tokio::{sync::mpsc, task::JoinHandle};

const LOG_BYTES: u64 = 10 * 1024 * 1024;
const LOG_FILES: usize = 5;

pub(super) struct Output {
    sender: Option<mpsc::Sender<String>>,
    writer: Option<JoinHandle<io::Result<()>>>,
}
impl Output {
    pub fn new(directory: &Path) -> io::Result<Self> {
        let mut log = RotatingLog::new(directory, LOG_BYTES, LOG_FILES)?;
        let (sender, mut receiver) = mpsc::channel::<String>(64);
        let writer = tokio::task::spawn_blocking(move || {
            while let Some(line) = receiver.blocking_recv() {
                log.write(line.as_bytes())?;
            }
            Ok(())
        });
        let output = Self {
            sender: Some(sender),
            writer: Some(writer),
        };
        output.event("starting");
        Ok(output)
    }
    pub fn event(&self, phase: &str) {
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(format!("idoris: {phase}\n"));
        }
    }
    pub async fn finish(&mut self) {
        self.event("stopped");
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.await;
        }
    }
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
    fn logs_rotate_five_private_files() {
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
