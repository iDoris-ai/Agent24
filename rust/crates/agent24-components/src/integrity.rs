//! Shared file-tree integrity for component installs and OS packages (S1-9).
//!
//! One path check, one [`FileDigest`], one tree walk. OS packages call this
//! module; they do not keep a second checker. The manifest format is the D2
//! map serialized as `files.json`. It is public so both sides share it. It is
//! not a stable external contract beyond this crate's callers.

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Payload cap shared with component installation (`install::MAX_UNPACKED_BYTES`).
pub const MAX_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;
/// Suggested bound for `files.json` (S1-9 §3).
pub const MAX_FILES_JSON_BYTES: u64 = 1024 * 1024;
/// Suggested bound for payload entries (S1-9 §3).
pub const MAX_FILE_ENTRIES: usize = 10_000;

pub const FILES_JSON_NAME: &str = "files.json";
pub const DOMAIN_OS_YML: &str = "domain-os.yml";

pub const DIR_MODE: u32 = 0o700;
pub const EXEC_MODE: u32 = 0o500;
pub const DATA_MODE: u32 = 0o400;

/// D2 per-file record: uncompressed size and lowercase SHA-256 hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    pub size: u64,
    pub sha256: String,
}

/// `integrity:` values that do not need the package bytes yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegritySection {
    pub manifest_file: String,
    pub sha256: String,
    pub files: Vec<String>,
    pub executables: Vec<String>,
}

#[derive(Debug)]
pub enum IntegrityError {
    /// CLI `verify` when the package has no integrity section.
    /// Discovery treats absence as the old package path and does not use this.
    Missing,
    HashMismatch {
        path: String,
    },
    ManifestInvalid {
        detail: String,
    },
    CheckFailed {
        detail: String,
    },
    NotNativeExecutable {
        path: String,
    },
}

impl fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("integrity_manifest_missing"),
            Self::HashMismatch { path } => write!(f, "hash_mismatch: {path}"),
            Self::ManifestInvalid { detail } => {
                write!(f, "integrity_manifest_invalid: {detail}")
            }
            Self::CheckFailed { detail } => write!(f, "integrity_check_failed: {detail}"),
            Self::NotNativeExecutable { path } => write!(f, "not_native_executable: {path}"),
        }
    }
}

impl std::error::Error for IntegrityError {}

/// Errors of the shared tree walk. Component install maps these onto
/// [`crate::InstallError`] without rewriting the rules.
#[derive(Debug)]
pub enum TreeError {
    Violation(String),
    HashMismatch(String),
    Io(io::Error),
    Cancelled,
}

impl From<io::Error> for TreeError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Safe relative path inside a package or component archive.
///
/// Rejects absolute paths, empty segments, `.`, `..`, backslashes and
/// control characters. This is the D2 `clean_path` / `clean_relative` check.
pub fn clean_path(name: &str, directory: bool) -> Result<&str, String> {
    let name = if directory {
        name.strip_suffix('/').unwrap_or(name)
    } else {
        name
    };
    if name.is_empty()
        || Path::new(name).is_absolute()
        || (name.as_bytes().get(1) == Some(&b':') && name.as_bytes()[0].is_ascii_alphabetic())
        || name.contains(['\\', '\0'])
        || name.chars().any(char::is_control)
        || name.split('/').any(|s| matches!(s, "" | "." | ".."))
    {
        return Err(format!("unsafe path: {name}"));
    }
    Ok(name)
}

pub fn is_safe_relative(path: &str) -> bool {
    clean_path(path, false).is_ok()
}

pub fn unix_file_mode(executables: &[String], name: &str) -> u32 {
    if executables.iter().any(|e| e == name) {
        EXEC_MODE
    } else {
        DATA_MODE
    }
}

pub fn set_unix_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

pub fn check_unix_mode(path: &Path, mode: u32) -> Result<(), TreeError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::symlink_metadata(path)?.permissions().mode() & 0o7777 != mode {
            return Err(TreeError::Violation(
                "installed permissions changed".to_owned(),
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

pub fn parent_dirs(files: &[String]) -> BTreeSet<String> {
    files
        .iter()
        .flat_map(|name| {
            Path::new(name)
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_string_lossy().into_owned())
        })
        .collect()
}

pub fn digest_reader(
    reader: &mut impl Read,
    cancel: &CancellationToken,
) -> Result<FileDigest, TreeError> {
    if cancel.is_cancelled() {
        return Err(TreeError::Cancelled);
    }
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(TreeError::Cancelled);
        }
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hash.update(&buffer[..n]);
    }
    Ok(FileDigest {
        size,
        sha256: format!("{:x}", hash.finalize()),
    })
}

/// Declaration rules that do not read the tree: fixed manifest name, digest
/// shape, no duplicates, executables are a subset, control files stay out of
/// the payload, and `spawn.command` is one of those executables.
pub fn validate_section(
    section: &IntegritySection,
    spawn_command: Option<&str>,
) -> Result<(), IntegrityError> {
    if section.manifest_file != FILES_JSON_NAME {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!("integrity.manifest must be {FILES_JSON_NAME}"),
        });
    }
    if !is_sha256_hex(&section.sha256) {
        return Err(IntegrityError::ManifestInvalid {
            detail: "integrity.sha256 must be 64 lowercase hex characters".to_owned(),
        });
    }
    let mut files = BTreeSet::new();
    for file in &section.files {
        clean_path(file, false).map_err(|detail| IntegrityError::ManifestInvalid { detail })?;
        if file == FILES_JSON_NAME || file == DOMAIN_OS_YML {
            return Err(IntegrityError::ManifestInvalid {
                detail: format!("control file listed as payload: {file}"),
            });
        }
        if !files.insert(file) {
            return Err(IntegrityError::ManifestInvalid {
                detail: format!("duplicate path: {file}"),
            });
        }
    }
    let mut executables = BTreeSet::new();
    for executable in &section.executables {
        if !files.contains(executable) || !executables.insert(executable) {
            return Err(IntegrityError::ManifestInvalid {
                detail: format!("executable is not a listed file: {executable}"),
            });
        }
    }
    // A bare name (no `/`) is resolved on PATH at launch (`agent24-os-proto` `launch::resolve`),
    // so a same-named file in the package would pass this check while a binary outside the
    // manifest runs. Only a package-relative path with a separator is bound to the listed bytes.
    if let Some(command) = spawn_command
        && !command.contains('/')
    {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!(
                "spawn.command {command} must be a package-relative path such as bin/{command}, not a PATH lookup"
            ),
        });
    }
    if let Some(command) = spawn_command
        && !executables
            .iter()
            .any(|executable| executable.as_str() == command)
    {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!("spawn.command {command} must be an integrity executable"),
        });
    }
    Ok(())
}

/// Canonical `files.json`: UTF-8 path order, two-space indent, trailing newline.
/// Verifiers hash these exact bytes. They do not re-serialize first.
pub fn render_files_json(
    manifest: &BTreeMap<String, FileDigest>,
) -> Result<Vec<u8>, IntegrityError> {
    let mut out = String::from("{\n");
    for (index, (path, digest)) in manifest.iter().enumerate() {
        if index > 0 {
            out.push_str(",\n");
        }
        let key = serde_json::to_string(path).map_err(|err| IntegrityError::ManifestInvalid {
            detail: err.to_string(),
        })?;
        let hash = serde_json::to_string(&digest.sha256).map_err(|err| {
            IntegrityError::ManifestInvalid {
                detail: err.to_string(),
            }
        })?;
        out.push_str(&format!(
            "  {key}: {{\"size\": {}, \"sha256\": {hash}}}",
            digest.size
        ));
    }
    out.push_str("\n}\n");
    Ok(out.into_bytes())
}

pub fn parse_files_json(bytes: &[u8]) -> Result<BTreeMap<String, FileDigest>, IntegrityError> {
    if bytes.len() as u64 > MAX_FILES_JSON_BYTES {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!(
                "files.json is {} bytes, over the {MAX_FILES_JSON_BYTES} byte limit",
                bytes.len()
            ),
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| IntegrityError::ManifestInvalid {
        detail: "files.json is not UTF-8".to_owned(),
    })?;
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let parsed = ParsedManifest::deserialize(&mut deserializer).map_err(|err| {
        IntegrityError::ManifestInvalid {
            detail: format!("files.json: {err}"),
        }
    })?;
    deserializer
        .end()
        .map_err(|err| IntegrityError::ManifestInvalid {
            detail: format!("files.json: {err}"),
        })?;
    if parsed.0.len() > MAX_FILE_ENTRIES {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!(
                "files.json has {} entries, over the {MAX_FILE_ENTRIES} entry limit",
                parsed.0.len()
            ),
        });
    }
    for (path, digest) in &parsed.0 {
        clean_path(path, false).map_err(|detail| IntegrityError::ManifestInvalid { detail })?;
        if path == FILES_JSON_NAME || path == DOMAIN_OS_YML {
            return Err(IntegrityError::ManifestInvalid {
                detail: format!("control file listed as payload: {path}"),
            });
        }
        if !is_sha256_hex(&digest.sha256) {
            return Err(IntegrityError::ManifestInvalid {
                detail: format!("illegal digest: {path}"),
            });
        }
    }
    Ok(parsed.0)
}

/// Hash the raw JSON, then parse those same bytes. A semantically identical
/// document with different bytes fails here.
pub fn verify_files_json(
    bytes: &[u8],
    section: &IntegritySection,
) -> Result<BTreeMap<String, FileDigest>, IntegrityError> {
    if bytes.len() as u64 > MAX_FILES_JSON_BYTES {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!(
                "files.json is {} bytes, over the {MAX_FILES_JSON_BYTES} byte limit",
                bytes.len()
            ),
        });
    }
    let actual = sha256_hex(bytes);
    if actual != section.sha256 {
        return Err(IntegrityError::HashMismatch {
            path: FILES_JSON_NAME.to_owned(),
        });
    }
    let parsed = parse_files_json(bytes)?;
    let keys: BTreeSet<&String> = parsed.keys().collect();
    let declared: BTreeSet<&String> = section.files.iter().collect();
    if keys != declared {
        return Err(IntegrityError::ManifestInvalid {
            detail: "integrity.files does not match files.json keys".to_owned(),
        });
    }
    let total = parsed
        .values()
        .try_fold(0u64, |sum, digest| sum.checked_add(digest.size));
    match total {
        Some(total) if total <= MAX_PAYLOAD_BYTES => Ok(parsed),
        Some(total) => Err(IntegrityError::ManifestInvalid {
            detail: format!("payload size {total} exceeds the {MAX_PAYLOAD_BYTES} byte limit"),
        }),
        None => Err(IntegrityError::ManifestInvalid {
            detail: "payload size overflow".to_owned(),
        }),
    }
}

struct ParsedManifest(BTreeMap<String, FileDigest>);

impl<'de> Deserialize<'de> for ParsedManifest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor;
        impl<'de> Visitor<'de> for MapVisitor {
            type Value = BTreeMap<String, FileDigest>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a files.json object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = BTreeMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if out.contains_key(&key) {
                        return Err(de::Error::custom(format!("duplicate path: {key}")));
                    }
                    let value = map.next_value::<FileDigest>()?;
                    out.insert(key, value);
                }
                Ok(out)
            }
        }
        deserializer.deserialize_map(MapVisitor).map(ParsedManifest)
    }
}

enum EntryKind {
    Dir,
    File,
}

fn classify(metadata: &fs::Metadata) -> Result<EntryKind, String> {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err("symlink".to_owned());
    }
    if file_type.is_dir() {
        return Ok(EntryKind::Dir);
    }
    if !file_type.is_file() {
        return Err("not an ordinary file".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err("hard-linked file".to_owned());
        }
    }
    Ok(EntryKind::File)
}

fn relative_name(root: &Path, path: &Path) -> Result<String, TreeError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        TreeError::Violation(format!("path escapes package root: {}", path.display()))
    })?;
    relative
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| TreeError::Violation(format!("non-UTF-8 path: {}", relative.display())))
}

/// Walk `root` against `expected`. `control_files` are regular root files that
/// are not payload (OS packages: `domain-os.yml` and `files.json`). Component
/// installs pass an empty list. `check_modes` is the D2 immutable flag.
pub fn verify_tree(
    root: &Path,
    expected: &BTreeMap<String, FileDigest>,
    executables: &[String],
    check_modes: bool,
    control_files: &[&str],
    cancel: &CancellationToken,
) -> Result<(), TreeError> {
    let root_meta = fs::symlink_metadata(root.components().collect::<std::path::PathBuf>())?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Err(TreeError::Violation(
            "version is not a directory".to_owned(),
        ));
    }
    let file_names: Vec<String> = expected.keys().cloned().collect();
    let parents = parent_dirs(&file_names);
    let mut pending = vec![root.to_path_buf()];
    let mut seen = BTreeSet::new();
    let mut seen_controls = BTreeSet::new();
    while let Some(dir) = pending.pop() {
        if check_modes {
            let label = if dir == root {
                ".".to_owned()
            } else {
                relative_name(root, &dir)?
            };
            check_unix_mode(&dir, DIR_MODE).map_err(|err| match err {
                TreeError::Violation(_) => {
                    TreeError::Violation(format!("installed permissions changed: {label}"))
                }
                other => other,
            })?;
        }
        for entry in fs::read_dir(&dir)? {
            if cancel.is_cancelled() {
                return Err(TreeError::Cancelled);
            }
            let path = entry?.path();
            let name = relative_name(root, &path)?;
            clean_path(&name, false).map_err(TreeError::Violation)?;
            let metadata = fs::symlink_metadata(&path)?;
            if control_files.iter().any(|control| *control == name) {
                match classify(&metadata) {
                    Ok(EntryKind::File) => {}
                    Ok(EntryKind::Dir) => {
                        return Err(TreeError::Violation(format!(
                            "control file is a directory: {name}"
                        )));
                    }
                    Err(why) => {
                        return Err(TreeError::Violation(format!("{why}: {name}")));
                    }
                }
                if check_modes {
                    check_unix_mode(&path, DATA_MODE).map_err(|err| match err {
                        TreeError::Violation(_) => {
                            TreeError::Violation(format!("installed permissions changed: {name}"))
                        }
                        other => other,
                    })?;
                }
                seen_controls.insert(name);
                continue;
            }
            match classify(&metadata) {
                Ok(EntryKind::Dir) if parents.contains(&name) => {
                    pending.push(path);
                    continue;
                }
                Ok(EntryKind::Dir) => {
                    return Err(TreeError::Violation(format!(
                        "unexpected directory: {name}"
                    )));
                }
                Err(why) => return Err(TreeError::Violation(format!("{why}: {name}"))),
                Ok(EntryKind::File) => {}
            }
            let Some(wanted) = expected.get(&name) else {
                return Err(TreeError::Violation(format!(
                    "unexpected installed file: {name}"
                )));
            };
            if metadata.len() != wanted.size {
                return Err(TreeError::HashMismatch(name));
            }
            let mut file = File::open(&path)?;
            if digest_reader(&mut file, cancel)? != *wanted {
                return Err(TreeError::HashMismatch(name));
            }
            if check_modes {
                check_unix_mode(&path, unix_file_mode(executables, &name)).map_err(
                    |err| match err {
                        TreeError::Violation(_) => {
                            TreeError::Violation(format!("installed permissions changed: {name}"))
                        }
                        other => other,
                    },
                )?;
            }
            seen.insert(name);
        }
    }
    for control in control_files {
        if !seen_controls.contains(*control) {
            return Err(TreeError::Violation(format!(
                "missing control file: {control}"
            )));
        }
    }
    if let Some(missing) = expected.keys().find(|name| !seen.contains(*name)) {
        return Err(TreeError::Violation(format!(
            "missing installed file: {missing}"
        )));
    }
    Ok(())
}

/// Hash every payload file under `root`. `skip_root_files` are control files:
/// they must be ordinary files when present, and they are not part of the map.
/// A missing skip name is allowed (`files.json` before the first generate).
pub fn collect_payload(
    root: &Path,
    skip_root_files: &[&str],
    cancel: &CancellationToken,
) -> Result<BTreeMap<String, FileDigest>, TreeError> {
    let root_meta = fs::symlink_metadata(root.components().collect::<std::path::PathBuf>())?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Err(TreeError::Violation(
            "package root is not a real directory".to_owned(),
        ));
    }
    let mut pending = vec![root.to_path_buf()];
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    while let Some(dir) = pending.pop() {
        let mut children = 0usize;
        for entry in fs::read_dir(&dir)? {
            children += 1;
            if cancel.is_cancelled() {
                return Err(TreeError::Cancelled);
            }
            let path = entry?.path();
            let name = relative_name(root, &path)?;
            clean_path(&name, false).map_err(TreeError::Violation)?;
            let metadata = fs::symlink_metadata(&path)?;
            if skip_root_files.iter().any(|skip| *skip == name) {
                match classify(&metadata) {
                    Ok(EntryKind::File) => {}
                    Ok(EntryKind::Dir) => {
                        return Err(TreeError::Violation(format!(
                            "control file is a directory: {name}"
                        )));
                    }
                    Err(why) => {
                        return Err(TreeError::Violation(format!("{why}: {name}")));
                    }
                }
                continue;
            }
            match classify(&metadata) {
                Ok(EntryKind::Dir) => {
                    pending.push(path);
                    continue;
                }
                Err(why) => return Err(TreeError::Violation(format!("{why}: {name}"))),
                Ok(EntryKind::File) => {}
            }
            if files.len() >= MAX_FILE_ENTRIES {
                return Err(TreeError::Violation(format!(
                    "payload has more than {MAX_FILE_ENTRIES} files"
                )));
            }
            let len = metadata.len();
            total = total
                .checked_add(len)
                .ok_or_else(|| TreeError::Violation("payload size overflow".to_owned()))?;
            if total > MAX_PAYLOAD_BYTES {
                return Err(TreeError::Violation(format!(
                    "payload size {total} exceeds the {MAX_PAYLOAD_BYTES} byte limit"
                )));
            }
            let mut file = File::open(&path)?;
            let digest = digest_reader(&mut file, cancel)?;
            if digest.size != len {
                return Err(TreeError::Violation(format!(
                    "file changed while hashing: {name}"
                )));
            }
            files.insert(name, digest);
        }
        if dir != root && children == 0 {
            let name = relative_name(root, &dir)?;
            return Err(TreeError::Violation(format!(
                "unexpected directory: {name}"
            )));
        }
    }
    Ok(files)
}

pub fn normalize_modes(
    root: &Path,
    payload: &BTreeMap<String, FileDigest>,
    executables: &[String],
    control_files: &[&str],
) -> Result<(), TreeError> {
    set_unix_mode(root, DIR_MODE)?;
    let names: Vec<String> = payload.keys().cloned().collect();
    for dir in parent_dirs(&names) {
        set_unix_mode(&root.join(&dir), DIR_MODE)?;
    }
    for name in payload.keys() {
        set_unix_mode(&root.join(name), unix_file_mode(executables, name))?;
    }
    for name in control_files {
        let path = root.join(name);
        if path.is_file() {
            set_unix_mode(&path, DATA_MODE)?;
        }
    }
    Ok(())
}

pub fn map_tree_error(err: TreeError) -> IntegrityError {
    match err {
        TreeError::HashMismatch(path) => IntegrityError::HashMismatch { path },
        TreeError::Violation(detail) => IntegrityError::ManifestInvalid { detail },
        TreeError::Io(err) => IntegrityError::CheckFailed {
            detail: err.to_string(),
        },
        TreeError::Cancelled => IntegrityError::CheckFailed {
            detail: "cancelled".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn digest(size: u64, fill: u8) -> FileDigest {
        FileDigest {
            size,
            sha256: format!("{fill:02x}").repeat(32),
        }
    }

    /// T1. The production binding rejects a wrong JSON digest and a files
    /// list that is not the key set. Dropping the raw-byte compare or the
    /// set compare turns this red.
    #[test]
    fn t1_shared_digest_roundtrip_and_json_binding() {
        let original = digest(3, 0xab);
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: FileDigest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
        assert!(
            serde_json::from_str::<FileDigest>(&encoded.replace("sha256", "sha256_typo")).is_err()
        );

        let mut manifest = BTreeMap::new();
        manifest.insert("b/file".to_owned(), digest(2, 0x11));
        manifest.insert("a/file".to_owned(), digest(1, 0x22));
        let bytes = render_files_json(&manifest).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let a = text.find("\"a/file\"").unwrap();
        let b = text.find("\"b/file\"").unwrap();
        assert!(a < b, "canonical JSON is not UTF-8 path order: {text}");
        assert!(text.ends_with("\n}\n") || text.ends_with("}\n"));

        let section = IntegritySection {
            manifest_file: FILES_JSON_NAME.to_owned(),
            sha256: sha256_hex(&bytes),
            files: vec!["a/file".to_owned(), "b/file".to_owned()],
            executables: vec!["a/file".to_owned()],
        };
        validate_section(&section, Some("a/file")).unwrap();
        let parsed = verify_files_json(&bytes, &section).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(
            parsed.keys().cloned().collect::<Vec<_>>(),
            section.files,
            "JSON keys must equal integrity.files"
        );

        let mut wrong = section.clone();
        wrong.sha256 = "ab".repeat(32);
        let mismatch = verify_files_json(&bytes, &wrong).unwrap_err();
        assert!(
            mismatch
                .to_string()
                .starts_with("hash_mismatch: files.json"),
            "{mismatch}"
        );

        let mut shifted = section.clone();
        shifted.files = vec!["a/file".to_owned()];
        let keys = verify_files_json(&bytes, &shifted).unwrap_err();
        assert!(
            keys.to_string().contains("does not match files.json keys"),
            "{keys}"
        );

        // Same objects, different bytes: the chain covers the JSON bytes.
        let pretty = serde_json::to_vec_pretty(&parsed).unwrap();
        assert_ne!(pretty, bytes);
        let rebound = IntegritySection {
            sha256: sha256_hex(&pretty),
            ..section.clone()
        };
        assert!(verify_files_json(&pretty, &rebound).is_ok());
        assert!(verify_files_json(&pretty, &section).is_err());
    }
    #[test]
    fn t3_strict_json_and_section_schema() {
        for path in [
            "C:/absolute",
            "C:relative",
            "/absolute",
            "a//b",
            "../x",
            "a/./b",
            "a\\b",
            "a\nb",
            "",
        ] {
            assert!(clean_path(path, false).is_err(), "{path:?}");
        }
        let good = format!(
            r#"{{"bin/app":{{"size":1,"sha256":"{}"}}}}"#,
            "ab".repeat(32)
        );
        for bad in [
            good.replace("\"size\":1", "\"size\":-1"),
            good.replace("\"size\":1", "\"size\":1,\"extra\":0"),
            good.replace("\"size\":1", "\"size\":1,\"size\":1"),
            good.replace(&"ab".repeat(32), &"AB".repeat(32)),
            good.replace(&"ab".repeat(32), "1234"),
            good.replace("bin/app", "files.json"),
            good.replace("bin/app", "domain-os.yml"),
        ] {
            assert!(parse_files_json(bad.as_bytes()).is_err(), "{bad}");
        }
        let section = IntegritySection {
            manifest_file: FILES_JSON_NAME.into(),
            sha256: sha256_hex(good.as_bytes()),
            files: vec!["bin/app".into()],
            executables: vec!["bin/app".into()],
        };
        for command in ["app", "/bin/app", "../app", "/bin/sh"] {
            assert!(validate_section(&section, Some(command)).is_err());
        }
        let mut invalid = section.clone();
        invalid.files.push("bin/app".into());
        assert!(validate_section(&invalid, Some("bin/app")).is_err());
        invalid = section.clone();
        invalid.executables.push("bin/app".into());
        assert!(validate_section(&invalid, Some("bin/app")).is_err());
        invalid = section.clone();
        invalid.executables = vec!["missing".into()];
        assert!(validate_section(&invalid, Some("bin/app")).is_err());
        invalid = section.clone();
        invalid.manifest_file = "other.json".into();
        assert!(validate_section(&invalid, Some("bin/app")).is_err());
        invalid = section;
        invalid.sha256 = "x".repeat(64);
        assert!(validate_section(&invalid, Some("bin/app")).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn t3_non_utf8_name_is_rejected_without_lossy_conversion() {
        use std::os::unix::ffi::OsStringExt;
        let root = Path::new("/package");
        let path = root.join(std::ffi::OsString::from_vec(vec![b'x', 0xff]));
        assert!(matches!(
            relative_name(root, &path),
            Err(TreeError::Violation(_))
        ));
    }
}
