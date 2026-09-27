//! `~/.agent24/attached.json` — the A3 attached-module registry record store
//! (`docs/design/A3-ATTACHED-MODULE.md` §3.2–§3.5, PR slice A3-2a).
//!
//! **This is the STORAGE layer only.** A3-2a persists what the REST endpoints
//! in [`crate::attached_routes`] add/rotate/revoke: the manifest text, its
//! digest, and the sha256 of the handshake token — never the plaintext
//! (§3.3, judgement C1). It wires no listening socket, no handshake, no live
//! `Generation`/`AttachSlot`: those need types from A3-1 (`Generation::attached()`,
//! `AttachSlot`, `accept_attached`) that do not exist on `agent24-os-proto` on
//! this branch yet — that wiring is A3-2b (design §10). Every record this
//! module can produce is therefore reported `attach_status: "detached"` until
//! A3-2b adds a real registry on top of it.
//!
//! # Why this does not parse with `agent24_domain::DomainOsManifest`
//!
//! That type's `RawManifest::impl_kind` is a *closed* two-variant enum
//! (`InProcessCrate` | `OutOfProcessProvider`, `deny_unknown_fields` on the
//! whole struct) — a real attached manifest's `impl_kind: attached_process`
//! (§3.1, added by A3-1, a separate in-flight branch) does not parse against
//! it on this branch. Re-validating the rest of the schema here and then
//! throwing it away the moment A3-1 lands would just be a second copy to keep
//! in sync, so this reads exactly the facts A3-2a's own judgements need —
//! `name`, `model_access`, `kernel_capabilities` — via the same *lenient*
//! reader a module already uses on its own manifest
//! (`agent24_os_proto::manifest::facts_from_yaml`, ME4-S3 §4.4), plus a
//! `model_access` lookup that reader does not carry. Full manifest validation
//! (the `impl_kind`/`host_commands`/`spawn` shape) is A3-1/A3-2b's job once
//! the domain crate knows about `attached_process`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use agent24_protocol::{AttachedAddResponse, AttachedView};

/// `sha256:` + lowercase hex of the exact submitted manifest bytes — the same
/// format (and the same function) a module's handshake and the kernel's
/// package discovery already use (ME4-S3 §4.4).
pub fn manifest_digest(bytes: &[u8]) -> String {
    agent24_os_proto::manifest::manifest_digest(bytes)
}

/// The three-ish facts A3-2a needs out of an attached manifest — see the
/// module doc for why this is not `agent24_domain::DomainOsManifest`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestFacts {
    name: String,
    model_access: agent24_domain::ModelAccess,
    capabilities: Vec<agent24_domain::Capability>,
}

/// Lenient: only the field A3-2a needs beyond
/// `agent24_os_proto::manifest::ManifestFacts`. No `deny_unknown_fields` — the
/// full schema is validated elsewhere (see the module doc); this is a read.
#[derive(Deserialize)]
struct RawModelAccess {
    #[serde(default)]
    model_access: Option<String>,
}

fn parse_model_access(yaml: &str) -> Result<agent24_domain::ModelAccess, String> {
    let raw: RawModelAccess = serde_yaml::from_str(yaml).map_err(|e| e.to_string())?;
    match raw.model_access.as_deref() {
        None => Ok(agent24_domain::ModelAccess::LocalOnly),
        Some(s) => agent24_domain::ModelAccess::parse(s),
    }
}

/// Parse and do the A3-2a-level checks: syntactically valid name, not a
/// reserved kernel segment (§3.2 M7), `route_namespace` still derived from
/// `name` (§3.1: "仍按既有规则由 name 推导并校验"), known capability strings,
/// a legal `model_access`. Does NOT check `impl_kind`, `host_commands`,
/// `spawn`, `event_module`, or `data_dir` — see the module doc.
fn parse_facts(yaml: &str) -> Result<ManifestFacts, String> {
    let facts = agent24_os_proto::manifest::facts_from_yaml(yaml)?;
    if !agent24_domain::is_valid_module_name(&facts.name) {
        return Err(format!(
            "invalid module name {:?}: 1-{} chars of [a-z0-9][a-z0-9_-]*, not a reserved device \
             name",
            facts.name,
            agent24_domain::MAX_NAME_BYTES
        ));
    }
    if crate::domain::is_reserved_kernel_segment(&facts.name) {
        return Err(format!(
            "module name {:?} is reserved for the kernel's own routes",
            facts.name
        ));
    }
    let expected_ns = agent24_domain::DomainOsManifest::declared_namespace(&facts.name);
    if facts.route_namespace != expected_ns {
        return Err(format!(
            "route_namespace {:?} must be exactly {:?} (derived from name)",
            facts.route_namespace, expected_ns
        ));
    }
    let mut capabilities = Vec::with_capacity(facts.kernel_capabilities.len());
    for c in &facts.kernel_capabilities {
        capabilities.push(agent24_domain::Capability::parse(c).map_err(|e| e.to_string())?);
    }
    let model_access = parse_model_access(yaml)?;
    Ok(ManifestFacts {
        name: facts.name,
        model_access,
        capabilities,
    })
}

/// §3.5: a request WIDENS privacy relative to `previous` (`None` = first-time
/// registration).
fn is_relax(previous: Option<&ManifestFacts>, new: &ManifestFacts) -> bool {
    match previous {
        None => new.model_access == agent24_domain::ModelAccess::RemoteAllowed,
        Some(prev) => {
            (prev.model_access == agent24_domain::ModelAccess::LocalOnly
                && new.model_access == agent24_domain::ModelAccess::RemoteAllowed)
                || new
                    .capabilities
                    .iter()
                    .any(|c| !prev.capabilities.contains(c))
        }
    }
}

/// One registered attached module (§3.3's `attached.json` shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AttachedRecord {
    /// The exact submitted bytes — re-parsed on the next `add` to recover the
    /// PREVIOUS facts for the §3.5 comparison, and re-hashed rather than
    /// trusting a stored digest.
    manifest_yaml: String,
    manifest_digest: String,
    /// Lowercase hex sha256 of the token. The plaintext is NEVER stored (C1).
    token_sha256: String,
    token_id: String,
    created_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachedStore {
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    modules: BTreeMap<String, AttachedRecord>,
}

fn one() -> u32 {
    1
}

/// `~/.agent24/attached.json`.
pub fn config_path() -> Option<PathBuf> {
    agent24_protocol::state_file::state_dir().map(|d| d.join("attached.json"))
}

/// `~/.agent24/attach/agent24d.sock`, expanded to an absolute path (§4.1).
/// A3-2a never binds this path — it only ever appears as a string in a
/// registration response, so a module knows where to dial once A3-2b starts
/// listening on it.
pub fn socket_path() -> Option<PathBuf> {
    agent24_protocol::state_file::state_dir().map(|d| d.join("attach").join("agent24d.sock"))
}

fn socket_path_string() -> Result<String, String> {
    socket_path()
        .map(|p| p.display().to_string())
        .ok_or_else(|| "HOME not set".to_owned())
}

/// A cross-process exclusive lock over `attached.json`, mirroring
/// `os_config::ConfigLock` (same primitive, same reason: axum handlers run
/// concurrently, and ephemeral daemons are exempt from the singleton lock, so
/// only a file lock covers both a race inside one daemon and two CLI
/// invocations with no daemon running).
struct ConfigLock(std::fs::File);

impl ConfigLock {
    fn acquire(dir: &Path) -> Result<Self, String> {
        use fs2::FileExt;
        let path = dir.join("attached.json.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        file.lock_exclusive()
            .map_err(|e| format!("cannot lock {}: {e}", path.display()))?;
        Ok(Self(file))
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

impl AttachedStore {
    /// A MISSING file is the empty registry (`(无记录)` in §5.1's state
    /// diagram), not an error. A malformed one IS an error — the same
    /// "never silently lose a secret/registration" rule `os_config.rs`
    /// documents for `os.json`.
    fn load(path: &Path) -> Result<Self, String> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).map_err(|e| format!("{} is not valid: {e}", path.display()))
    }

    /// Caller must hold [`ConfigLock`]. Same temp-file-plus-rename shape as
    /// `os_config::OsConfig::write_atomically`, with one addition: the temp
    /// file (and therefore the file it is renamed onto) is created `0600` —
    /// this file holds token hashes, `os.json` holds none (§3.3).
    fn write_atomically(&self, path: &Path, parent: &Path) -> Result<(), String> {
        use std::io::Write;

        let body = serde_json::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize attached.json: {e}"))?;

        let tmp = parent.join(format!("attached.json.writing.{}", std::process::id()));
        if std::fs::symlink_metadata(&tmp).is_ok() {
            tracing::warn!(
                "removing a stale {} left by an earlier interrupted write",
                tmp.display()
            );
            std::fs::remove_file(&tmp)
                .map_err(|e| format!("cannot clear stale {}: {e}", tmp.display()))?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;

        let written = (|| -> std::io::Result<()> {
            f.write_all(body.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()
        })();
        drop(f);
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("cannot write {}: {e}", tmp.display()));
        }

        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("cannot replace {}: {e}", path.display()));
        }
        // Belt-and-braces against a permissive umask: `create_new` above
        // already asked for 0600, but the mode passed to `open` is masked by
        // the process umask before the OS applies it, so a umask like 0022
        // still leaves the file group/world readable unless it is set again
        // explicitly here.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("written, but cannot chmod {}: {e}", path.display()))?;
        }
        #[cfg(unix)]
        {
            let dir = std::fs::File::open(parent).map_err(|e| {
                format!(
                    "written, but cannot open {} to fsync it: {e}",
                    parent.display()
                )
            })?;
            dir.sync_all()
                .map_err(|e| format!("written, but fsync of {} failed: {e}", parent.display()))?;
        }
        Ok(())
    }
}

fn hash_token_hex(token: &str) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(token.as_bytes());
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time: not load-bearing for A3-2a (which never compares a
/// presented token — that is A3-2b's handshake), but cheap insurance against
/// a future caller reaching for `==` on a secret hash.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[allow(dead_code, reason = "exposed for a future A3-2b handshake verifier")]
pub fn token_matches(token: &str, token_sha256_hex: &str) -> bool {
    constant_time_eq(
        hash_token_hex(token).as_bytes(),
        token_sha256_hex.as_bytes(),
    )
}

/// A fresh `tok_<8 hex>` id — a SEPARATE random draw from the token itself
/// (never a truncation of it): this value is shown in `GET /api/v1/attached`
/// and logs, so it must carry no recoverable fragment of the secret.
fn mint_token_id() -> Result<String, String> {
    let raw = agent24_os_proto::launch::mint_token().map_err(|e| e.to_string())?;
    Ok(format!("tok_{}", &raw[..8]))
}

/// What a successful `POST /api/v1/attached` produced (§3.4): a fresh name,
/// or a rotation/re-registration of one already there. The REST layer maps
/// the two onto `201`/`200` respectively; the body shape is identical.
#[derive(Debug, Clone, PartialEq)]
pub enum RegisterOutcome {
    Created(AttachedAddResponse),
    Rotated(AttachedAddResponse),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    InvalidManifest(String),
    /// Already a domain OS this daemon provides some other way (an installed
    /// package or a compiled-in module) — carries the name for the message.
    NameTaken(String),
    RelaxRequiresConfirmation,
    Io(String),
}

/// Register (first time) or rotate/re-register (already present) an attached
/// module (§3.2/§3.4), inside ONE file-lock critical section (§5.2 — A3-2a's
/// slice of it: there is no live generation to revoke yet, so "commit" here
/// means only "the record is updated and durable").
///
/// `name_taken` decides the §3.2 409 case for a BRAND NEW name — it is given
/// the candidate name and answers whether some other kind of domain OS (an
/// installed package, a compiled-in module) already claims it. It is never
/// consulted for a name that is already an attached record: re-adding one is
/// a rotation/re-registration, not a new claim.
pub fn register(
    path: &Path,
    manifest_yaml: &str,
    allow_relax: bool,
    name_taken: impl Fn(&str) -> bool,
) -> Result<RegisterOutcome, RegisterError> {
    let facts = parse_facts(manifest_yaml).map_err(RegisterError::InvalidManifest)?;
    let digest = manifest_digest(manifest_yaml.as_bytes());

    let parent = path
        .parent()
        .ok_or_else(|| RegisterError::Io(format!("{} has no parent directory", path.display())))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| RegisterError::Io(format!("cannot create {}: {e}", parent.display())))?;
    let _guard = ConfigLock::acquire(parent).map_err(RegisterError::Io)?;

    let mut store = AttachedStore::load(path).map_err(RegisterError::Io)?;
    let previous_record = store.modules.get(&facts.name);
    let previous_facts = previous_record
        .map(|r| parse_facts(&r.manifest_yaml))
        .transpose()
        .map_err(|e| {
            RegisterError::Io(format!(
                "the stored manifest for {:?} no longer parses: {e}",
                facts.name
            ))
        })?;

    // §3.2 processing order: validate → name clash → privacy relax → mint →
    // store.
    if previous_record.is_none() && name_taken(&facts.name) {
        return Err(RegisterError::NameTaken(facts.name));
    }
    if is_relax(previous_facts.as_ref(), &facts) && !allow_relax {
        return Err(RegisterError::RelaxRequiresConfirmation);
    }

    let token =
        agent24_os_proto::launch::mint_token().map_err(|e| RegisterError::Io(e.to_string()))?;
    let token_id = mint_token_id().map_err(RegisterError::Io)?;
    let token_sha256 = hash_token_hex(&token);
    let created_at = chrono::Utc::now().to_rfc3339();
    let is_new = previous_record.is_none();

    store.modules.insert(
        facts.name.clone(),
        AttachedRecord {
            manifest_yaml: manifest_yaml.to_owned(),
            manifest_digest: digest.clone(),
            token_sha256,
            token_id: token_id.clone(),
            created_at,
        },
    );
    store
        .write_atomically(path, parent)
        .map_err(RegisterError::Io)?;

    let response = AttachedAddResponse {
        name: facts.name,
        manifest_digest: digest,
        token,
        socket_path: socket_path_string().map_err(RegisterError::Io)?,
        token_id,
    };
    Ok(if is_new {
        RegisterOutcome::Created(response)
    } else {
        RegisterOutcome::Rotated(response)
    })
}

/// `DELETE /api/v1/attached/{name}` (§3.2/§3.4). `Ok(true)`: a record existed
/// and is gone. `Ok(false)`: nothing to do — `404` at the REST layer.
pub fn revoke(path: &Path, name: &str) -> Result<bool, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let _guard = ConfigLock::acquire(parent)?;

    let mut store = AttachedStore::load(path)?;
    let existed = store.modules.remove(name).is_some();
    if existed {
        store.write_atomically(path, parent)?;
    }
    Ok(existed)
}

/// `GET /api/v1/attached` (a deviation from the design doc's §3.2 table —
/// see `docs/design/A3-ATTACHED-MODULE.md` and this PR's own report for why:
/// the augmentation described there for `GET /api/v1/os` needs live
/// generation state (`attach_status`, `generation`) that only exists once
/// A3-2b wires a real registry; a NEW, narrower endpoint here avoids widening
/// `agent24_protocol::DomainOsView`/`os_routes.rs` twice). NEVER includes the
/// token or its hash (§3.2/§3.3 C1).
pub fn list(path: &Path) -> Result<Vec<AttachedView>, String> {
    let store = AttachedStore::load(path)?;
    Ok(store
        .modules
        .into_iter()
        .map(|(name, r)| AttachedView {
            name,
            manifest_digest: r.manifest_digest,
            token_id: r.token_id,
            created_at: r.created_at,
            // A3-2a wires no live generation (module doc); every entry it can
            // produce is `detached` until A3-2b.
            attach_status: "detached".to_owned(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    // macOS parallel-test temp dirs: a per-process counter folded into the
    // directory name, so concurrent `cargo test` threads on this file never
    // collide even though `tempfile::tempdir()` alone would already avoid
    // that — this mirrors the project's stated convention for this file's
    // tests and makes the naming pattern greppable.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    fn tmp() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("a24-attached-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest(name: &str, model_access: Option<&str>, caps: &[&str]) -> String {
        let ma = model_access
            .map(|m| format!("model_access: {m}\n"))
            .unwrap_or_default();
        format!(
            "name: {name}\nversion: \"1\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             impl_kind: attached_process\nkernel_capabilities: [{}]\n{ma}",
            caps.join(", ")
        )
    }

    #[test]
    fn a_first_registration_stores_only_the_token_hash() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", None, &["events", "models"]);
        let outcome = register(&path, &m, false, |_| false).unwrap();
        let RegisterOutcome::Created(resp) = outcome else {
            panic!("expected Created");
        };
        assert_eq!(resp.name, "agentear");
        assert!(resp.manifest_digest.starts_with("sha256:"));
        assert!(resp.token_id.starts_with("tok_"));
        assert!(resp.socket_path.ends_with("attach/agent24d.sock"));

        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            raw.matches(&resp.token).count(),
            0,
            "the plaintext token must never be written to attached.json"
        );
        let hash = hash_token_hex(&resp.token);
        assert_eq!(
            raw.matches(&hash).count(),
            1,
            "the token's sha256 must appear exactly once (positive control)"
        );
        assert!(token_matches(&resp.token, &hash));
        assert!(!token_matches("wrong-token", &hash));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "attached.json must be 0600");
        }
    }

    #[test]
    fn a_get_never_returns_the_token_or_its_hash() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", None, &["events"]);
        let RegisterOutcome::Created(resp) = register(&path, &m, false, |_| false).unwrap() else {
            panic!("expected Created");
        };
        let views = list(&path).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].name, "agentear");
        assert_eq!(views[0].token_id, resp.token_id);
        assert_eq!(views[0].attach_status, "detached");
        // Serialize and grep — proves the WIRE shape, not just the Rust type.
        let json = serde_json::to_string(&views).unwrap();
        assert!(!json.contains(&resp.token));
        assert!(!json.contains("token_sha256"));
        assert!(!json.contains("\"token\""));
        // Catches a leak under the WRONG field name too (e.g. the hash
        // copy-pasted into `token_id` by mistake) — checking only for the
        // literal key names above would miss that, since the VALUE would
        // still be present just filed under a different key.
        let hash = hash_token_hex(&resp.token);
        assert!(
            !json.contains(&hash),
            "the token's hash must not appear anywhere in the list response, under any field"
        );
    }

    #[test]
    fn re_adding_the_same_manifest_rotates_the_token_and_reuses_the_id_slot() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", None, &["events"]);
        let RegisterOutcome::Created(first) = register(&path, &m, false, |_| false).unwrap() else {
            panic!("expected Created");
        };
        let RegisterOutcome::Rotated(second) = register(&path, &m, false, |_| false).unwrap()
        else {
            panic!("expected Rotated");
        };
        assert_ne!(
            first.token, second.token,
            "rotation must mint a fresh token"
        );
        assert_ne!(
            first.token_id, second.token_id,
            "token_id changes on every mint (§3.3)"
        );
        assert_eq!(first.manifest_digest, second.manifest_digest);
        let old_hash = hash_token_hex(&first.token);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            raw.matches(&old_hash).count(),
            0,
            "the old token's hash must not survive a rotation"
        );
    }

    #[test]
    fn a_relaxing_request_without_allow_relax_is_refused_and_leaves_the_record_untouched() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", Some("remote_allowed"), &["events", "models"]);
        let err = register(&path, &m, false, |_| false).unwrap_err();
        assert_eq!(err, RegisterError::RelaxRequiresConfirmation);
        assert!(
            !path.exists(),
            "a refused relax must not create a record at all"
        );
    }

    #[test]
    fn the_same_relaxing_request_with_allow_relax_succeeds() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", Some("remote_allowed"), &["events", "models"]);
        let outcome = register(&path, &m, true, |_| false).unwrap();
        assert!(matches!(outcome, RegisterOutcome::Created(_)));
    }

    #[test]
    fn widening_capabilities_on_reregistration_is_a_relax_without_allow_relax() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let base = manifest("agentear", None, &["events"]);
        register(&path, &base, false, |_| false).unwrap();
        let wider = manifest("agentear", None, &["events", "models"]);
        let err = register(&path, &wider, false, |_| false).unwrap_err();
        assert_eq!(err, RegisterError::RelaxRequiresConfirmation);
        // Untouched: still the narrow capability set from the first add.
        let views = list(&path).unwrap();
        assert_eq!(views[0].manifest_digest, manifest_digest(base.as_bytes()));
    }

    #[test]
    fn narrowing_privacy_on_reregistration_is_not_a_relax() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let wide = manifest("agentear", Some("remote_allowed"), &["events", "models"]);
        register(&path, &wide, true, |_| false).unwrap();
        let narrow = manifest("agentear", Some("local_only"), &["events", "models"]);
        let outcome = register(&path, &narrow, false, |_| false).unwrap();
        assert!(matches!(outcome, RegisterOutcome::Rotated(_)));
    }

    #[test]
    fn a_name_already_claimed_by_another_kind_of_module_is_refused() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("sin90", None, &["events"]);
        let err = register(&path, &m, false, |n| n == "sin90").unwrap_err();
        assert_eq!(err, RegisterError::NameTaken("sin90".to_owned()));
        assert!(!path.exists());
    }

    #[test]
    fn the_reserved_name_attached_is_refused() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("attached", None, &[]);
        let err = register(&path, &m, false, |_| false).unwrap_err();
        assert!(matches!(err, RegisterError::InvalidManifest(_)));
    }

    #[test]
    fn revoke_removes_the_record_and_is_idempotent_about_reporting_it() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", None, &["events"]);
        register(&path, &m, false, |_| false).unwrap();
        assert!(revoke(&path, "agentear").unwrap());
        assert!(list(&path).unwrap().is_empty());
        assert!(
            !revoke(&path, "agentear").unwrap(),
            "nothing left to remove"
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_registry() {
        let dir = tmp();
        let path = dir.join("never-written.json");
        assert!(list(&path).unwrap().is_empty());
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_silent_empty_registry() {
        let dir = tmp();
        let path = dir.join("attached.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(list(&path).is_err());
        assert!(register(&path, &manifest("x", None, &[]), false, |_| false).is_err());
    }

    #[test]
    fn concurrent_registrations_do_not_lose_an_update() {
        // Same property `os_config.rs` proves for `os.json`: without the file
        // lock, two threads both read the empty file and the second `rename`
        // silently discards the first's entry.
        let dir = tmp();
        let path = dir.join("attached.json");
        let names = ["alpha", "beta", "gamma", "delta"];
        std::thread::scope(|scope| {
            for n in names {
                let path = path.clone();
                scope.spawn(move || {
                    let m = manifest(n, None, &["events"]);
                    register(&path, &m, false, |_| false).unwrap();
                });
            }
        });
        let views = list(&path).unwrap();
        let mut got: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
        got.sort_unstable();
        let mut expected = names;
        expected.sort_unstable();
        assert_eq!(got, expected);
    }
}
