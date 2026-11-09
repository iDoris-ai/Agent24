//! `agent24 package-integrity` — generate and verify an OS package file manifest.
//!
//! Release self-checks verify hashes and native format. Host checks additionally
//! enforce the host signature policy after verifying package contents.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use agent24_components::integrity::{
    self, DOMAIN_OS_YML, FILES_JSON_NAME, IntegrityError, IntegritySection,
};
use agent24_domain::DomainOsManifest;

const YAML_LIMIT: u64 = DomainOsManifest::MAX_YAML_BYTES as u64;
use tokio_util::sync::CancellationToken;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Read a package that may or may not declare integrity.
///
/// Absence is success. Presence runs the full shared check and refuses a
/// broken package. This is the PR-1 hook; PR-3 replaces it with a required check.
///
/// `cancel` is the caller's shutdown token. A fired token is
/// [`IntegrityError::Cancelled`]: the hash did not finish, so this is not a
/// hash mismatch and not a successful check. Every scan still hashes the
/// whole tree; nothing is skipped because a stat tuple matched.
pub fn verify_if_present(
    dir: &Path,
    manifest: &DomainOsManifest,
    cancel: &CancellationToken,
) -> Result<(), IntegrityError> {
    verify_with_policy(
        dir,
        manifest,
        cancel,
        &crate::signature::Policy::default(),
        agent24_components::sign::VerifyMode::AtRest,
    )
}

pub fn verify_with_policy(
    dir: &Path,
    manifest: &DomainOsManifest,
    cancel: &CancellationToken,
    policy: &crate::signature::Policy,
    mode: agent24_components::sign::VerifyMode,
) -> Result<(), IntegrityError> {
    verify_contents(
        dir,
        manifest,
        cancel,
        std::env::consts::ARCH,
        Some((policy, mode)),
    )
}

fn verify_if_present_for_arch(
    dir: &Path,
    manifest: &DomainOsManifest,
    cancel: &CancellationToken,
    arch: &str,
) -> Result<(), IntegrityError> {
    verify_contents(dir, manifest, cancel, arch, None)
}

fn verify_contents(
    dir: &Path,
    manifest: &DomainOsManifest,
    cancel: &CancellationToken,
    arch: &str,
    signature: Option<(
        &crate::signature::Policy,
        agent24_components::sign::VerifyMode,
    )>,
) -> Result<(), IntegrityError> {
    if cancel.is_cancelled() {
        return Err(IntegrityError::Cancelled);
    }
    let Some(declared) = manifest.integrity() else {
        // Legacy manifests still need host signature authorization. Omitting
        // the optional hash manifest must not bypass executable verification.
        if let (Some((policy, mode)), Some(spawn)) = (signature, manifest.spawn()) {
            policy.verify(dir, &spawn.command, mode, cancel)?;
        }
        return Ok(());
    };
    verify_declared(
        dir,
        manifest.spawn().map(|spawn| spawn.command.as_str()),
        &IntegritySection {
            manifest_file: declared.manifest_file().to_owned(),
            sha256: declared.sha256().to_owned(),
            files: declared.files().to_vec(),
            executables: declared.executables().to_vec(),
        },
        cancel,
        arch,
        signature,
    )
}

/// Strict read-only check used by `package-integrity verify`.
pub fn verify(root: &Path) -> Result<(), IntegrityError> {
    verify_for_arch(root, std::env::consts::ARCH)
}

/// `package-integrity verify --arch`: the release script's self-check of a
/// package built for another CPU architecture. Discovery, recheck and
/// installation never come through here; they always use the host
/// architecture (`verify_if_present`).
pub fn verify_for_arch(root: &Path, arch: &str) -> Result<(), IntegrityError> {
    check_target_arch(arch)?;
    ensure_real_directory(root)?;
    let bytes = read_regular(&root.join(DOMAIN_OS_YML), DOMAIN_OS_YML, YAML_LIMIT)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| IntegrityError::ManifestInvalid {
        detail: format!("{DOMAIN_OS_YML} is not valid UTF-8"),
    })?;
    let manifest =
        DomainOsManifest::from_yaml(text).map_err(|err| IntegrityError::ManifestInvalid {
            detail: err.to_string(),
        })?;
    match manifest.integrity() {
        None => Err(IntegrityError::Missing),
        // One-shot CLI. The daemon passes its shutdown token through
        // `discovery::scan` / `recheck`, not through this entry.
        Some(_) => verify_if_present_for_arch(root, &manifest, &CancellationToken::new(), arch),
    }
}

/// Normalize `root` and write `files.json` plus the `integrity` section.
///
/// The directory is the release staging area. There is no exclude list.
/// Rewriting is atomic per file; a failed YAML publish rolls `files.json` back.
/// Two successful runs on an unchanged tree produce the same bytes.
pub fn generate(root: &Path, executables: &[String]) -> Result<(), IntegrityError> {
    generate_for_arch(root, executables, std::env::consts::ARCH)
}

fn check_target_arch(arch: &str) -> Result<(), IntegrityError> {
    if !matches!(arch, "aarch64" | "x86_64") {
        return Err(IntegrityError::ManifestInvalid {
            detail: "unsupported target architecture (expected aarch64 or x86_64)".to_owned(),
        });
    }
    Ok(())
}

/// Generate for a target CPU architecture on the host OS.
/// Daemon-side verification (discovery and recheck) always requires the host
/// architecture, including installation.
pub fn generate_for_arch(
    root: &Path,
    executables: &[String],
    arch: &str,
) -> Result<(), IntegrityError> {
    check_target_arch(arch)?;
    if !matches!(std::env::consts::OS, "macos" | "linux" | "windows") {
        return Err(IntegrityError::ManifestInvalid {
            detail: "unsupported target OS (expected macos, linux or windows)".to_owned(),
        });
    }
    ensure_real_directory(root)?;
    if executables.is_empty() {
        return Err(IntegrityError::ManifestInvalid {
            detail: "at least one --executable is required".to_owned(),
        });
    }
    let mut seen = BTreeSet::new();
    for executable in executables {
        integrity::clean_path(executable, false)
            .map_err(|detail| IntegrityError::ManifestInvalid { detail })?;
        if executable == FILES_JSON_NAME || executable == DOMAIN_OS_YML {
            return Err(IntegrityError::ManifestInvalid {
                detail: format!("control file cannot be an executable: {executable}"),
            });
        }
        if !seen.insert(executable.as_str()) {
            return Err(IntegrityError::ManifestInvalid {
                detail: format!("duplicate executable: {executable}"),
            });
        }
    }

    let yaml_path = root.join(DOMAIN_OS_YML);
    let original = read_regular(&yaml_path, DOMAIN_OS_YML, YAML_LIMIT)?;
    let text =
        String::from_utf8(original.clone()).map_err(|_| IntegrityError::ManifestInvalid {
            detail: format!("{DOMAIN_OS_YML} is not valid UTF-8"),
        })?;
    let mut document: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|err| IntegrityError::ManifestInvalid {
            detail: err.to_string(),
        })?;
    let mapping = document
        .as_mapping_mut()
        .ok_or_else(|| IntegrityError::ManifestInvalid {
            detail: "domain-os.yml must be a mapping".to_owned(),
        })?;
    mapping.remove(serde_yaml::Value::from("integrity"));
    let business =
        serde_yaml::to_string(&document).map_err(|err| IntegrityError::ManifestInvalid {
            detail: err.to_string(),
        })?;
    let parsed =
        DomainOsManifest::from_yaml(&business).map_err(|err| IntegrityError::ManifestInvalid {
            detail: err.to_string(),
        })?;
    if let Some(spawn) = parsed.spawn()
        && !executables
            .iter()
            .any(|executable| executable == &spawn.command)
    {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!(
                "spawn.command {} must be passed as --executable",
                spawn.command
            ),
        });
    }

    refuse_stale_temp_files(root)?;
    let cancel = CancellationToken::new();
    let payload = integrity::collect_payload(root, &[DOMAIN_OS_YML, FILES_JSON_NAME], &cancel)
        .map_err(integrity::map_tree_error)?;
    for executable in executables {
        if !payload.contains_key(executable) {
            return Err(IntegrityError::ManifestInvalid {
                detail: format!("executable not in package: {executable}"),
            });
        }
    }
    let mut listed_executables = executables.to_vec();
    listed_executables.sort();
    let json = integrity::render_files_json(&payload)?;
    let section = IntegritySection {
        manifest_file: FILES_JSON_NAME.to_owned(),
        sha256: integrity::sha256_hex(&json),
        files: payload.keys().cloned().collect(),
        executables: listed_executables,
    };
    integrity::validate_section(&section, parsed.spawn().map(|spawn| spawn.command.as_str()))?;
    integrity::verify_files_json(&json, &section)?;
    for executable in executables {
        check_native_executable_for_arch(&root.join(executable), executable, arch)?;
    }
    // Only after the section validates: a refused package (e.g. a bare spawn.command) must
    // leave the staging tree's modes as they were.
    integrity::normalize_modes(
        root,
        &payload,
        executables,
        &[DOMAIN_OS_YML, FILES_JSON_NAME],
    )
    .map_err(integrity::map_tree_error)?;
    let composed = compose_yaml(&business, &integrity_block(&section));
    let republished =
        DomainOsManifest::from_yaml(&composed).map_err(|err| IntegrityError::ManifestInvalid {
            detail: err.to_string(),
        })?;
    let published = republished
        .integrity()
        .ok_or_else(|| IntegrityError::ManifestInvalid {
            detail: "generate did not write an integrity section".to_owned(),
        })?;
    if published.sha256() != section.sha256 {
        return Err(IntegrityError::ManifestInvalid {
            detail: "integrity.sha256 does not match files.json".to_owned(),
        });
    }

    let json_path = root.join(FILES_JSON_NAME);
    // Backed up only to restore on a failed YAML write; bounded like any other read of it.
    let previous_json =
        read_bounded(&json_path, FILES_JSON_NAME, integrity::MAX_FILES_JSON_BYTES).ok();
    let json_existed = previous_json.is_some();
    atomic_write(root, FILES_JSON_NAME, &json)?;
    if let Err(err) = atomic_write(root, DOMAIN_OS_YML, composed.as_bytes()) {
        let _ = restore_files_json(root, json_existed, previous_json.as_deref());
        return Err(err);
    }
    Ok(())
}

fn verify_declared(
    dir: &Path,
    spawn_command: Option<&str>,
    section: &IntegritySection,
    cancel: &CancellationToken,
    arch: &str,
    signature: Option<(
        &crate::signature::Policy,
        agent24_components::sign::VerifyMode,
    )>,
) -> Result<(), IntegrityError> {
    if cancel.is_cancelled() {
        return Err(IntegrityError::Cancelled);
    }
    ensure_real_directory(dir)?;
    integrity::validate_section(section, spawn_command)?;
    let path = dir.join(FILES_JSON_NAME);
    let metadata = fs::symlink_metadata(&path).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            IntegrityError::ManifestInvalid {
                detail: format!("missing control file: {FILES_JSON_NAME}"),
            }
        } else {
            IntegrityError::CheckFailed {
                detail: format!("{FILES_JSON_NAME}: {err}"),
            }
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!("{FILES_JSON_NAME} is not a regular file"),
        });
    }
    if metadata.len() > integrity::MAX_FILES_JSON_BYTES {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!(
                "{FILES_JSON_NAME} is {} bytes, over the {} byte limit",
                metadata.len(),
                integrity::MAX_FILES_JSON_BYTES
            ),
        });
    }
    let bytes = read_bounded(&path, FILES_JSON_NAME, integrity::MAX_FILES_JSON_BYTES)?;
    // `verify_files_json` does not take the token. A cancel during that read
    // still has to beat a hash or manifest error, same as `tree_result`.
    let files_json = integrity::verify_files_json(&bytes, section);
    #[cfg(test)]
    on_files_json_checked();
    let expected = prefer_cancel(cancel, files_json)?;
    // A mismatch (or any other tree error) discovered as the token fires is
    // still an unfinished check. Cancel wins over the mismatch.
    tree_result(
        cancel,
        integrity::verify_tree(
            dir,
            &expected,
            &section.executables,
            true,
            &[DOMAIN_OS_YML, FILES_JSON_NAME],
            cancel,
        ),
    )?;
    for executable in &section.executables {
        prefer_cancel(
            cancel,
            check_native_executable_for_arch(&dir.join(executable), executable, arch),
        )?;
    }
    if let Some((policy, mode)) = signature {
        for executable in &section.executables {
            policy.verify(dir, executable, mode, cancel)?;
        }
    }
    prefer_cancel(cancel, Ok(()))
}

fn tree_result(
    cancel: &CancellationToken,
    result: Result<(), integrity::TreeError>,
) -> Result<(), IntegrityError> {
    prefer_cancel(cancel, result.map_err(integrity::map_tree_error))
}

/// Cancel wins over an error already in hand. The check that produced `result`
/// may not have looked at the token again (`verify_files_json` never does).
fn prefer_cancel<T>(
    cancel: &CancellationToken,
    result: Result<T, IntegrityError>,
) -> Result<T, IntegrityError> {
    if cancel.is_cancelled() {
        return Err(IntegrityError::Cancelled);
    }
    result
}

// Test stand-in for a shutdown that lands while `files.json` is being
// checked: the hook runs after that check returns and before `prefer_cancel`.
#[cfg(test)]
thread_local! {
    static FILES_JSON_CHECKED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn on_files_json_checked() {
    let hook = FILES_JSON_CHECKED.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
fn arm_files_json_checked(hook: impl FnOnce() + 'static) {
    FILES_JSON_CHECKED.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn clear_files_json_checked() {
    FILES_JSON_CHECKED.with(|slot| *slot.borrow_mut() = None);
}

const HEADER_WINDOW: usize = 1024 * 1024;
const MAX_FAT_ARCHES: usize = 32;

fn native_macho<H: object::read::macho::MachHeader>(data: &[u8], arch: &str) -> bool {
    let Ok(header) = H::parse(data, 0) else {
        return false;
    };
    let Ok(endian) = header.endian() else {
        return false;
    };
    let machine = match arch {
        "aarch64" => object::macho::CPU_TYPE_ARM64,
        "x86_64" => object::macho::CPU_TYPE_X86_64,
        _ => return false,
    };
    if header.cputype(endian) != machine {
        return false;
    }
    if header.filetype(endian) != object::macho::MH_EXECUTE {
        return false;
    }
    // load_commands checks sizeofcmds against this bounded window, even with no commands.
    let Ok(mut commands) = header.load_commands(endian, data, 0) else {
        return false;
    };
    let mut has_entry = false;
    loop {
        match commands.next() {
            Ok(Some(command)) => {
                // `variant` reads the command's body, so an entry command too short
                // to hold its fields is rejected rather than counted by number alone.
                let cmd = command.cmd();
                match command.variant() {
                    Ok(object::read::macho::LoadCommandVariant::EntryPoint(_)) => has_entry = true,
                    // A thread command carries at least a flavor and a count after its header.
                    Ok(object::read::macho::LoadCommandVariant::Thread(_, state))
                        if cmd == object::macho::LC_UNIXTHREAD && state.len() >= 8 =>
                    {
                        has_entry = true;
                    }
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
            Ok(None) => return has_entry,
            Err(_) => return false,
        }
    }
}

fn native_elf<H: object::read::elf::FileHeader>(data: &[u8], arch: &str) -> bool {
    let Ok(header) = H::parse(data) else {
        return false;
    };
    let Ok(endian) = header.endian() else {
        return false;
    };
    let machine = match arch {
        "aarch64" => object::elf::EM_AARCH64,
        "x86_64" => object::elf::EM_X86_64,
        _ => return false,
    };
    if header.e_machine(endian) != machine {
        return false;
    }
    matches!(
        header.e_type(endian),
        object::elf::ET_EXEC | object::elf::ET_DYN
    ) && header.e_entry(endian).into() != 0
}

fn native_pe<H: object::read::pe::ImageNtHeaders>(data: &[u8], arch: &str) -> bool {
    let Ok(dos) = object::pe::ImageDosHeader::parse(data) else {
        return false;
    };
    let Ok((header, _)) = H::parse(data, &mut u64::from(dos.nt_headers_offset())) else {
        return false;
    };
    let machine = match arch {
        "aarch64" => object::pe::IMAGE_FILE_MACHINE_ARM64,
        "x86_64" => object::pe::IMAGE_FILE_MACHINE_AMD64,
        _ => return false,
    };
    if header.file_header().machine.get(object::LittleEndian) != machine {
        return false;
    }
    let flags = header
        .file_header()
        .characteristics
        .get(object::LittleEndian);
    flags & object::pe::IMAGE_FILE_EXECUTABLE_IMAGE != 0 && flags & object::pe::IMAGE_FILE_DLL == 0
}

fn native_executable(data: &[u8], platform: &str, arch: &str) -> bool {
    use object::{Endianness, FileKind};
    let data = &data[..data.len().min(HEADER_WINDOW)];
    match (platform, FileKind::parse(data)) {
        ("macos", Ok(FileKind::MachO32)) => {
            native_macho::<object::macho::MachHeader32<Endianness>>(data, arch)
        }
        ("macos", Ok(FileKind::MachO64)) => {
            native_macho::<object::macho::MachHeader64<Endianness>>(data, arch)
        }
        ("linux", Ok(FileKind::Elf32)) => {
            native_elf::<object::elf::FileHeader32<Endianness>>(data, arch)
        }
        ("linux", Ok(FileKind::Elf64)) => {
            native_elf::<object::elf::FileHeader64<Endianness>>(data, arch)
        }
        ("windows", Ok(FileKind::Pe32)) => native_pe::<object::pe::ImageNtHeaders32>(data, arch),
        ("windows", Ok(FileKind::Pe64)) => native_pe::<object::pe::ImageNtHeaders64>(data, arch),
        _ => false,
    }
}

fn read_header(reader: &mut impl Read, size: u64) -> io::Result<Vec<u8>> {
    let mut window = vec![0; size.min(HEADER_WINDOW as u64) as usize];
    let mut filled = 0;
    while filled < window.len() {
        match reader.read(&mut window[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    window.truncate(filled);
    Ok(window)
}

/// Only the host slice is read; the container and slice each own at most one window.
fn native_fat_slice<A: object::read::macho::FatArch>(
    reader: &mut (impl Read + Seek),
    arches: &[A],
    arch: &str,
) -> bool {
    let host = if arch == "aarch64" {
        object::Architecture::Aarch64
    } else if arch == "x86_64" {
        object::Architecture::X86_64
    } else {
        return false;
    };
    if arches.len() > MAX_FAT_ARCHES {
        return false;
    }
    arches
        .iter()
        .find(|arch| arch.architecture() == host)
        .is_some_and(|slice| {
            let (offset, size) = slice.file_range();
            if offset
                .checked_add(size)
                .zip(reader.seek(SeekFrom::End(0)).ok())
                .is_none_or(|(end, len)| end > len)
            {
                return false;
            }
            reader.seek(SeekFrom::Start(offset)).is_ok()
                && read_header(reader, size)
                    .is_ok_and(|window| native_executable(&window, "macos", arch))
        })
}

fn native_reader(reader: &mut (impl Read + Seek), platform: &str, arch: &str) -> bool {
    use object::read::macho::{MachOFatFile32, MachOFatFile64};
    let Ok(window) = read_header(reader, u64::MAX) else {
        return false;
    };
    match object::FileKind::parse(window.as_slice()) {
        Ok(object::FileKind::MachOFat32) if platform == "macos" => {
            MachOFatFile32::parse(window.as_slice())
                .is_ok_and(|fat| native_fat_slice(reader, fat.arches(), arch))
        }
        Ok(object::FileKind::MachOFat64) if platform == "macos" => {
            MachOFatFile64::parse(window.as_slice())
                .is_ok_and(|fat| native_fat_slice(reader, fat.arches(), arch))
        }
        _ => native_executable(&window, platform, arch),
    }
}

#[cfg(test)]
fn check_native_executable(path: &Path, rel_path: &str) -> Result<(), IntegrityError> {
    check_native_executable_for_arch(path, rel_path, std::env::consts::ARCH)
}

fn check_native_executable_for_arch(
    path: &Path,
    rel_path: &str,
    arch: &str,
) -> Result<(), IntegrityError> {
    #[cfg(unix)]
    let opened = {
        use rustix::fs::{Mode, OFlags};
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    };
    // On Windows rustix is unavailable; collect_payload/verify_tree already rejected symlinks.
    // Known boundary (v1): a concurrent writer replacing the file with a link between that
    // check and this open is not caught here — the same class as S1-9 §3's documented
    // "after recheck, before spawn" window.
    #[cfg(not(unix))]
    let opened = File::open(path);
    let mut file = opened.map_err(|err| IntegrityError::CheckFailed {
        detail: format!("{rel_path}: {err}"),
    })?;
    let metadata = file.metadata().map_err(|err| IntegrityError::CheckFailed {
        detail: format!("{rel_path}: {err}"),
    })?;
    if !metadata.is_file() {
        return Err(IntegrityError::CheckFailed {
            detail: format!("{rel_path}: not a regular file"),
        });
    }
    if !native_reader(&mut file, std::env::consts::OS, arch) {
        return Err(IntegrityError::NotNativeExecutable {
            path: rel_path.to_owned(),
        });
    }
    Ok(())
}

fn ensure_real_directory(root: &Path) -> Result<(), IntegrityError> {
    let metadata = fs::symlink_metadata(root.components().collect::<std::path::PathBuf>())
        .map_err(|err| IntegrityError::CheckFailed {
            detail: err.to_string(),
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(IntegrityError::ManifestInvalid {
            detail: "package root is not a real directory".to_owned(),
        });
    }
    Ok(())
}

fn read_regular(path: &Path, label: &str, limit: u64) -> Result<Vec<u8>, IntegrityError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            IntegrityError::ManifestInvalid {
                detail: format!("missing control file: {label}"),
            }
        } else {
            IntegrityError::CheckFailed {
                detail: format!("{label}: {err}"),
            }
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!("{label} is not a regular file"),
        });
    }
    read_bounded(path, label, limit)
}

/// Reads at most `limit + 1` bytes so an oversized or sparse control file is refused
/// before it is pulled into memory.
fn read_bounded(path: &Path, label: &str, limit: u64) -> Result<Vec<u8>, IntegrityError> {
    use std::io::Read as _;
    let file = File::open(path).map_err(|err| IntegrityError::CheckFailed {
        detail: format!("{label}: {err}"),
    })?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| IntegrityError::CheckFailed {
            detail: format!("{label}: {err}"),
        })?;
    if bytes.len() as u64 > limit {
        return Err(IntegrityError::ManifestInvalid {
            detail: format!("{label} is over the {limit} byte limit"),
        });
    }
    Ok(bytes)
}

/// `atomic_write` names its temporaries `.<name>.<pid>.<n>.tmp`. One left by a crashed run
/// would otherwise be hashed into the next manifest as payload.
fn refuse_stale_temp_files(root: &Path) -> Result<(), IntegrityError> {
    let entries = fs::read_dir(root).map_err(|err| IntegrityError::CheckFailed {
        detail: format!("{}: {err}", root.display()),
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| IntegrityError::CheckFailed {
            detail: err.to_string(),
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ours = [DOMAIN_OS_YML, FILES_JSON_NAME]
            .iter()
            .any(|control| name.starts_with(&format!(".{control}.")));
        if ours && name.ends_with(".tmp") {
            return Err(IntegrityError::ManifestInvalid {
                detail: format!(
                    "stale temporary file {name} from an interrupted generate; remove it and retry"
                ),
            });
        }
    }
    Ok(())
}

fn restore_files_json(root: &Path, existed: bool, previous: Option<&[u8]>) -> io::Result<()> {
    let path = root.join(FILES_JSON_NAME);
    match (existed, previous) {
        (true, Some(bytes)) => atomic_write(root, FILES_JSON_NAME, bytes)
            .map_err(|err| io::Error::other(err.to_string())),
        _ => fs::remove_file(path),
    }
}

fn atomic_write(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), IntegrityError> {
    let dest = dir.join(name);
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let write_result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        integrity::set_unix_mode(&tmp, integrity::DATA_MODE)?;
        fs::rename(&tmp, &dest)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result.map_err(|err| IntegrityError::CheckFailed {
        detail: format!("{name}: {err}"),
    })
}

fn compose_yaml(business: &str, block: &str) -> String {
    let mut out = business.trim_end_matches(['\n', '\r']).to_owned();
    out.push('\n');
    out.push_str(block);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn integrity_block(section: &IntegritySection) -> String {
    let mut out = String::from("integrity:\n  manifest: files.json\n");
    out.push_str(&format!("  sha256: {}\n", section.sha256));
    push_yaml_list(&mut out, "files", &section.files);
    push_yaml_list(&mut out, "executables", &section.executables);
    out
}

fn push_yaml_list(out: &mut String, key: &str, values: &[String]) {
    if values.is_empty() {
        out.push_str(&format!("  {key}: []\n"));
        return;
    }
    out.push_str(&format!("  {key}:\n"));
    for value in values {
        out.push_str("    - ");
        out.push_str(&yaml_quote(value));
        out.push('\n');
    }
}

fn yaml_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' | '"' => {
                out.push('\\');
                out.push(character);
            }
            _ => out.push(character),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use super::*;
    use agent24_components::integrity::{MAX_FILES_JSON_BYTES, MAX_PAYLOAD_BYTES, sha256_hex};
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn yaml_for(name: &str) -> String {
        format!(
            "# keep-me\nname: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: [events]\nimpl_kind: out_of_process_provider\n\
             spawn:\n  command: bin/{name}\n"
        )
    }

    fn copy_current_exe(dest: &Path) {
        let current = std::env::current_exe().unwrap();
        fs::copy(current, dest).unwrap();
    }

    fn write_tree(dir: &Path, name: &str) {
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::create_dir_all(dir.join("config")).unwrap();
        fs::write(dir.join(DOMAIN_OS_YML), yaml_for(name)).unwrap();
        copy_current_exe(&dir.join("bin").join(name));
        copy_current_exe(&dir.join("bin/helper"));
        fs::write(dir.join("config/default.json"), b"{\"ok\":true}\n").unwrap();
    }

    fn seal(path: &Path) {
        let mut permissions = fs::symlink_metadata(path).unwrap().permissions();
        permissions.set_mode(0o400);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn mode_of(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn a_generated_package_still_scans_after_os_install() {
        // #1614 B1: install used to create directories 0755 while verify_tree wants 0700.
        let staging = tempfile::tempdir().unwrap();
        write_tree(staging.path(), "cos72");
        generate(staging.path(), &["bin/cos72".into(), "bin/helper".into()]).unwrap();
        verify(staging.path()).unwrap();
        let packages = tempfile::tempdir().unwrap();
        let root = packages.path().join("packages");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        crate::test_support::install(staging.path(), &root).unwrap();
        let scan = crate::test_support::scan(&root, &CancellationToken::new());
        assert_eq!(scan.found.len(), 1, "refused: {:?}", scan.refused);
    }

    #[test]
    fn a_refused_generate_leaves_staging_modes_untouched() {
        // #1614 N1: validation runs before normalize_modes.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_tree(root, "cos72");
        let yaml = yaml_for("cos72").replace("command: bin/cos72", "command: cos72");
        fs::write(root.join(DOMAIN_OS_YML), yaml).unwrap();
        let before = mode_of(&root.join("config/default.json"));
        assert!(generate(root, &["bin/cos72".into()]).is_err());
        assert_eq!(mode_of(&root.join("config/default.json")), before);
    }

    #[test]
    fn a_stale_temp_file_from_a_crashed_generate_is_refused() {
        // #1614 N5: it would otherwise be hashed into the manifest as payload.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_tree(root, "cos72");
        fs::write(root.join(".files.json.4242.0.tmp"), b"{}").unwrap();
        let err = generate(root, &["bin/cos72".into()]).unwrap_err();
        assert!(err.to_string().contains("stale temporary file"), "{err}");
    }

    #[test]
    fn oversized_yaml_is_refused_without_reading_it_whole() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_tree(root, "cos72");
        let mut yaml = yaml_for("cos72");
        yaml.push_str(&format!(
            "# {}\n",
            "x".repeat(DomainOsManifest::MAX_YAML_BYTES)
        ));
        fs::write(root.join(DOMAIN_OS_YML), yaml).unwrap();
        let err = verify(root).unwrap_err();
        assert!(err.to_string().contains("byte limit"), "{err}");
        let err = generate(root, &["bin/cos72".into()]).unwrap_err();
        assert!(err.to_string().contains("byte limit"), "{err}");
    }

    #[test]
    fn bare_spawn_command_is_rejected_even_when_a_same_named_file_is_listed() {
        // `sh` in the package root would satisfy "is an integrity executable", but launch
        // resolves a bare name on PATH and would run an unverified binary outside the package.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_tree(root, "cos72");
        let yaml = yaml_for("cos72").replace("command: bin/cos72", "command: sh");
        fs::write(root.join(DOMAIN_OS_YML), yaml).unwrap();
        fs::write(root.join("sh"), b"not-a-shell").unwrap();
        let err = generate(root, &["sh".into(), "bin/cos72".into()]).unwrap_err();
        assert!(err.to_string().contains("package-relative path"), "{err}");
        // A hand-written section with the same shape is refused by the shared validator too,
        // which verify and discovery both call.
        let section = IntegritySection {
            manifest_file: FILES_JSON_NAME.into(),
            sha256: "0".repeat(64),
            files: vec!["sh".into()],
            executables: vec!["sh".into()],
        };
        let err = integrity::validate_section(&section, Some("sh")).unwrap_err();
        assert!(err.to_string().contains("package-relative path"), "{err}");
        assert!(integrity::validate_section(&section, Some("./sh")).is_err());
    }

    #[test]
    fn t2_generate_preserves_flow_yaml_business_fields() {
        let dir = tempfile::tempdir().unwrap();
        write_tree(dir.path(), "cos72");
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml_for("cos72")).unwrap();
        let flow = "{name: cos72, version: '0.1.0', route_namespace: /api/v1/cos72, event_module: cos72, data_dir: '~/.agent24/os/cos72/', kernel_capabilities: [events], impl_kind: out_of_process_provider, spawn: {command: bin/cos72}}\n";
        fs::write(dir.path().join(DOMAIN_OS_YML), flow).unwrap();
        generate(dir.path(), &["bin/cos72".into()]).unwrap();
        verify(dir.path()).unwrap();
        let first = fs::read(dir.path().join(DOMAIN_OS_YML)).unwrap();
        let mut actual: serde_yaml::Value = serde_yaml::from_slice(&first).unwrap();
        actual
            .as_mapping_mut()
            .unwrap()
            .remove(serde_yaml::Value::from("integrity"));
        assert_eq!(actual, value);
        generate(dir.path(), &["bin/cos72".into()]).unwrap();
        assert_eq!(first, fs::read(dir.path().join(DOMAIN_OS_YML)).unwrap());
    }

    /// T2. Canonical order is part of the assertion, so a stable-but-unsorted
    /// generator still fails. `verify` is read-only.
    #[test]
    fn t2_generate_is_byte_stable_and_verify_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_tree(root, "cos72");
        let executables = vec!["bin/helper".to_owned(), "bin/cos72".to_owned()];
        generate(root, &executables).unwrap();
        let yaml = fs::read(root.join(DOMAIN_OS_YML)).unwrap();
        let json = fs::read(root.join(FILES_JSON_NAME)).unwrap();
        let yaml_text = std::str::from_utf8(&yaml).unwrap();
        let json_text = std::str::from_utf8(&json).unwrap();
        let bin_at = json_text.find("\"bin/cos72\"").unwrap();
        let helper_at = json_text.find("\"bin/helper\"").unwrap();
        let config_at = json_text.find("\"config/default.json\"").unwrap();
        assert!(bin_at < helper_at && helper_at < config_at, "{json_text}");
        let exe_at = yaml_text.find("executables:").unwrap();
        let exe_block = &yaml_text[exe_at..];
        assert!(
            exe_block.find("bin/cos72").unwrap() < exe_block.find("bin/helper").unwrap(),
            "{exe_block}"
        );

        generate(root, &["bin/cos72".into(), "bin/helper".into()]).unwrap();
        assert_eq!(fs::read(root.join(DOMAIN_OS_YML)).unwrap(), yaml);
        assert_eq!(fs::read(root.join(FILES_JSON_NAME)).unwrap(), json);
        verify(root).unwrap();
        assert_eq!(fs::read(root.join(DOMAIN_OS_YML)).unwrap(), yaml);
        assert_eq!(fs::read(root.join(FILES_JSON_NAME)).unwrap(), json);
        assert_eq!(mode_of(root), 0o700);
        assert_eq!(mode_of(&root.join("bin")), 0o700);
        assert_eq!(mode_of(&root.join("config")), 0o700);
        assert_eq!(mode_of(&root.join("bin/cos72")), 0o500);
        assert_eq!(mode_of(&root.join("bin/helper")), 0o500);
        assert_eq!(mode_of(&root.join("config/default.json")), 0o400);
        assert_eq!(mode_of(&root.join(DOMAIN_OS_YML)), 0o400);
        assert_eq!(mode_of(&root.join(FILES_JSON_NAME)), 0o400);
        for entry in fs::read_dir(root).unwrap() {
            let name = entry.unwrap().file_name();
            let name = name.to_str().unwrap();
            assert!(!name.starts_with('.'), "leftover temp file {name}");
        }

        let duplicate = generate(root, &["bin/cos72".into(), "bin/cos72".into()]).unwrap_err();
        assert!(
            duplicate.to_string().contains("duplicate executable"),
            "{duplicate}"
        );
    }

    fn generated(name: &str) -> (tempfile::TempDir, PathBuf) {
        let parent = tempfile::tempdir().unwrap();
        let package = parent.path().join(name);
        fs::create_dir(&package).unwrap();
        write_tree(&package, name);
        generate(&package, &[format!("bin/{name}"), "bin/helper".into()]).unwrap();
        (parent, package)
    }

    fn reason(package: &Path) -> String {
        verify(package).unwrap_err().to_string()
    }

    fn scan_why(parent: &Path, name: &str) -> String {
        let scan = crate::test_support::scan(parent, &CancellationToken::new());
        scan.refused
            .iter()
            .find(|refused| refused.dir.ends_with(name))
            .map(|refused| refused.why.clone())
            .unwrap_or_else(|| format!("not refused: {:?}", scan.found))
    }

    fn rewrite_controls(package: &Path, json: &str, files: &[&str], executables: &[&str]) {
        let sha = sha256_hex(json.as_bytes());
        let yaml = fs::read_to_string(package.join(DOMAIN_OS_YML)).unwrap();
        let business = yaml.split("\nintegrity:\n").next().unwrap();
        let section = IntegritySection {
            manifest_file: FILES_JSON_NAME.to_owned(),
            sha256: sha,
            files: files.iter().map(|file| (*file).to_owned()).collect(),
            executables: executables.iter().map(|file| (*file).to_owned()).collect(),
        };
        let composed = compose_yaml(business, &integrity_block(&section));
        for name in [DOMAIN_OS_YML, FILES_JSON_NAME] {
            let path = package.join(name);
            let mut permissions = fs::symlink_metadata(&path).unwrap().permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(&path, permissions).unwrap();
        }
        fs::write(package.join(DOMAIN_OS_YML), composed).unwrap();
        fs::write(package.join(FILES_JSON_NAME), json).unwrap();
        seal(&package.join(DOMAIN_OS_YML));
        seal(&package.join(FILES_JSON_NAME));
    }

    /// T3. Each arm names the check it exists to pin. Removing that check,
    /// and leaving the others, turns that arm red.
    #[test]
    #[allow(clippy::type_complexity)]
    fn t3_tree_digest_link_permission_path_and_limit_failures() {
        let cases: &[(&str, fn(&Path) -> String)] = &[
            ("unexpected installed file: note.txt", |package| {
                fs::write(package.join("note.txt"), b"x").unwrap();
                reason(package)
            }),
            ("missing installed file: config/default.json", |package| {
                fs::remove_file(package.join("config/default.json")).unwrap();
                reason(package)
            }),
            ("hash_mismatch: config/default.json", |package| {
                let path = package.join("config/default.json");
                let mut permissions = fs::symlink_metadata(&path).unwrap().permissions();
                permissions.set_mode(0o600);
                fs::set_permissions(&path, permissions).unwrap();
                let original = fs::read(&path).unwrap();
                let mut changed = original.clone();
                changed[0] ^= 0x01;
                assert_eq!(changed.len(), original.len());
                fs::write(&path, changed).unwrap();
                seal(&path);
                reason(package)
            }),
            ("symlink: config/default.json", |package| {
                let path = package.join("config/default.json");
                let outside = package.parent().unwrap().join("same.json");
                fs::write(&outside, fs::read(&path).unwrap()).unwrap();
                fs::remove_file(&path).unwrap();
                symlink(&outside, &path).unwrap();
                reason(package)
            }),
            (
                "installed permissions changed: config/default.json",
                |package| {
                    let path = package.join("config/default.json");
                    let mut permissions = fs::symlink_metadata(&path).unwrap().permissions();
                    permissions.set_mode(0o644);
                    fs::set_permissions(&path, permissions).unwrap();
                    reason(package)
                },
            ),
            ("hard-linked file: config/default.json", |package| {
                let path = package.join("config/default.json");
                let outside = package.parent().unwrap().join("linked.json");
                fs::hard_link(&path, &outside).unwrap();
                reason(package)
            }),
            ("unexpected directory: emptydir", |package| {
                fs::create_dir(package.join("emptydir")).unwrap();
                reason(package)
            }),
            ("unsafe path: ../x", |package| {
                let json = "{\n  \"../x\": {\"size\": 1, \"sha256\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}\n}\n";
                rewrite_controls(package, json, &["../x", "bin/cos72"], &["bin/cos72"]);
                reason(package)
            }),
            ("duplicate path: bin/cos72", |package| {
                let json = fs::read_to_string(package.join(FILES_JSON_NAME)).unwrap();
                let line = json
                    .lines()
                    .find(|line| line.contains("\"bin/cos72\""))
                    .unwrap()
                    .trim_end_matches(',')
                    .to_owned();
                let duplicated = json.replacen(&line, &format!("{line},\n{line}"), 1);
                let yaml_files = ["bin/cos72", "bin/helper", "config/default.json"];
                rewrite_controls(
                    package,
                    &duplicated,
                    &yaml_files,
                    &["bin/cos72", "bin/helper"],
                );
                reason(package)
            }),
            ("entry limit", |package| {
                let hash = "ab".repeat(32);
                let mut json = String::from("{");
                for index in 0..=integrity::MAX_FILE_ENTRIES {
                    if index > 0 {
                        json.push(',');
                    }
                    json.push_str(&format!(
                        "\"f{index:04}\":{{\"size\":0,\"sha256\":\"{hash}\"}}"
                    ));
                }
                json.push('}');
                assert!(
                    json.len() as u64 <= MAX_FILES_JSON_BYTES,
                    "fixture is the entry cap, not the byte cap: {} bytes",
                    json.len()
                );
                rewrite_controls(package, &json, &["bin/cos72"], &["bin/cos72"]);
                reason(package)
            }),
            ("payload size", |package| {
                let current = fs::read_to_string(package.join(FILES_JSON_NAME)).unwrap();
                let pos = current
                    .find("\"size\": ")
                    .expect("size field in files.json");
                let num_start = pos + 8;
                let num_end = current[num_start..]
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap()
                    + num_start;
                let blown = format!(
                    "{}\"size\": {}{}",
                    &current[..pos],
                    MAX_PAYLOAD_BYTES + 1,
                    &current[num_end..]
                );
                // The replaced size is not the real size; key set stays the same.
                let files = ["bin/cos72", "bin/helper", "config/default.json"];
                rewrite_controls(package, &blown, &files, &["bin/cos72", "bin/helper"]);
                reason(package)
            }),
            ("byte limit", |package| {
                let mut blob = vec![b' '; (MAX_FILES_JSON_BYTES + 1) as usize];
                blob[0] = b'{';
                let text = String::from_utf8(blob).unwrap();
                rewrite_controls(package, &text, &["bin/cos72"], &["bin/cos72"]);
                reason(package)
            }),
        ];

        for (needle, mutate) in cases {
            let (parent, package) = generated("cos72");
            let message = mutate(&package);
            assert!(
                message.contains(needle),
                "verify wanted {needle} in {message}"
            );
            let scanned = scan_why(parent.path(), "cos72");
            assert!(
                scanned.contains(needle),
                "scan wanted {needle} in {scanned}"
            );
        }
    }

    /// T4. Discovery keeps an old package, including one with extra files.
    /// A package that declares integrity and then changes a payload file is
    /// refused, and a sibling package is still returned. `verify` on the old
    /// package stays strict.
    #[test]
    fn t4_old_package_is_readable_and_a_damaged_new_package_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let old = parent.path().join("oldmod");
        fs::create_dir_all(old.join("bin")).unwrap();
        fs::write(old.join(DOMAIN_OS_YML), yaml_for("oldmod")).unwrap();
        fs::write(old.join("bin/oldmod"), b"not-hashed").unwrap();
        fs::write(old.join("notes.txt"), b"extra").unwrap();

        let new_package = parent.path().join("newmod");
        fs::create_dir(&new_package).unwrap();
        write_tree(&new_package, "newmod");
        generate(&new_package, &["bin/newmod".into(), "bin/helper".into()]).unwrap();

        let first = crate::test_support::scan(parent.path(), &CancellationToken::new());
        let old_found = first
            .found
            .iter()
            .find(|found| found.manifest.name() == "oldmod")
            .unwrap();
        let new_found = first
            .found
            .iter()
            .find(|found| found.manifest.name() == "newmod")
            .unwrap();
        assert!(
            first
                .found
                .iter()
                .any(|found| found.manifest.name() == "oldmod"),
            "old package refused: {:?}",
            first.refused
        );
        crate::test_support::recheck(&old, &old_found.digest, &CancellationToken::new()).unwrap();
        crate::test_support::recheck(&new_package, &new_found.digest, &CancellationToken::new())
            .unwrap();
        let old_verify = verify(&old).unwrap_err();
        assert!(
            old_verify
                .to_string()
                .contains("integrity_manifest_missing"),
            "{old_verify}"
        );

        let binary = new_package.join("bin/newmod");
        let mut permissions = fs::symlink_metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&binary, permissions).unwrap();
        let mut bytes = fs::read(&binary).unwrap();
        bytes[0] ^= 0x01;
        fs::write(&binary, bytes).unwrap();

        let second = crate::test_support::scan(parent.path(), &CancellationToken::new());
        assert!(
            second
                .found
                .iter()
                .any(|found| found.manifest.name() == "oldmod")
        );
        assert!(
            second
                .found
                .iter()
                .all(|found| found.manifest.name() != "newmod")
        );
        let why = second
            .refused
            .iter()
            .find(|refused| refused.dir.ends_with("newmod"))
            .unwrap()
            .why
            .clone();
        assert!(why.contains("hash_mismatch: bin/newmod"), "{why}");
        let recheck = crate::test_support::recheck(
            &new_package,
            &new_found.digest,
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(recheck.contains("hash_mismatch: bin/newmod"), "{recheck}");
        crate::test_support::recheck(&old, &old_found.digest, &CancellationToken::new()).unwrap();
    }
    #[test]
    fn t3_control_directory_special_file_and_non_utf8_boundaries() {
        for name in [DOMAIN_OS_YML, FILES_JSON_NAME, "bin/cos72", "bin"] {
            let (parent, package) = generated("cos72");
            let original = package.join(name);
            let outside = parent.path().join("outside");
            fs::rename(&original, &outside).unwrap();
            symlink(&outside, &original).unwrap();
            assert!(verify(&package).is_err(), "symlink {name}");
            assert!(
                generate(&package, &["bin/cos72".into()]).is_err(),
                "generate symlink {name}"
            );
        }
        let (parent, package) = generated("cos72");
        let alias = parent.path().join("alias");
        symlink(&package, &alias).unwrap();
        for suffix in ["", "/", "/."] {
            let alias = PathBuf::from(format!("{}{suffix}", alias.display()));
            assert!(verify(&alias).is_err(), "{}", alias.display());
            assert!(generate(&alias, &["bin/cos72".into()]).is_err());
        }
        for name in [".", "bin", "bin/cos72", DOMAIN_OS_YML, FILES_JSON_NAME] {
            let (_parent, package) = generated("cos72");
            let path = package.join(name);
            let original = mode_of(&path);
            let modes = if path.is_dir() {
                vec![original | 0o020]
            } else {
                vec![original | 0o020, original | 0o4000, original | 0o2000]
            };
            for mode in modes {
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
                let actual = mode_of(&path);
                if actual == mode {
                    assert!(verify(&package).is_err(), "mode {mode:o}: {name}");
                } else {
                    // Some sandboxes strip set-ID bits even when chmod succeeds.
                    assert_eq!(actual, original, "unexpected chmod result for {name}");
                    eprintln!(
                        "set-ID fixture unavailable: requested {mode:o}, got {actual:o}: {name}"
                    );
                }
            }
        }
        // APFS rejects non-UTF-8 names before the verifier can see them.
        #[cfg(not(target_os = "macos"))]
        {
            use std::os::unix::ffi::OsStringExt;
            let (_parent, package) = generated("cos72");
            let name = std::ffi::OsString::from_vec(vec![b'x', 0xff]);
            fs::write(package.join(name), b"x").unwrap();
            assert!(reason(&package).contains("non-UTF-8"));
            assert!(generate(&package, &["bin/cos72".into()]).is_err());
        }

        let (_parent, package) = generated("cos72");
        let fifo = package.join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        assert!(reason(&package).contains("not an ordinary file"));
        assert!(generate(&package, &["bin/cos72".into()]).is_err());
    }

    /// `files.json` does not match, and the token fires after that check
    /// returns — before the error is published. The entry `is_cancelled`
    /// check has already passed. Publishing `verify_files_json`'s error
    /// directly (`?` without [`prefer_cancel`]) turns this into
    /// `hash_mismatch: files.json`.
    #[test]
    fn a_files_json_mismatch_while_cancelled_is_cancelled() {
        let (_parent, package) = generated("cos72");
        let json_path = package.join(FILES_JSON_NAME);
        let mut permissions = fs::symlink_metadata(&json_path).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&json_path, permissions).unwrap();
        let mut bytes = fs::read(&json_path).unwrap();
        bytes.push(b'\n');
        fs::write(&json_path, &bytes).unwrap();
        let yaml = fs::read_to_string(package.join(DOMAIN_OS_YML)).unwrap();
        let manifest = DomainOsManifest::from_yaml(&yaml).unwrap();
        let cancel = CancellationToken::new();
        let flag = cancel.clone();
        super::arm_files_json_checked(move || flag.cancel());
        let _clear = FilesJsonHookGuard;
        let err = verify_if_present(&package, &manifest, &cancel).unwrap_err();
        assert!(matches!(err, IntegrityError::Cancelled), "{err}");
        assert!(!err.to_string().contains("hash_mismatch"), "{err}");
    }

    struct FilesJsonHookGuard;

    impl Drop for FilesJsonHookGuard {
        fn drop(&mut self) {
            super::clear_files_json_checked();
        }
    }

    /// The tree walk already decided this was a mismatch. The token fired
    /// before that error was published, so the caller must see `cancelled`.
    /// Dropping the cancel arm in [`prefer_cancel`] turns this into
    /// `hash_mismatch`.
    #[test]
    fn a_hash_mismatch_while_cancelled_is_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = tree_result(
            &cancel,
            Err(integrity::TreeError::HashMismatch("bin/mod".to_owned())),
        )
        .unwrap_err();
        assert!(matches!(err, IntegrityError::Cancelled), "{err}");
        assert!(!err.to_string().contains("hash_mismatch"), "{err}");
    }

    #[test]
    fn package_integrity_rejects_scripts_and_random_bytes_on_generate_and_verify() {
        let parent = tempfile::tempdir().unwrap();
        let pkg = parent.path().join("script_pkg");
        write_tree(&pkg, "script_pkg");

        // 1. Script is rejected at generate
        fs::write(pkg.join("bin/script_pkg"), b"#!/bin/sh\necho oops\n").unwrap();
        let gen_err = generate(&pkg, &["bin/script_pkg".into(), "bin/helper".into()]).unwrap_err();
        assert!(
            gen_err
                .to_string()
                .contains("not_native_executable: bin/script_pkg"),
            "got {gen_err}"
        );

        // 2. Random bytes are rejected at generate
        fs::write(
            pkg.join("bin/script_pkg"),
            b"not a native executable binary header",
        )
        .unwrap();
        let gen_rand = generate(&pkg, &["bin/script_pkg".into(), "bin/helper".into()]).unwrap_err();
        assert!(
            gen_rand
                .to_string()
                .contains("not_native_executable: bin/script_pkg"),
            "got {gen_rand}"
        );

        // 3. Verify on forged package with valid files.json but script spawn.command is rejected
        copy_current_exe(&pkg.join("bin/script_pkg"));
        generate(&pkg, &["bin/script_pkg".into(), "bin/helper".into()]).unwrap();
        assert!(verify(&pkg).is_ok());

        // Craft files.json where bin/script_pkg has script content and correct hash for that script
        let script_content = b"#!/bin/bash\necho 1\n";
        let script_sha = sha256_hex(script_content);
        let helper_sha = sha256_hex(&fs::read(pkg.join("bin/helper")).unwrap());
        let config_sha = sha256_hex(&fs::read(pkg.join("config/default.json")).unwrap());
        let forged_json = format!(
            "{{\n  \"bin/helper\": {{\"sha256\": \"{helper_sha}\", \"size\": {}}},\n  \"bin/script_pkg\": {{\"sha256\": \"{script_sha}\", \"size\": {}}},\n  \"config/default.json\": {{\"sha256\": \"{config_sha}\", \"size\": 12}}\n}}\n",
            fs::metadata(pkg.join("bin/helper")).unwrap().len(),
            script_content.len()
        );
        let mut permissions = fs::symlink_metadata(pkg.join("bin/script_pkg"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(pkg.join("bin/script_pkg"), permissions).unwrap();
        fs::write(pkg.join("bin/script_pkg"), script_content).unwrap();
        let mut permissions = fs::symlink_metadata(pkg.join("bin/script_pkg"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o500);
        fs::set_permissions(pkg.join("bin/script_pkg"), permissions).unwrap();
        rewrite_controls(
            &pkg,
            &forged_json,
            &["bin/helper", "bin/script_pkg", "config/default.json"],
            &["bin/helper", "bin/script_pkg"],
        );

        // Tree verification passes hash check, but verify rejects because it's not a native executable!
        let verify_err = verify(&pkg).unwrap_err();
        assert!(
            verify_err
                .to_string()
                .contains("not_native_executable: bin/script_pkg"),
            "got {verify_err}"
        );
    }

    #[test]
    fn verify_rejects_when_auxiliary_executable_is_script() {
        let parent = tempfile::tempdir().unwrap();
        let pkg = parent.path().join("aux_script_pkg");
        write_tree(&pkg, "aux_script_pkg");

        // Spawn command is valid native executable
        copy_current_exe(&pkg.join("bin/aux_script_pkg"));
        // Helper executable is also native initially so generate succeeds
        copy_current_exe(&pkg.join("bin/helper"));
        generate(&pkg, &["bin/aux_script_pkg".into(), "bin/helper".into()]).unwrap();
        assert!(verify(&pkg).is_ok());

        // Now forge bin/helper to be a script while preserving tree hash integrity
        let script_content = b"#!/bin/sh\necho helper\n";
        let script_sha = sha256_hex(script_content);
        let main_sha = sha256_hex(&fs::read(pkg.join("bin/aux_script_pkg")).unwrap());
        let config_sha = sha256_hex(&fs::read(pkg.join("config/default.json")).unwrap());
        let forged_json = format!(
            "{{\n  \"bin/aux_script_pkg\": {{\"sha256\": \"{main_sha}\", \"size\": {}}},\n  \"bin/helper\": {{\"sha256\": \"{script_sha}\", \"size\": {}}},\n  \"config/default.json\": {{\"sha256\": \"{config_sha}\", \"size\": 12}}\n}}\n",
            fs::metadata(pkg.join("bin/aux_script_pkg")).unwrap().len(),
            script_content.len()
        );
        let mut permissions = fs::symlink_metadata(pkg.join("bin/helper"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(pkg.join("bin/helper"), permissions).unwrap();
        fs::write(pkg.join("bin/helper"), script_content).unwrap();
        let mut permissions = fs::symlink_metadata(pkg.join("bin/helper"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o500);
        fs::set_permissions(pkg.join("bin/helper"), permissions).unwrap();
        rewrite_controls(
            &pkg,
            &forged_json,
            &["bin/aux_script_pkg", "bin/helper", "config/default.json"],
            &["bin/aux_script_pkg", "bin/helper"],
        );

        // spawn.command is native, but auxiliary executable bin/helper is a script.
        // verify must check ALL executables and reject it!
        let verify_err = verify(&pkg).unwrap_err();
        assert!(
            verify_err
                .to_string()
                .contains("not_native_executable: bin/helper"),
            "got {verify_err}"
        );
    }

    fn rejects_native_bytes(bytes: &[u8]) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("executable");
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            check_native_executable(&path, "executable"),
            Err(IntegrityError::NotNativeExecutable { .. })
        ));
    }

    #[test]
    fn native_macho_architectures_must_match() {
        check_architectures("macos");
    }

    #[test]
    fn native_elf_architectures_must_match() {
        check_architectures("linux");
    }

    #[test]
    fn native_pe_architectures_must_match() {
        check_architectures("windows");
    }

    fn check_architectures(platform_filter: &str) {
        for (platform, offset, machines) in [
            (
                "macos",
                4,
                [
                    object::macho::CPU_TYPE_ARM64,
                    object::macho::CPU_TYPE_X86_64,
                ],
            ),
            (
                "linux",
                18,
                [
                    u32::from(object::elf::EM_AARCH64),
                    u32::from(object::elf::EM_X86_64),
                ],
            ),
            (
                "windows",
                68,
                [
                    u32::from(object::pe::IMAGE_FILE_MACHINE_ARM64),
                    u32::from(object::pe::IMAGE_FILE_MACHINE_AMD64),
                ],
            ),
        ] {
            if platform != platform_filter {
                continue;
            }
            let mut bytes = match platform {
                "macos" => macho_with_entry(object::macho::LC_MAIN),
                "linux" => {
                    let mut bytes = vec![0; 64];
                    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
                    bytes[16..18].copy_from_slice(&object::elf::ET_EXEC.to_le_bytes());
                    bytes[24..32].copy_from_slice(&4096u64.to_le_bytes());
                    bytes
                }
                _ => {
                    let mut bytes = vec![0; 328];
                    bytes[..2].copy_from_slice(b"MZ");
                    bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
                    bytes[64..68].copy_from_slice(b"PE\0\0");
                    bytes[84..86].copy_from_slice(&240u16.to_le_bytes());
                    bytes[86..88]
                        .copy_from_slice(&object::pe::IMAGE_FILE_EXECUTABLE_IMAGE.to_le_bytes());
                    bytes[88..90]
                        .copy_from_slice(&object::pe::IMAGE_NT_OPTIONAL_HDR64_MAGIC.to_le_bytes());
                    bytes
                }
            };
            for (index, arch) in ["aarch64", "x86_64"].iter().enumerate() {
                for machine in [machines[index], machines[1 - index], 0] {
                    let width = if platform == "macos" { 4 } else { 2 };
                    bytes[offset..offset + width].copy_from_slice(&machine.to_le_bytes()[..width]);
                    assert_eq!(
                        native_executable(&bytes, platform, arch),
                        machine == machines[index],
                        "{platform} {arch} {machine}"
                    );
                    assert!(!native_executable(&bytes, platform, "unsupported"));
                }
            }
        }
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn generate_for_arch_accepts_x86_64_but_verify_requires_host() {
        let dir = tempfile::tempdir().unwrap();
        write_tree(dir.path(), "cos72");
        fs::write(
            dir.path().join("bin/cos72"),
            macho_with_entry(object::macho::LC_MAIN),
        )
        .unwrap();
        let executables = ["bin/cos72".into()];
        assert!(matches!(
            generate(dir.path(), &executables),
            Err(IntegrityError::NotNativeExecutable { .. })
        ));
        generate_for_arch(dir.path(), &executables, "x86_64").unwrap();
        assert!(matches!(
            verify(dir.path()),
            Err(IntegrityError::NotNativeExecutable { .. })
        ));
        // The release script's explicit cross-arch self-check accepts it ...
        verify_for_arch(dir.path(), "x86_64").unwrap();
        // ... but the daemon's entry (discovery / recheck) has no arch
        // parameter and keeps refusing a foreign-arch package.
        let manifest = DomainOsManifest::from_yaml(
            &fs::read_to_string(dir.path().join(DOMAIN_OS_YML)).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            verify_if_present(dir.path(), &manifest, &CancellationToken::new()),
            Err(IntegrityError::NotNativeExecutable { .. })
        ));
    }

    #[test]
    fn generate_for_arch_rejects_unknown_arch_before_reading_package() {
        let dir = tempfile::tempdir().unwrap();
        for arch in ["unknown", "arm64", "", "X86_64"] {
            assert!(
                matches!(generate_for_arch(dir.path(), &[], arch), Err(IntegrityError::ManifestInvalid { detail }) if detail == "unsupported target architecture (expected aarch64 or x86_64)")
            );
        }
    }

    /// #1751 N1. `verify --arch` must refuse an unknown architecture before it
    /// reads the package. Deleting `check_target_arch(arch)?` in
    /// [`verify_for_arch`] turns this into a missing-manifest error (the
    /// directory is empty) and the assertion goes red. The generate-side twin
    /// above does not cover this entry.
    #[test]
    fn verify_for_arch_rejects_unknown_arch_before_reading_package() {
        let dir = tempfile::tempdir().unwrap();
        for arch in ["unknown", "arm64", "", "X86_64"] {
            assert!(
                matches!(verify_for_arch(dir.path(), arch), Err(IntegrityError::ManifestInvalid { detail }) if detail == "unsupported target architecture (expected aarch64 or x86_64)"),
                "{arch}"
            );
        }
    }

    fn macho_with_entry(command: u32) -> Vec<u8> {
        let mut bytes = macho_header(1, 24);
        bytes.extend_from_slice(&command.to_le_bytes());
        bytes.extend_from_slice(&24u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes
    }

    #[test]
    fn native_macho_requires_entry_command() {
        assert!(!native_executable(&macho_header(0, 0), "macos", "x86_64"));
        let mut segment = macho_header(1, 72);
        segment.extend_from_slice(&object::macho::LC_SEGMENT_64.to_le_bytes());
        segment.extend_from_slice(&72u32.to_le_bytes());
        segment.resize(104, 0);
        assert!(!native_executable(&segment, "macos", "x86_64"));
        for command in [object::macho::LC_MAIN, object::macho::LC_UNIXTHREAD] {
            assert!(native_executable(
                &macho_with_entry(command),
                "macos",
                "x86_64"
            ));
            // The same command with only its 8-byte header and no body.
            let mut empty = macho_header(1, 8);
            empty.extend_from_slice(&command.to_le_bytes());
            empty.extend_from_slice(&8u32.to_le_bytes());
            assert!(!native_executable(&empty, "macos", "x86_64"));
        }
    }

    #[test]
    fn native_fat_rejects_mislabeled_slice() {
        let bytes = macho_with_entry(object::macho::LC_MAIN);
        let fat = fat32_with(&[object::macho::CPU_TYPE_ARM64], &bytes);
        assert!(!native_reader(
            &mut io::Cursor::new(fat),
            "macos",
            "aarch64"
        ));
        let fat = fat32_with(&[object::macho::CPU_TYPE_X86_64], &bytes);
        assert!(native_reader(
            &mut io::Cursor::new(fat.clone()),
            "macos",
            "x86_64"
        ));
        assert!(!native_reader(
            &mut io::Cursor::new(fat),
            "macos",
            "unsupported"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn native_open_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("executable");
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &path).unwrap();
        assert!(matches!(
            check_native_executable(&path, "link"),
            Err(IntegrityError::CheckFailed { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn native_open_rejects_fifo_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("executable");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let fifo = path.clone();
        let worker = std::thread::spawn(move || {
            tx.send(check_native_executable(&fifo, "fifo")).unwrap();
        });
        let result = rx.recv_timeout(std::time::Duration::from_secs(2));
        // Unblock a regressed blocking open so the test can fail without hanging.
        let unblock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        drop(unblock);
        worker.join().unwrap();
        assert!(
            matches!(result.unwrap(), Err(IntegrityError::CheckFailed { detail }) if detail.contains("not a regular file"))
        );
    }

    #[test]
    fn native_current_exe_is_accepted() {
        check_native_executable(&std::env::current_exe().unwrap(), "self").unwrap();
    }

    #[test]
    fn native_scripts_and_random_bytes_are_rejected() {
        rejects_native_bytes(b"#!/bin/sh\necho hello\n");
        rejects_native_bytes(b"random bytes, not an executable");
    }

    #[test]
    fn native_truncated_executables_are_rejected() {
        let bytes = fs::read(std::env::current_exe().unwrap()).unwrap();
        for len in [16, 32] {
            rejects_native_bytes(&bytes[..len]);
        }
        // A 64-byte prefix is a complete ELF64 header, which is all the Linux
        // check reads; on macOS the load commands it also requires are cut off.
        #[cfg(target_os = "macos")]
        rejects_native_bytes(&bytes[..64]);
    }

    #[test]
    fn native_magic_only_is_rejected() {
        for magic in [
            &b"\xcf\xfa\xed\xfe"[..],
            &b"\xce\xfa\xed\xfe"[..],
            &b"\xca\xfe\xba\xbe"[..],
            &b"\xca\xfe\xba\xbf"[..],
            &b"\x7fELF"[..],
            &b"MZ"[..],
        ] {
            rejects_native_bytes(magic);
        }
    }

    #[test]
    fn native_elf_kind_and_entry_are_checked() {
        // Minimal ELF64, no sections or program headers, with a nonzero entry.
        let mut elf = vec![0; 64];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&object::elf::EM_X86_64.to_le_bytes());
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());
        elf[24..32].copy_from_slice(&4096u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());
        assert!(native_executable(elf.as_slice(), "linux", "x86_64"));
        elf[16..18].copy_from_slice(&3u16.to_le_bytes());
        assert!(native_executable(elf.as_slice(), "linux", "x86_64"));
        elf[24..32].fill(0);
        assert!(!native_executable(elf.as_slice(), "linux", "x86_64"));
        elf[24..32].copy_from_slice(&4096u64.to_le_bytes());
        elf[16..18].copy_from_slice(&1u16.to_le_bytes());
        assert!(!native_executable(elf.as_slice(), "linux", "x86_64"));
        elf[4] = 0;
        assert!(!native_executable(elf.as_slice(), "linux", "x86_64"));
    }

    #[test]
    fn native_pe_dll_and_header_window_are_checked() {
        for offset in [8192, HEADER_WINDOW - 128, HEADER_WINDOW + 8] {
            let mut pe = vec![0; offset + 264];
            pe[..2].copy_from_slice(b"MZ");
            pe[60..64].copy_from_slice(&(offset as u32).to_le_bytes());
            pe[offset..offset + 4].copy_from_slice(b"PE\0\0");
            pe[offset + 4..offset + 6]
                .copy_from_slice(&object::pe::IMAGE_FILE_MACHINE_AMD64.to_le_bytes());
            pe[offset + 20..offset + 22].copy_from_slice(&240u16.to_le_bytes());
            pe[offset + 22..offset + 24].copy_from_slice(&2u16.to_le_bytes());
            pe[offset + 24..offset + 26].copy_from_slice(&0x20bu16.to_le_bytes());
            assert_eq!(native_executable(&pe, "windows", "x86_64"), offset == 8192);
            pe[offset + 22..offset + 24].copy_from_slice(&0x2002u16.to_le_bytes());
            assert!(!native_executable(&pe, "windows", "x86_64"));
            pe[offset + 22..offset + 24].fill(0);
            assert!(!native_executable(&pe, "windows", "x86_64"));
        }
    }

    fn macho_header(commands: u32, size: u32) -> Vec<u8> {
        let mut bytes = vec![0; 32];
        bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
        bytes[4..8].copy_from_slice(&object::macho::CPU_TYPE_X86_64.to_le_bytes());
        bytes[12..16].copy_from_slice(&object::macho::MH_EXECUTE.to_le_bytes());
        bytes[16..20].copy_from_slice(&commands.to_le_bytes());
        bytes[20..24].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    #[test]
    fn native_macho_commands_must_fit_window() {
        let mut bytes = macho_header(1, HEADER_WINDOW as u32);
        bytes.resize(HEADER_WINDOW + 32, 0);
        bytes[32..36].copy_from_slice(&object::macho::LC_MAIN.to_le_bytes());
        bytes[36..40].copy_from_slice(&24u32.to_le_bytes());
        assert!(!native_executable(&bytes, "macos", "x86_64"));
        bytes[20..24].copy_from_slice(&((HEADER_WINDOW - 32) as u32).to_le_bytes());
        assert!(native_executable(&bytes, "macos", "x86_64"));
    }

    #[test]
    fn native_macho_malformed_command_is_rejected() {
        let mut bytes = macho_header(1, 8);
        bytes.extend_from_slice(&[0; 8]);
        assert!(!native_executable(&bytes, "macos", "x86_64"));
    }

    #[test]
    fn native_overlapping_symtabs_read_at_most_two_windows() {
        struct CountingReader {
            inner: io::Cursor<Vec<u8>>,
            read: usize,
            interrupted: bool,
        }
        impl Read for CountingReader {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let limit = buf.len().min(8191);
                let n = self.inner.read(&mut buf[..limit])?;
                self.read += n;
                Ok(n)
            }
        }
        impl Seek for CountingReader {
            fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
                self.inner.seek(pos)
            }
        }
        let mut bytes = macho_header(129, 129 * 24);
        for _ in 0..128 {
            for field in [
                object::macho::LC_SYMTAB,
                24,
                HEADER_WINDOW as u32,
                65536,
                HEADER_WINDOW as u32,
                1048576,
            ] {
                bytes.extend_from_slice(&field.to_le_bytes());
            }
        }
        bytes.extend_from_slice(&object::macho::LC_MAIN.to_le_bytes());
        bytes.extend_from_slice(&24u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes.resize(4 * HEADER_WINDOW, 0);
        let host = object::macho::CPU_TYPE_X86_64;
        for data in [bytes.clone(), fat32_with(&[host], &bytes)] {
            let mut reader = CountingReader {
                inner: io::Cursor::new(data),
                read: 0,
                interrupted: false,
            };
            assert!(native_reader(&mut reader, "macos", "x86_64"));
            assert!(
                reader.read <= 2 * HEADER_WINDOW,
                "read {} bytes",
                reader.read
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_non_executable_and_foreign_format_are_rejected() {
        let mut bytes = fs::read(std::env::current_exe().unwrap()).unwrap();
        for platform in ["linux", "windows", "unknown"] {
            assert!(!native_executable(bytes.as_slice(), platform, "x86_64"));
        }
        // Change the real Mach-O's filetype to MH_DYLIB.
        bytes[12..16].copy_from_slice(&6u32.to_le_bytes());
        rejects_native_bytes(&bytes);
    }

    /// A fat32 container whose every slice points at the same embedded binary.
    fn fat32_with(cputypes: &[u32], binary: &[u8]) -> Vec<u8> {
        let mut fat = Vec::new();
        fat.extend_from_slice(&0xcafebabeu32.to_be_bytes());
        fat.extend_from_slice(&(cputypes.len() as u32).to_be_bytes());
        for cputype in cputypes {
            fat.extend_from_slice(&cputype.to_be_bytes());
            fat.extend_from_slice(&0u32.to_be_bytes());
            fat.extend_from_slice(&4096u32.to_be_bytes());
            fat.extend_from_slice(&(binary.len() as u32).to_be_bytes());
            fat.extend_from_slice(&12u32.to_be_bytes());
        }
        fat.resize(4096, 0);
        fat.extend_from_slice(binary);
        fat
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_fat_parses_only_the_host_slice_and_caps_slice_count() {
        let bytes = fs::read(std::env::current_exe().unwrap()).unwrap();
        let host = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        // CPU_TYPE_X86_64 and CPU_TYPE_ARM64: whichever this machine is not.
        let foreign = if host == object::macho::CPU_TYPE_X86_64 {
            object::macho::CPU_TYPE_ARM64
        } else {
            object::macho::CPU_TYPE_X86_64
        };
        rejects_native_bytes(&fat32_with(&[foreign], &bytes));
        let accepted = fat32_with(&[foreign, host], &bytes);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fat");
        fs::write(&path, &accepted).unwrap();
        check_native_executable(&path, "fat").unwrap();
        let many: Vec<u32> = std::iter::repeat_n(host, MAX_FAT_ARCHES + 1).collect();
        rejects_native_bytes(&fat32_with(&many, &bytes));
    }

    // Wraps the current test binary in a Mach-O fat container: only meaningful
    // where that binary is itself Mach-O.
    #[cfg(target_os = "macos")]
    #[test]
    fn native_fat_slices_are_parsed() {
        let bytes = fs::read(std::env::current_exe().unwrap()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fat");
        if std::process::Command::new("which")
            .arg("lipo")
            .output()
            .unwrap()
            .status
            .success()
        {
            assert!(
                std::process::Command::new("lipo")
                    .arg("-create")
                    .arg(std::env::current_exe().unwrap())
                    .arg("-output")
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            check_native_executable(&path, "fat").unwrap();
        } else {
            eprintln!("SKIP lipo: not installed; using constructed fat containers");
        }
        for wide in [false, true] {
            let mut fat = Vec::new();
            fat.extend_from_slice(
                &(if wide { 0xcafebabfu32 } else { 0xcafebabeu32 }).to_be_bytes(),
            );
            fat.extend_from_slice(&1u32.to_be_bytes());
            for field in [&bytes[4..8], &bytes[8..12]] {
                fat.extend_from_slice(&u32::from_le_bytes(field.try_into().unwrap()).to_be_bytes());
            }
            if wide {
                fat.extend_from_slice(&4096u64.to_be_bytes());
                fat.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            } else {
                fat.extend_from_slice(&4096u32.to_be_bytes());
                fat.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            }
            fat.extend_from_slice(&12u32.to_be_bytes());
            if wide {
                fat.extend_from_slice(&0u32.to_be_bytes());
            }
            fat.resize(4096, 0);
            fat.extend_from_slice(&bytes);
            fs::write(&path, &fat).unwrap();
            check_native_executable(&path, "fat").unwrap();
            let mut oversized = fat.clone();
            if wide {
                oversized[24..32].copy_from_slice(&u64::MAX.to_be_bytes());
            } else {
                oversized[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
            }
            rejects_native_bytes(&oversized);
            fat[4108..4112].copy_from_slice(&6u32.to_le_bytes());
            rejects_native_bytes(&fat);
            fat.truncate(4160);
            rejects_native_bytes(&fat);
        }
    }
}
