//! File-descriptor checks shared by the download and installation boundaries.
//! Managed layout (ADR-038 D1): `<data>/components/<id>/<version>/`, with the
//! per-component root (`components/<id>`) owning the operation lock and cache.

use rustix::fs::{Mode, OFlags, open};
use std::{
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
};

pub fn open_regular(path: &Path) -> io::Result<File> {
    // NONBLOCK also prevents a substituted FIFO from blocking before fstat.
    let file = File::from(open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file() || !owned(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "archive must be an owned regular file",
        ));
    }
    Ok(file)
}

fn owned(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.uid() == rustix::process::geteuid().as_raw() && metadata.nlink() == 1
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

pub fn private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    let directory = File::from(open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )?);
    let metadata = directory.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "managed directory must be owned and mode 0700",
            ));
        }
    }
    Ok(())
}

/// Validate and create a cache path inside a managed component root, i.e. below
/// `<...>/components/<id>/`. Returns the canonical component root, which owns
/// the operation lock. Every directory from the component root down is opened
/// without following links and must be owned by us with mode 0700.
pub(crate) fn prepare_cache(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache must be an absolute managed path",
        ));
    }
    let root = path
        .ancestors()
        .find(|p| {
            p.parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "components")
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache must be inside a components/<id> tree",
            )
        })?;
    // Only the external parent may contain platform aliases such as /var.
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }
    private_directory(root)?;
    let mut current = root.to_path_buf();
    for component in path
        .strip_prefix(root)
        .map_err(io::Error::other)?
        .components()
    {
        current.push(component);
        private_directory(&current)?;
    }
    fs::canonicalize(root)
}
