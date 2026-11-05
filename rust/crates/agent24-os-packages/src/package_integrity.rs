//! `agent24 package-integrity` — generate and verify an OS package file manifest.
//!
//! Signature checks are intentionally absent. External CI signs executables
//! before `generate`. PR-3 is what starts refusing packages that omit the
//! manifest.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use agent24_components::integrity::{
    self, DOMAIN_OS_YML, FILES_JSON_NAME, IntegrityError, IntegritySection,
};
use agent24_domain::DomainOsManifest;
use tokio_util::sync::CancellationToken;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Read a package that may or may not declare integrity.
///
/// Absence is success. Presence runs the full shared check and refuses a
/// broken package. This is the PR-1 hook; PR-3 replaces it with a required check.
pub fn verify_if_present(dir: &Path, manifest: &DomainOsManifest) -> Result<(), IntegrityError> {
    let Some(declared) = manifest.integrity() else {
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
    )
}

/// Strict read-only check used by `package-integrity verify`.
pub fn verify(root: &Path) -> Result<(), IntegrityError> {
    ensure_real_directory(root)?;
    let bytes = read_regular(&root.join(DOMAIN_OS_YML), DOMAIN_OS_YML)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| IntegrityError::ManifestInvalid {
        detail: format!("{DOMAIN_OS_YML} is not valid UTF-8"),
    })?;
    let manifest =
        DomainOsManifest::from_yaml(text).map_err(|err| IntegrityError::ManifestInvalid {
            detail: err.to_string(),
        })?;
    match manifest.integrity() {
        None => Err(IntegrityError::Missing),
        Some(_) => verify_if_present(root, &manifest),
    }
}

/// Normalize `root` and write `files.json` plus the `integrity` section.
///
/// The directory is the release staging area. There is no exclude list.
/// Rewriting is atomic per file; a failed YAML publish rolls `files.json` back.
/// Two successful runs on an unchanged tree produce the same bytes.
pub fn generate(root: &Path, executables: &[String]) -> Result<(), IntegrityError> {
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
    let original = read_regular(&yaml_path, DOMAIN_OS_YML)?;
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
    integrity::normalize_modes(
        root,
        &payload,
        executables,
        &[DOMAIN_OS_YML, FILES_JSON_NAME],
    )
    .map_err(integrity::map_tree_error)?;

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
    let previous_json = fs::read(&json_path).ok();
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
) -> Result<(), IntegrityError> {
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
    let bytes = fs::read(&path).map_err(|err| IntegrityError::CheckFailed {
        detail: format!("{FILES_JSON_NAME}: {err}"),
    })?;
    let expected = integrity::verify_files_json(&bytes, section)?;
    integrity::verify_tree(
        dir,
        &expected,
        &section.executables,
        true,
        &[DOMAIN_OS_YML, FILES_JSON_NAME],
        &CancellationToken::new(),
    )
    .map_err(integrity::map_tree_error)
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

fn read_regular(path: &Path, label: &str) -> Result<Vec<u8>, IntegrityError> {
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
    fs::read(path).map_err(|err| IntegrityError::CheckFailed {
        detail: format!("{label}: {err}"),
    })
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

    fn write_tree(dir: &Path, name: &str) {
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::create_dir_all(dir.join("config")).unwrap();
        fs::write(dir.join(DOMAIN_OS_YML), yaml_for(name)).unwrap();
        fs::write(dir.join("bin").join(name), b"payload-v1").unwrap();
        fs::write(dir.join("bin/helper"), b"helper-v1").unwrap();
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
        let scan = crate::discovery::scan(parent);
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
                let blown = current.replacen(
                    "\"size\": 10",
                    &format!("\"size\": {}", MAX_PAYLOAD_BYTES + 1),
                    1,
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

        let first = crate::discovery::scan(parent.path());
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
        crate::discovery::recheck(&old, &old_found.digest).unwrap();
        crate::discovery::recheck(&new_package, &new_found.digest).unwrap();
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

        let second = crate::discovery::scan(parent.path());
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
        let recheck = crate::discovery::recheck(&new_package, &new_found.digest).unwrap_err();
        assert!(recheck.contains("hash_mismatch: bin/newmod"), "{recheck}");
        crate::discovery::recheck(&old, &old_found.digest).unwrap();
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
}
