//! An upload's bytes on disk (ADR-DOC-02 §5.6): `uploads/<upload_id>/data`
//! under the data directory, outside `blobs/tmp/`, so neither the startup
//! clean-up nor blob GC touches it.
//!
//! Each write starts by cutting the file back to its offset: bytes a crash
//! left past `received` (written, never counted) are overwritten by the next
//! append rather than kept.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// `<data_dir>/uploads/<upload_id>`.
#[must_use]
pub fn upload_dir(data_dir: &Path, upload_id: &str) -> PathBuf {
    data_dir.join("uploads").join(upload_id)
}

#[derive(Debug)]
pub enum WriteError {
    Io(io::Error),
    /// The file holds fewer bytes than the rows count: data was lost.
    Short {
        len: u64,
    },
}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self {
        WriteError::Io(e)
    }
}

/// Writes `bytes` at `offset` of `<dir>/data`, after cutting the file back
/// to `offset`, and fsyncs the file and the directories above it. The
/// directories are synced on every write, not only when created: a first
/// attempt can create them and fail before its sync, and the row a retry
/// commits must not point at a file a power loss could still take away.
///
/// The caller must pass the upload's committed `received` as `offset`: the
/// cut discards everything past it, so a smaller offset would destroy
/// counted bytes, and a replay must not call this at all. It takes no lock;
/// the caller holds one turn per upload across the write and the commit
/// that counts it (`chunks.rs`, #827 review).
pub fn write_at(dir: &Path, offset: u64, bytes: &[u8]) -> Result<(), WriteError> {
    #[cfg(test)]
    hooks::in_writer(dir);
    let path: PathBuf = dir.join("data");
    std::fs::create_dir_all(dir)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    let len = file.metadata()?.len();
    if len < offset {
        return Err(WriteError::Short { len });
    }
    file.set_len(offset)?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    for d in [Some(dir), dir.parent(), dir.parent().and_then(Path::parent)]
        .into_iter()
        .flatten()
    {
        #[cfg(test)]
        hooks::before_dir_sync(d)?;
        File::open(d)?.sync_all()?;
    }
    Ok(())
}

/// Test hooks into [`write_at`]: stop a writer on entry, fail a directory
/// sync once, record directory syncs.
#[cfg(test)]
pub(crate) mod hooks {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::io;
    use std::path::Path;
    use std::sync::{Mutex, mpsc};

    /// A writer for the upload `id` stops on entry until released.
    pub(crate) struct Gate {
        pub id: String,
        pub entered: mpsc::Sender<()>,
        pub release: mpsc::Receiver<()>,
    }

    pub(crate) static GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());
    /// Upload ids whose next directory sync fails, once.
    pub(crate) static FAIL_DIR_SYNC: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// Every directory synced, by path.
    pub(crate) static DIR_SYNCS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    pub(super) fn in_writer(dir: &Path) {
        let id = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let gate = {
            let mut gates = GATES.lock().unwrap();
            let at = gates.iter().position(|g| g.id == id);
            at.map(|i| gates.remove(i))
        };
        if let Some(gate) = gate {
            gate.entered.send(()).unwrap();
            gate.release.recv().unwrap();
        }
    }

    pub(super) fn before_dir_sync(dir: &Path) -> io::Result<()> {
        DIR_SYNCS.lock().unwrap().push(dir.display().to_string());
        let id = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let mut fail = FAIL_DIR_SYNC.lock().unwrap();
        match fail.iter().position(|f| f == id) {
            Some(i) => {
                fail.remove(i);
                Err(io::Error::other("injected directory sync failure"))
            }
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
