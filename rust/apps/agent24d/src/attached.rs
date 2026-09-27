//! `~/.agent24/attached.json` — the A3 attached-module registry record store
//! (`docs/design/A3-ATTACHED-MODULE.md` §3.2–§3.5, PR slice A3-2a; manifest
//! validation and the [`Change`] payload upgraded to full
//! `agent24_domain::DomainOsManifest` validation in A3-2b, §10).
//!
//! **This is the STORAGE layer.** It persists what the REST endpoints in
//! [`crate::attached_routes`] add/rotate/revoke: the manifest text, its
//! digest, and the sha256 of the handshake token — never the plaintext
//! (§3.3, judgement C1). The listening socket, the handshake, and the live
//! `Generation`/`AttachSlot` bookkeeping live in [`crate::attach_registry`]
//! (A3-2b) — this module hands it the facts it needs through [`Change`],
//! passed to `on_commit` while the file lock is still held (§5.2).
//!
//! # Manifest validation
//!
//! A3-1 added `ImplKind::AttachedProcess` and `host_commands` to
//! `agent24_domain::DomainOsManifest`, so this module validates a submitted
//! manifest the same way the kernel validates any other domain-OS manifest
//! (`DomainOsManifest::from_yaml`), plus the one extra rule specific to this
//! endpoint: `impl_kind` must actually BE `attached_process` (a manifest that
//! validates but declares `in_process_crate`/`out_of_process_provider` is a
//! well-formed manifest for the WRONG endpoint, not a valid attached-module
//! registration).

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

/// The privacy-relevant facts `is_relax` compares (§3.5) — a narrower view of
/// a [`agent24_domain::DomainOsManifest`] than the full struct, and the same
/// shape a [`RevokedFacts`] tombstone reconstructs into.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Facts {
    model_access: agent24_domain::ModelAccess,
    capabilities: Vec<agent24_domain::Capability>,
}

impl Facts {
    fn of(manifest: &agent24_domain::DomainOsManifest) -> Self {
        Self {
            model_access: manifest.model_access(),
            capabilities: manifest.kernel_capabilities().to_vec(),
        }
    }
}

/// Parse and validate a submitted `domain-os.yml` as an ATTACHED manifest
/// (§3.1): full `agent24_domain::DomainOsManifest` validation (A3-1 added
/// `ImplKind::AttachedProcess`/`host_commands` to that type), plus the two
/// checks specific to this endpoint that the domain crate cannot make on its
/// own — `impl_kind` must actually be `attached_process` (a well-formed
/// manifest for a DIFFERENT impl_kind is not a valid registration here), and
/// the name must not be a reserved kernel route segment (§3.2 M7; the domain
/// crate's own `RESERVED_KERNEL_SEGMENTS` equivalent lives in `crate::domain`
/// and is exposed via `is_reserved_kernel_segment` for exactly this call
/// site, which has no manifest-mount pass of its own to run it inside).
fn parse_manifest(yaml: &str) -> Result<agent24_domain::DomainOsManifest, String> {
    let manifest = agent24_domain::DomainOsManifest::from_yaml(yaml).map_err(|e| e.to_string())?;
    if manifest.impl_kind() != agent24_domain::ImplKind::AttachedProcess {
        return Err(format!(
            "impl_kind must be attached_process for an attached-module registration, got {:?}",
            manifest.impl_kind()
        ));
    }
    if crate::domain::is_reserved_kernel_segment(manifest.name()) {
        return Err(format!(
            "module name {:?} is reserved for the kernel's own routes",
            manifest.name()
        ));
    }
    Ok(manifest)
}

/// §3.5: a request WIDENS privacy relative to `previous` (`None` = first-time
/// registration).
fn is_relax(previous: Option<&Facts>, new: &Facts) -> bool {
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
    /// A3-2b (§3.2/§5.3): set by `PATCH /api/v1/attached/{name}`. Absent from
    /// any `attached.json` written before this field existed —
    /// `#[serde(default)]` reads that as `false` (never disabled). Review M4:
    /// a register/rotate PRESERVES this from the previous record (see
    /// `register`'s `previous_disabled`) — only `DELETE` (which drops the
    /// record entirely) or an explicit `PATCH ... {"enabled":true}` clears
    /// it. A brand-new name (no previous record) always starts `false`.
    #[serde(default)]
    disabled: bool,
}

/// A revoked module's LAST facts (§3.5 M2 / this PR's judgement): kept just
/// long enough to stop "revoke, then re-add under the same name" from
/// dodging the privacy-relax confirmation. Without this, a re-add of a
/// revoked name falls into `is_relax`'s `previous == None` (first-time)
/// branch, which only asks for confirmation on `remote_allowed` — a widened
/// CAPABILITY set sails through unconfirmed even though, for that name, it
/// is exactly as much a relax as a rotation would be. Cleared the instant a
/// new registration for the name actually commits.
///
/// Stored as strings, not `agent24_domain::ModelAccess`/`Capability`
/// directly: neither derives `Deserialize` (deliberately — see their doc
/// comments), so this mirrors the same string-in/parse-back shape the
/// manifest YAML itself uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RevokedFacts {
    model_access: String,
    capabilities: Vec<String>,
}

impl RevokedFacts {
    fn capture(facts: &Facts) -> Self {
        Self {
            model_access: facts.model_access.as_str().to_owned(),
            capabilities: facts
                .capabilities
                .iter()
                .map(|c| c.as_str().to_owned())
                .collect(),
        }
    }

    /// Reconstruct comparable [`Facts`] for `is_relax`.
    fn into_facts(self) -> Result<Facts, String> {
        let model_access = agent24_domain::ModelAccess::parse(&self.model_access)?;
        let mut capabilities = Vec::with_capacity(self.capabilities.len());
        for c in &self.capabilities {
            capabilities.push(agent24_domain::Capability::parse(c).map_err(|e| e.to_string())?);
        }
        Ok(Facts {
            model_access,
            capabilities,
        })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachedStore {
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    modules: BTreeMap<String, AttachedRecord>,
    /// Tombstones left by `revoke` — see [`RevokedFacts`]. Absent from any
    /// `attached.json` written before this change; `#[serde(default)]` reads
    /// that as empty rather than an error.
    #[serde(default)]
    revoked: BTreeMap<String, RevokedFacts>,
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
///
/// `Rotated` carries `digest_changed`: whether the resubmitted manifest's
/// bytes differ from what was already on file. A rotation always mints a
/// fresh token (§3.3) even when the manifest is byte-identical (that is the
/// whole point of a bare re-add being a supported way to rotate a
/// compromised token), so this is the only way a caller can tell "same
/// manifest, new token" apart from "the manifest itself changed too".
#[derive(Debug, Clone, PartialEq)]
pub enum RegisterOutcome {
    Created(AttachedAddResponse),
    Rotated {
        response: AttachedAddResponse,
        digest_changed: bool,
    },
}

/// What `register`/`revoke` are about to commit to `attached.json`, handed to
/// `on_commit` while the file lock is STILL HELD — the hook [`crate::attach_registry`]
/// (A3-2b) uses to revoke a module's current live generation in the SAME
/// critical section as the record update/removal (design §3.4/§5.2: 改记录、
/// 落盘、撤销现役代 must not be splittable by a concurrent request landing in
/// between).
///
/// `Registered` carries everything the registry needs to install (or refresh)
/// its in-memory entry without a second read of `attached.json` from inside
/// the callback: the validated manifest (§3.4 — rebuilding `Grants`/`Offer`/
/// `ModelGrant` needs it), the digest, and the token facts a later handshake's
/// `AttachRegistry::commit` re-checks (§4.3 ②).
#[derive(Debug, Clone, Copy)]
pub enum Change<'a> {
    /// A name was created or rotated. `rotated` distinguishes the two the
    /// same way [`RegisterOutcome`] does.
    Registered {
        name: &'a str,
        rotated: bool,
        manifest: &'a agent24_domain::DomainOsManifest,
        manifest_digest: &'a str,
        token_sha256_hex: &'a str,
        token_id: &'a str,
        /// Review M4: the record's `disabled` flag AFTER this register/rotate
        /// committed — i.e. `previous_disabled` (register never flips it) —
        /// so the registry mirrors the same "a rotation preserves disable"
        /// rule the on-disk store now follows, instead of assuming `false`.
        disabled: bool,
    },
    /// A name's record was removed.
    Revoked { name: &'a str },
    /// A3-2b: `PATCH /api/v1/attached/{name}` toggled `disabled`. `disabled:
    /// true` must revoke any live generation in the SAME critical section as
    /// the flag flip (§5.3), same reasoning as `Revoked` above.
    Disabled { name: &'a str, disabled: bool },
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
///
/// `on_commit` runs exactly once, still inside the file lock, right after the
/// updated store is durably written — see [`Change`]'s doc comment. It never
/// runs on any error path (nothing was committed).
pub fn register(
    path: &Path,
    manifest_yaml: &str,
    allow_relax: bool,
    name_taken: impl Fn(&str) -> bool,
    on_commit: impl FnOnce(&Change),
) -> Result<RegisterOutcome, RegisterError> {
    let manifest = parse_manifest(manifest_yaml).map_err(RegisterError::InvalidManifest)?;
    let name = manifest.name().to_owned();
    let facts = Facts::of(&manifest);
    let digest = manifest_digest(manifest_yaml.as_bytes());

    let parent = path
        .parent()
        .ok_or_else(|| RegisterError::Io(format!("{} has no parent directory", path.display())))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| RegisterError::Io(format!("cannot create {}: {e}", parent.display())))?;
    let _guard = ConfigLock::acquire(parent).map_err(RegisterError::Io)?;

    let mut store = AttachedStore::load(path).map_err(RegisterError::Io)?;
    let previous_record = store.modules.get(&name);
    let previous_digest = previous_record.map(|r| r.manifest_digest.clone());
    // Review M4: a rotation/re-registration must NOT clear a user's `disable`
    // — an AgentEar auto-update that re-runs `attach add` (§5.6's automatic
    // digest-mismatch rotation, or a plain token refresh) would otherwise
    // silently undo a `PATCH /api/v1/attached/{name} {"enabled":false}` the
    // user made in between. `disabled` now survives register/rotate; it is
    // cleared only by `DELETE` (a genuinely fresh registration afterwards has
    // no record to inherit from) or an explicit `PATCH ... {"enabled":true}`.
    let previous_disabled = previous_record.is_some_and(|r| r.disabled);
    let previous_facts = match previous_record {
        Some(r) => Some(Facts::of(&parse_manifest(&r.manifest_yaml).map_err(
            |e| {
                RegisterError::Io(format!(
                    "the stored manifest for {name:?} no longer parses: {e}"
                ))
            },
        )?)),
        // M2: no ACTIVE record, but a tombstone left by an earlier `revoke`
        // means this name is not really "first-time" — compare against what
        // it had before, exactly like a rotation, so revoke-then-re-add
        // cannot dodge the relax confirmation a plain rotation would need
        // (§3.5). No tombstone at all is the genuine first-time case.
        None => match store.revoked.get(&name) {
            Some(tombstone) => Some(tombstone.clone().into_facts().map_err(|e| {
                RegisterError::Io(format!(
                    "the revoked-record tombstone for {name:?} no longer parses: {e}"
                ))
            })?),
            None => None,
        },
    };

    // §3.2 processing order: validate → name clash → privacy relax → mint →
    // store.
    if previous_record.is_none() && name_taken(&name) {
        return Err(RegisterError::NameTaken(name));
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
        name.clone(),
        AttachedRecord {
            manifest_yaml: manifest_yaml.to_owned(),
            manifest_digest: digest.clone(),
            token_sha256: token_sha256.clone(),
            token_id: token_id.clone(),
            created_at,
            disabled: previous_disabled,
        },
    );
    // M2: a successful registration clears any tombstone for this name — it
    // has just been re-confirmed (or was never relaxing in the first place),
    // so nothing is left for a FUTURE re-add to compare against.
    store.revoked.remove(&name);
    store
        .write_atomically(path, parent)
        .map_err(RegisterError::Io)?;
    on_commit(&Change::Registered {
        name: &name,
        rotated: !is_new,
        manifest: &manifest,
        manifest_digest: &digest,
        token_sha256_hex: &token_sha256,
        token_id: &token_id,
        disabled: previous_disabled,
    });

    let response = AttachedAddResponse {
        name,
        manifest_digest: digest.clone(),
        token,
        socket_path: socket_path_string().map_err(RegisterError::Io)?,
        token_id,
    };
    Ok(if is_new {
        RegisterOutcome::Created(response)
    } else {
        RegisterOutcome::Rotated {
            digest_changed: previous_digest.as_deref() != Some(digest.as_str()),
            response,
        }
    })
}

/// `DELETE /api/v1/attached/{name}` (§3.2/§3.4). `Ok(true)`: a record existed
/// and is gone. `Ok(false)`: nothing to do — `404` at the REST layer.
///
/// `on_commit` runs exactly once, still inside the file lock, right after the
/// removal is durably written — see [`Change`]'s doc comment. It never runs
/// when there was nothing to remove.
pub fn revoke(path: &Path, name: &str, on_commit: impl FnOnce(&Change)) -> Result<bool, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let _guard = ConfigLock::acquire(parent)?;

    let mut store = AttachedStore::load(path)?;
    let removed = store.modules.remove(name);
    let existed = removed.is_some();
    if let Some(record) = removed {
        // M2: leave a tombstone of what this name's privacy/capabilities
        // WERE, so a later re-add under the same name is judged by the
        // rotation rule, not the (more permissive) first-time rule.
        let manifest = parse_manifest(&record.manifest_yaml)
            .map_err(|e| format!("the stored manifest for {name:?} no longer parses: {e}"))?;
        let facts = Facts::of(&manifest);
        store
            .revoked
            .insert(name.to_owned(), RevokedFacts::capture(&facts));
        store.write_atomically(path, parent)?;
        on_commit(&Change::Revoked { name });
    }
    Ok(existed)
}

/// `PATCH /api/v1/attached/{name}` (A3-2b — a deviation from the design
/// doc's §3.2 table for the same reason `list`/`GET` below deviates: enabling
/// and disabling an attached module needs no `os.json`/supervisor machinery,
/// so it gets its own endpoint on the registry this file already owns rather
/// than teaching `os_routes.rs` about a second, unrelated store). `Ok(true)`:
/// the record existed and its `disabled` flag is now `disabled`. `Ok(false)`:
/// no such record — `404` at the REST layer.
///
/// `on_commit` runs exactly once, still inside the file lock, right after the
/// flag flip is durably written — see [`Change`]'s doc comment. It never runs
/// when there was nothing to flip. A no-op flip (already in that state) still
/// runs it: a disable retried after a crash must still revoke any generation
/// that came back up in between (§5.3).
pub fn set_disabled(
    path: &Path,
    name: &str,
    disabled: bool,
    on_commit: impl FnOnce(&Change),
) -> Result<bool, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let _guard = ConfigLock::acquire(parent)?;

    let mut store = AttachedStore::load(path)?;
    let Some(record) = store.modules.get_mut(name) else {
        return Ok(false);
    };
    record.disabled = disabled;
    store.write_atomically(path, parent)?;
    on_commit(&Change::Disabled { name, disabled });
    Ok(true)
}

/// A3-2b: everything [`crate::attach_registry::AttachRegistry`] needs to
/// hydrate its in-memory entry for one registered module at daemon startup —
/// before the listener has taken its first connection, so `attached.json` is
/// the only source of truth available yet (§5.6: "daemon 重启：注册记录从
/// `attached.json` 读回").
pub struct StoredEntry {
    pub manifest: agent24_domain::DomainOsManifest,
    pub manifest_digest: String,
    pub token_sha256_hex: String,
    pub token_id: String,
    pub disabled: bool,
}

/// Every registered module, in full — see [`StoredEntry`]. Unlike [`list`],
/// this re-parses each stored manifest (needed to rebuild `Grants`/`Offer`/
/// `ModelGrant`).
///
/// Review M2 ②: a record whose manifest no longer parses (the domain crate's
/// validation rules changed underneath a daemon upgrade) is logged and
/// SKIPPED, not propagated as an error for the whole call — the first version
/// used `.collect::<Result<Vec<_>, _>>()`, so ONE unparseable record made
/// EVERY other registered module fail to hydrate (and therefore refuse every
/// handshake with `auth_failed`, §4.3's `expectation` returning `None`), the
/// exact "one bad apple" blast radius this function's own doc used to warn
/// against without actually preventing. The file itself being unreadable or
/// not valid JSON (`AttachedStore::load`'s error) is a different, harder
/// failure — that one still propagates, since there is no per-record data to
/// salvage from a file that never parsed as JSON at all.
pub fn load_all(path: &Path) -> Result<Vec<(String, StoredEntry)>, String> {
    let store = AttachedStore::load(path)?;
    let mut out = Vec::with_capacity(store.modules.len());
    for (name, r) in store.modules {
        match parse_manifest(&r.manifest_yaml) {
            Ok(manifest) => out.push((
                name,
                StoredEntry {
                    manifest,
                    manifest_digest: r.manifest_digest,
                    token_sha256_hex: r.token_sha256,
                    token_id: r.token_id,
                    disabled: r.disabled,
                },
            )),
            Err(e) => {
                tracing::error!(
                    "the stored manifest for {name:?} no longer parses ({e}); skipping it — \
                     every OTHER registered module still hydrates. {name:?} will not be \
                     reachable until re-registered."
                );
            }
        }
    }
    Ok(out)
}

/// `GET /api/v1/attached` (a deviation from the design doc's §3.2 table —
/// see `docs/design/A3-ATTACHED-MODULE.md` and this PR's own report for why:
/// the augmentation described there for `GET /api/v1/os` needs live
/// generation state (`attach_status`, `generation`) that only exists once
/// A3-2b wires a real registry; a NEW, narrower endpoint here avoids widening
/// `agent24_protocol::DomainOsView`/`os_routes.rs` twice). NEVER includes the
/// token or its hash (§3.2/§3.3 C1).
///
/// `attach_status`/`generation` are always the disk-only default
/// (`"detached"`/`None`) here — this function knows nothing about a live
/// registry. [`crate::attached_routes::list_attached_at`] overlays the real
/// values from [`crate::attach_registry::AttachRegistry::status_of`] before
/// this is ever sent to a client.
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
            attach_status: "detached".to_owned(),
            generation: None,
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
        let outcome = register(&path, &m, false, |_| false, |_| {}).unwrap();
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
        let RegisterOutcome::Created(resp) = register(&path, &m, false, |_| false, |_| {}).unwrap()
        else {
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
        let RegisterOutcome::Created(first) =
            register(&path, &m, false, |_| false, |_| {}).unwrap()
        else {
            panic!("expected Created");
        };
        let RegisterOutcome::Rotated {
            response: second,
            digest_changed,
        } = register(&path, &m, false, |_| false, |_| {}).unwrap()
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
        assert!(
            !digest_changed,
            "the exact same manifest bytes must report digest_changed = false"
        );
        let old_hash = hash_token_hex(&first.token);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            raw.matches(&old_hash).count(),
            0,
            "the old token's hash must not survive a rotation"
        );
    }

    #[test]
    fn rotating_with_a_different_manifest_reports_digest_changed() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let base = manifest("agentear", None, &["events"]);
        register(&path, &base, false, |_| false, |_| {}).unwrap();
        // Narrowing (not a relax) so this does not need `allow_relax`, but
        // the bytes — and therefore the digest — are different from `base`.
        let narrower = manifest("agentear", None, &[]);
        let RegisterOutcome::Rotated { digest_changed, .. } =
            register(&path, &narrower, false, |_| false, |_| {}).unwrap()
        else {
            panic!("expected Rotated");
        };
        assert!(
            digest_changed,
            "a different manifest must report digest_changed = true"
        );
    }

    #[test]
    fn on_commit_runs_inside_the_file_lock() {
        // Deterministic, single-threaded proof rather than racing a second
        // thread's `register` against the first's `on_commit`: from INSIDE
        // `on_commit`, open a second file descriptor on the very same lock
        // file and try a NON-BLOCKING exclusive lock on it. `flock`-style
        // locks are per-open-file-description, so this must fail exactly
        // when `register`'s own `_guard` (a different fd, same path) is
        // still held — i.e. only while `on_commit` genuinely runs inside the
        // critical section.
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", None, &["events"]);
        let lock_path = dir.join("attached.json.lock");

        let mut still_locked = false;
        register(
            &path,
            &m,
            false,
            |_| false,
            |change| {
                match change {
                    Change::Registered { name, rotated, .. } => {
                        assert_eq!(*name, "agentear");
                        assert!(!rotated);
                    }
                    Change::Revoked { .. } | Change::Disabled { .. } => {
                        panic!("expected Registered")
                    }
                }
                use fs2::FileExt;
                let probe = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(&lock_path)
                    .unwrap();
                still_locked = probe.try_lock_exclusive().is_err();
            },
        )
        .unwrap();

        assert!(
            still_locked,
            "on_commit must run while register's own file lock is still held"
        );
    }

    #[test]
    fn a_relaxing_request_without_allow_relax_is_refused_and_leaves_the_record_untouched() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", Some("remote_allowed"), &["events", "models"]);
        let err = register(&path, &m, false, |_| false, |_| {}).unwrap_err();
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
        let outcome = register(&path, &m, true, |_| false, |_| {}).unwrap();
        assert!(matches!(outcome, RegisterOutcome::Created(_)));
    }

    #[test]
    fn widening_capabilities_on_reregistration_is_a_relax_without_allow_relax() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let base = manifest("agentear", None, &["events"]);
        register(&path, &base, false, |_| false, |_| {}).unwrap();
        let wider = manifest("agentear", None, &["events", "models"]);
        let err = register(&path, &wider, false, |_| false, |_| {}).unwrap_err();
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
        register(&path, &wide, true, |_| false, |_| {}).unwrap();
        let narrow = manifest("agentear", Some("local_only"), &["events", "models"]);
        let outcome = register(&path, &narrow, false, |_| false, |_| {}).unwrap();
        assert!(matches!(outcome, RegisterOutcome::Rotated { .. }));
    }

    #[test]
    fn a_name_already_claimed_by_another_kind_of_module_is_refused() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("sin90", None, &["events"]);
        let err = register(&path, &m, false, |n| n == "sin90", |_| {}).unwrap_err();
        assert_eq!(err, RegisterError::NameTaken("sin90".to_owned()));
        assert!(!path.exists());
    }

    #[test]
    fn the_reserved_name_attached_is_refused() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("attached", None, &[]);
        let err = register(&path, &m, false, |_| false, |_| {}).unwrap_err();
        assert!(matches!(err, RegisterError::InvalidManifest(_)));
    }

    #[test]
    fn revoke_removes_the_record_and_is_idempotent_about_reporting_it() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", None, &["events"]);
        register(&path, &m, false, |_| false, |_| {}).unwrap();
        assert!(revoke(&path, "agentear", |_| {}).unwrap());
        assert!(list(&path).unwrap().is_empty());
        assert!(
            !revoke(&path, "agentear", |_| {}).unwrap(),
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
        assert!(register(&path, &manifest("x", None, &[]), false, |_| false, |_| {}).is_err());
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
                    register(&path, &m, false, |_| false, |_| {}).unwrap();
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

    // ── M2: revoke-then-re-add must not dodge the relax confirmation ──

    #[test]
    fn re_adding_a_revoked_name_with_wider_capabilities_still_needs_confirmation() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let narrow = manifest("agentear", None, &["events"]);
        register(&path, &narrow, false, |_| false, |_| {}).unwrap();
        assert!(revoke(&path, "agentear", |_| {}).unwrap());

        // Without the tombstone comparison this would fall into the
        // first-time rule, which only cares about `remote_allowed` — a wider
        // CAPABILITY set on a first-time-looking add would sail through
        // unconfirmed. With it, this is judged exactly like a rotation would
        // be: `models` is new relative to what "agentear" had before it was
        // revoked, so it is a relax.
        let wider = manifest("agentear", None, &["events", "models"]);
        let err = register(&path, &wider, false, |_| false, |_| {}).unwrap_err();
        assert_eq!(err, RegisterError::RelaxRequiresConfirmation);
        assert!(
            list(&path).unwrap().is_empty(),
            "a refused relax must not create a record"
        );
    }

    #[test]
    fn re_adding_a_revoked_name_with_wider_capabilities_and_allow_relax_succeeds() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let narrow = manifest("agentear", None, &["events"]);
        register(&path, &narrow, false, |_| false, |_| {}).unwrap();
        assert!(revoke(&path, "agentear", |_| {}).unwrap());

        let wider = manifest("agentear", None, &["events", "models"]);
        let outcome = register(&path, &wider, true, |_| false, |_| {}).unwrap();
        assert!(
            matches!(outcome, RegisterOutcome::Created(_)),
            "a brand-new record after a revoke is a Created, not a Rotated, even though a \
             tombstone informed the relax check"
        );
    }

    #[test]
    fn a_successful_registration_clears_the_revoked_tombstone() {
        // White-box (same-module access to `AttachedStore`): a mutation that
        // dropped the `store.revoked.remove(&facts.name)` call in `register`
        // would leave this red, since the tombstone would still be on disk
        // after the second registration succeeds.
        let dir = tmp();
        let path = dir.join("attached.json");
        let narrow = manifest("agentear", None, &["events"]);
        register(&path, &narrow, false, |_| false, |_| {}).unwrap();
        assert!(revoke(&path, "agentear", |_| {}).unwrap());

        let after_revoke = AttachedStore::load(&path).unwrap();
        assert!(
            after_revoke.revoked.contains_key("agentear"),
            "revoke must leave a tombstone behind"
        );

        let wider = manifest("agentear", None, &["events", "models"]);
        register(&path, &wider, true, |_| false, |_| {}).unwrap();

        let after_register = AttachedStore::load(&path).unwrap();
        assert!(
            !after_register.revoked.contains_key("agentear"),
            "a successful registration must clear the name's tombstone"
        );
    }

    /// Review M4: a rotation (re-`register` of an already-registered name)
    /// must PRESERVE `disabled` — only `DELETE` (nothing left to inherit
    /// from) or an explicit `set_disabled(..., false, ...)` (`PATCH
    /// .../{"enabled":true}`) may clear it. Before this fix, `register`
    /// always wrote `disabled: false`, so an AgentEar auto-update re-running
    /// `attach add` (§5.6's automatic digest-mismatch rotation, or a plain
    /// token refresh) would silently re-enable a module the user had
    /// disabled.
    #[test]
    fn rotating_a_disabled_module_keeps_it_disabled() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let m = manifest("agentear", None, &["events"]);
        register(&path, &m, false, |_| false, |_| {}).unwrap();
        assert!(set_disabled(&path, "agentear", true, |_| {}).unwrap());
        assert!(AttachedStore::load(&path).unwrap().modules["agentear"].disabled);

        // A rotation (re-add, same manifest — a token-only rotation) must
        // NOT clear it.
        register(&path, &m, false, |_| false, |_| {}).unwrap();
        assert!(
            AttachedStore::load(&path).unwrap().modules["agentear"].disabled,
            "a rotation must preserve `disabled`, not silently re-enable the module"
        );

        // DELETE then a fresh add DOES start enabled — there is no record
        // left to inherit `disabled` from.
        assert!(revoke(&path, "agentear", |_| {}).unwrap());
        register(&path, &m, false, |_| false, |_| {}).unwrap();
        assert!(!AttachedStore::load(&path).unwrap().modules["agentear"].disabled);

        // An explicit re-enable clears it, same as always.
        assert!(set_disabled(&path, "agentear", true, |_| {}).unwrap());
        assert!(set_disabled(&path, "agentear", false, |_| {}).unwrap());
        assert!(!AttachedStore::load(&path).unwrap().modules["agentear"].disabled);
    }

    /// Review M4, the digest-CHANGED half: a full re-registration (not just a
    /// token-only rotation) must also preserve `disabled` — narrowing
    /// capabilities is not a relax (§3.5), so this does not need
    /// `allow_relax` and isolates the `digest_changed` branch of `register`
    /// from the tombstone-relax interaction the token-only-rotation test
    /// above does not exercise.
    #[test]
    fn a_full_reregistration_with_a_different_digest_also_preserves_disabled() {
        let dir = tmp();
        let path = dir.join("attached.json");
        let wide = manifest("agentear", None, &["events", "models"]);
        register(&path, &wide, false, |_| false, |_| {}).unwrap();
        assert!(set_disabled(&path, "agentear", true, |_| {}).unwrap());

        let narrower = manifest("agentear", None, &["events"]);
        let RegisterOutcome::Rotated { digest_changed, .. } =
            register(&path, &narrower, false, |_| false, |_| {}).unwrap()
        else {
            panic!("expected Rotated");
        };
        assert!(
            digest_changed,
            "different capability lists must be different bytes"
        );
        assert!(
            AttachedStore::load(&path).unwrap().modules["agentear"].disabled,
            "a full re-registration (digest changed) must also preserve `disabled`"
        );
    }

    /// Review M2 ②: one record whose manifest no longer parses must not take
    /// every OTHER registered module down with it. The first version used
    /// `.collect::<Result<Vec<_>, _>>()`, so a single bad record made
    /// `load_all` return `Err` wholesale — and `AttachRegistry::hydrate`
    /// propagates that `Err`, which meant EVERY module (not just the broken
    /// one) would fail every future handshake with `auth_failed`
    /// (`AttachRegistry::expectation` finding nothing at all).
    #[test]
    fn load_all_skips_one_unparseable_record_and_still_loads_the_rest() {
        let dir = tmp();
        let path = dir.join("attached.json");
        register(
            &path,
            &manifest("good1", None, &["events"]),
            false,
            |_| false,
            |_| {},
        )
        .unwrap();
        register(
            &path,
            &manifest("good2", None, &["events"]),
            false,
            |_| false,
            |_| {},
        )
        .unwrap();

        // Corrupt "good2" in place: white-box edit of the stored manifest
        // text so it no longer parses as `attached_process` (simulates a
        // domain-crate validation rule tightening underneath a daemon
        // upgrade — not something `register` itself could ever produce).
        let mut store = AttachedStore::load(&path).unwrap();
        store.modules.get_mut("good2").unwrap().manifest_yaml =
            "name: good2\nversion: \"1\"\nimpl_kind: not_a_real_impl_kind\n".to_owned();
        let parent = path.parent().unwrap();
        let _guard = ConfigLock::acquire(parent).unwrap();
        store.write_atomically(&path, parent).unwrap();
        drop(_guard);

        let loaded = load_all(&path).unwrap();
        let names: Vec<&str> = loaded.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["good1"],
            "the unparseable record must be skipped, not turn the whole call into Err — and \
             the still-good record must still load"
        );
    }
}
