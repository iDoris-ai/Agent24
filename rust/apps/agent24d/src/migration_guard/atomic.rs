//! Durable atomic file replace (design D3, section 8 R1).
//!
//! Sequence: exclusive 0600 temp file in the target directory, write, fsync,
//! rename, then fsync the parent directory and every ancestor up to and
//! including `data_dir`. Any failure leaves the target untouched (or absent)
//! and removes the temp file. File operations go through [`FileOps`] so tests
//! can record the call order and inject failures.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub trait FileOps {
    /// Create `path` with `O_EXCL`, mode 0600, write `data`, fsync the file.
    fn write_new_synced(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn fsync_dir(&self, dir: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RealOps;

impl FileOps for RealOps {
    fn write_new_synced(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(data)?;
        f.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn fsync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(dir)?.sync_all()
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Replace `target` with `data` durably. `target` must be under `data_dir`.
pub fn atomic_write(
    ops: &dyn FileOps,
    data_dir: &Path,
    target: &Path,
    data: &[u8],
) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| invalid("target has no parent"))?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| invalid("target has no utf-8 file name"))?;
    if !parent.starts_with(data_dir) {
        return Err(invalid("target is outside data_dir"));
    }
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp: PathBuf = parent.join(format!(".{name}.tmp.{}.{seq}", std::process::id()));
    if let Err(err) = ops.write_new_synced(&tmp, data) {
        let _ = ops.remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = ops.rename(&tmp, target) {
        let _ = ops.remove_file(&tmp);
        return Err(err);
    }
    fsync_chain(ops, data_dir, parent)
}

/// fsync `start` and each ancestor up to and including `data_dir`.
pub fn fsync_chain(ops: &dyn FileOps, data_dir: &Path, start: &Path) -> io::Result<()> {
    let mut dir = start;
    loop {
        ops.fsync_dir(dir)?;
        if dir == data_dir {
            return Ok(());
        }
        dir = dir
            .parent()
            .filter(|p| p.starts_with(data_dir))
            .ok_or_else(|| invalid("directory chain left data_dir"))?;
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

#[cfg(test)]
pub(crate) mod test_ops {
    use super::*;
    use std::cell::RefCell;

    /// Records calls; fails the call whose name equals `fail_on`.
    #[derive(Default)]
    pub struct RecordingOps {
        pub calls: RefCell<Vec<String>>,
        pub fail_on: Option<&'static str>,
    }

    impl RecordingOps {
        pub fn failing(op: &'static str) -> Self {
            Self {
                calls: RefCell::default(),
                fail_on: Some(op),
            }
        }
        fn hit(&self, op: &'static str, arg: &Path) -> io::Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("{op}:{}", arg.display()));
            if self.fail_on == Some(op) {
                return Err(io::Error::other("injected"));
            }
            Ok(())
        }
    }

    impl FileOps for RecordingOps {
        fn write_new_synced(&self, path: &Path, data: &[u8]) -> io::Result<()> {
            RealOps.write_new_synced(path, data)?;
            self.hit("write", path)
        }
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.hit("rename", from)?;
            RealOps.rename(from, to)
        }
        fn fsync_dir(&self, dir: &Path) -> io::Result<()> {
            self.hit("fsync_dir", dir)
        }
        fn remove_file(&self, path: &Path) -> io::Result<()> {
            let _ = RealOps.remove_file(path);
            self.hit("remove", path)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::test_ops::RecordingOps;
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn writes_0600_and_fsyncs_every_level_after_rename() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let deep = root.join("a/b");
        fs::create_dir_all(&deep).unwrap();
        let ops = RecordingOps::default();
        atomic_write(&ops, root, &deep.join("f.json"), b"x").unwrap();
        assert_eq!(fs::read(deep.join("f.json")).unwrap(), b"x");
        let mode = fs::metadata(deep.join("f.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let calls = ops.calls.borrow();
        let kinds: Vec<&str> = calls.iter().map(|c| c.split(':').next().unwrap()).collect();
        assert_eq!(
            kinds,
            ["write", "rename", "fsync_dir", "fsync_dir", "fsync_dir"]
        );
        assert!(calls[2].ends_with("a/b") && calls[3].ends_with("/a"));
        assert_eq!(calls[4], format!("fsync_dir:{}", root.display()));
    }

    #[test]
    fn failure_before_rename_leaves_no_target_and_no_temp() {
        for op in ["write", "rename"] {
            let td = tempfile::tempdir().unwrap();
            let target = td.path().join("g.json");
            fs::write(&target, b"old").unwrap();
            let ops = RecordingOps::failing(op);
            assert!(atomic_write(&ops, td.path(), &target, b"new").is_err());
            assert_eq!(fs::read(&target).unwrap(), b"old", "{op}");
            assert_eq!(leftovers(td.path()), ["g.json"], "{op}");
        }
    }

    #[test]
    fn parent_fsync_failure_is_an_error() {
        let td = tempfile::tempdir().unwrap();
        let ops = RecordingOps::failing("fsync_dir");
        assert!(atomic_write(&ops, td.path(), &td.path().join("g.json"), b"n").is_err());
    }

    #[test]
    fn rejects_target_outside_data_dir() {
        let td = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let err = atomic_write(&RealOps, td.path(), &other.path().join("x"), b"n").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
