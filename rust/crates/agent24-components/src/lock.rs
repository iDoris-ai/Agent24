//! `ComponentLock` (ADR-038 D2, schema 2): the one lock format for every component that
//! Agent24 downloads. Parsing is strict: unknown fields, other schema versions and
//! structurally unsafe values are rejected. Schema 1 is readable only through
//! [`ComponentLock::migrate_schema1`].

use semver::Version;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: u32 = 2;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentLock {
    pub schema: u32,
    pub id: String,
    /// Release tag including the leading `v`; also the managed-root version directory.
    pub version: String,
    pub source: Source,
    pub contract: Contract,
    pub release_url_base: String,
    /// Keyed by `<os>-<arch>` (`darwin-arm64`); a platform without an asset has no key.
    pub assets: BTreeMap<String, Asset>,
    /// Required as soon as any `darwin-*` asset exists.
    #[serde(default)]
    pub signing: Option<Signing>,
    #[serde(default)]
    pub reproducible: Option<Reproducible>,
    #[serde(default)]
    pub stop_grace_ms: Option<u64>,
    /// Capabilities (能力) this component provides. Not an authorization input (D7).
    pub provides: Vec<Provides>,
    /// Kernel permissions the component asks for (D7). Not a capability token list.
    pub requires: Requires,
    /// Assurance levels with provenance (iDoris L1-L6); not capabilities.
    #[serde(default)]
    pub assurance: Vec<Assurance>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub repo: String,
    pub commit: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub name: String,
    pub major: u64,
    pub min_minor: u64,
    pub tested: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub name: String,
    /// Digest of the artifact as published, i.e. after signing.
    pub sha256: String,
    pub files: Vec<String>,
    /// Paths inside the artifact that D2a verifies one by one. Never empty.
    pub executables: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signing {
    pub macos_team_id: String,
    #[serde(default)]
    pub notarized: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reproducible {
    pub toolchain: String,
    pub recipe: String,
    /// Digest before signing, compared by the reproduction CI (never by the downloader).
    pub unsigned_sha256: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Localized {
    pub zh: String,
    pub en: String,
    pub th: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provides {
    pub id: String,
    pub name: Localized,
    pub summary: Localized,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    #[serde(default)]
    pub kernel: Vec<String>,
    #[serde(default)]
    pub operations: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assurance {
    pub id: String,
    pub source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("invalid component lock JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid component lock field: {0}")]
    Invalid(&'static str),
    #[error("component lock schema {0} is not readable here (expected 2)")]
    Schema(u32),
}

fn require(valid: bool, field: &'static str) -> Result<(), LockError> {
    valid.then_some(()).ok_or(LockError::Invalid(field))
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `<os>-<arch>` key of the host, or None for a platform with no defined key.
pub fn platform_key(os: &str, arch: &str) -> Option<String> {
    let os = match os {
        "macos" | "darwin" => "darwin",
        "linux" => "linux",
        _ => return None,
    };
    let arch = match arch {
        "aarch64" | "arm64" => "arm64",
        "x86_64" | "x64" => "x64",
        _ => return None,
    };
    Some(format!("{os}-{arch}"))
}

/// Rust target triple (`aarch64-apple-darwin`) to platform key, for schema-1 readers.
pub fn platform_key_from_triple(triple: &str) -> Option<String> {
    let mut parts = triple.split('-');
    let arch = parts.next()?;
    let _vendor = parts.next()?;
    platform_key(parts.next()?, arch)
}

/// Safe relative path inside an artifact.
pub(crate) fn clean_relative(path: &str) -> bool {
    !path.contains(['\\', '\0'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|p| !matches!(p, "" | "." | ".."))
}

impl ComponentLock {
    pub fn parse(json: &str) -> Result<Self, LockError> {
        // Read the schema first so a schema-1 file reports a version error, not a
        // confusing unknown-field error.
        #[derive(Deserialize)]
        struct Probe {
            schema: u32,
        }
        let probe: Probe = serde_json::from_str(json)?;
        if probe.schema != SCHEMA {
            return Err(LockError::Schema(probe.schema));
        }
        let lock: Self = serde_json::from_str(json)?;
        lock.validate()?;
        Ok(lock)
    }

    pub fn validate(&self) -> Result<(), LockError> {
        require(self.schema == SCHEMA, "schema")?;
        let id_ok = |s: &str, min: usize, max: usize| {
            (min..=max).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && s.as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        };
        require(id_ok(&self.id, 1, 32), "id")?;
        let tag = self
            .version
            .strip_prefix('v')
            .ok_or(LockError::Invalid("version"))?;
        require(
            Version::parse(tag).is_ok_and(|v| v.pre.is_empty() && v.build.is_empty()),
            "version",
        )?;
        require(is_lower_hex(&self.source.commit, 40), "source.commit")?;
        require(
            self.source.repo.starts_with("https://github.com/"),
            "source.repo",
        )?;
        require(!self.contract.name.is_empty(), "contract.name")?;
        let tested = Version::parse(&self.contract.tested)
            .map_err(|_| LockError::Invalid("contract.tested"))?;
        require(
            tested.major == self.contract.major && tested.minor >= self.contract.min_minor,
            "contract.tested",
        )?;
        require(!self.assets.is_empty(), "assets")?;
        let mut macos = false;
        for (platform, asset) in &self.assets {
            let (os, arch) = platform
                .split_once('-')
                .ok_or(LockError::Invalid("assets.platform"))?;
            require(
                matches!(os, "darwin" | "linux") && matches!(arch, "arm64" | "x64"),
                "assets.platform",
            )?;
            macos |= os == "darwin";
            require(
                !asset.name.is_empty() && clean_relative(&asset.name) && !asset.name.contains('/'),
                "assets.name",
            )?;
            require(is_lower_hex(&asset.sha256, 64), "assets.sha256")?;
            let mut seen = BTreeSet::new();
            require(!asset.files.is_empty(), "assets.files")?;
            for file in &asset.files {
                require(clean_relative(file) && seen.insert(file), "assets.files")?;
            }
            require(!asset.executables.is_empty(), "assets.executables")?;
            let mut exes = BTreeSet::new();
            for exe in &asset.executables {
                require(seen.contains(exe) && exes.insert(exe), "assets.executables")?;
            }
        }
        match &self.signing {
            Some(s) => require(
                s.macos_team_id.len() == 10
                    && s.macos_team_id
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
                "signing.macos_team_id",
            )?,
            // Our own macOS assets must be signed: no signing section, no valid lock.
            None => require(!macos, "signing")?,
        }
        if let Some(r) = &self.reproducible {
            require(
                !r.toolchain.is_empty() && !r.recipe.is_empty(),
                "reproducible",
            )?;
            for (platform, hash) in &r.unsigned_sha256 {
                require(
                    self.assets.contains_key(platform) && is_lower_hex(hash, 64),
                    "reproducible.unsigned_sha256",
                )?;
            }
        }
        let base = url::Url::parse(&self.release_url_base)
            .map_err(|_| LockError::Invalid("release_url_base"))?;
        let repo_path = self
            .source
            .repo
            .trim_start_matches("https://github.com")
            .trim_end_matches('/');
        let expected = format!("{repo_path}/releases/download");
        // Url removes an explicit :443, so also check the original authority.
        require(
            base.scheme() == "https"
                && base.host_str() == Some("github.com")
                && self.release_url_base.split('/').nth(2) == Some("github.com")
                && (base.path() == expected || base.path().starts_with(&format!("{expected}/")))
                && base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.fragment().is_none()
                && !self
                    .release_url_base
                    .contains(|c: char| c.is_whitespace() || c == '\\'),
            "release_url_base",
        )?;
        let mut ids = BTreeSet::new();
        for p in &self.provides {
            // Registered-id check (REGISTRY.md) belongs to the CI added in C7.
            require(id_ok(&p.id, 2, 32) && ids.insert(&p.id), "provides.id")?;
        }
        Ok(())
    }

    pub fn asset_for(&self, platform: &str) -> Option<&Asset> {
        self.assets.get(platform)
    }

    pub fn asset_url(&self, asset: &Asset) -> String {
        let base = self.release_url_base.trim_end_matches('/');
        format!("{base}/{}/{}", self.version, asset.name)
    }

    /// Contract acceptance (D3): same major, minor at least `min_minor`.
    pub fn accepts_contract(&self, version: &str) -> bool {
        Version::parse(version)
            .is_ok_and(|v| v.major == self.contract.major && v.minor >= self.contract.min_minor)
    }

    /// The only reader of schema-1 locks (iDoris shape). `id`, `provides` and
    /// `requires` did not exist in schema 1, so the caller supplies them; nothing
    /// else is guessed. No shipped lock is schema 1 any more (iDoris moved in C1b'),
    /// so this survives only to test the migration; Hyphae's schema-1 lock is read by
    /// agent24-comm. Delete it with its tests once C1b has moved Hyphae.
    #[cfg(test)]
    pub fn migrate_schema1(
        json: &str,
        id: &str,
        contract_name: &str,
        provides: Vec<Provides>,
        requires: Requires,
        executables: &[&str],
    ) -> Result<Self, LockError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct V1Asset {
            name: String,
            sha256: String,
            files: Vec<String>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct V1 {
            schema: u32,
            tag: String,
            commit: String,
            assets: BTreeMap<String, V1Asset>,
            team_id: String,
            contract_major: u64,
            min_contract_minor: u64,
            tested_contract_version: String,
            capabilities: Vec<Assurance>,
            release_url_base: String,
        }
        let v1: V1 = serde_json::from_str(json)?;
        require(v1.schema == 1, "schema")?;
        let repo = v1
            .release_url_base
            .strip_suffix("/releases/download")
            .ok_or(LockError::Invalid("release_url_base"))?
            .to_owned();
        let mut assets = BTreeMap::new();
        for (triple, a) in v1.assets {
            let key =
                platform_key_from_triple(&triple).ok_or(LockError::Invalid("assets.triple"))?;
            assets.insert(
                key,
                Asset {
                    name: a.name,
                    sha256: a.sha256,
                    files: a.files,
                    executables: executables.iter().map(|s| (*s).to_owned()).collect(),
                },
            );
        }
        let lock = Self {
            schema: SCHEMA,
            id: id.into(),
            version: v1.tag,
            source: Source {
                repo,
                commit: v1.commit,
            },
            contract: Contract {
                name: contract_name.into(),
                major: v1.contract_major,
                min_minor: v1.min_contract_minor,
                tested: v1.tested_contract_version,
            },
            release_url_base: v1.release_url_base,
            assets,
            signing: Some(Signing {
                macos_team_id: v1.team_id,
                // Schema 1 never recorded notarization; the caller sets it afterwards.
                notarized: false,
            }),
            reproducible: None,
            stop_grace_ms: None,
            provides,
            requires,
            assurance: v1.capabilities,
        };
        lock.validate()?;
        Ok(lock)
    }
}

/// A small valid lock for tests in this and dependent crates.
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::unwrap_used)]
pub fn test_lock() -> ComponentLock {
    ComponentLock::parse(TEST_LOCK).unwrap()
}

#[cfg(any(test, feature = "test-support"))]
pub const TEST_LOCK: &str = r#"{
  "schema": 2,
  "id": "demo",
  "version": "v1.2.3",
  "source": { "repo": "https://github.com/example/demo", "commit": "0123456789abcdef0123456789abcdef01234567" },
  "contract": { "name": "demo-http", "major": 1, "min_minor": 1, "tested": "1.2.0" },
  "release_url_base": "https://github.com/example/demo/releases/download",
  "assets": {
    "darwin-arm64": {
      "name": "demo-v1.2.3-darwin-arm64.tar.gz",
      "sha256": "ab46e0180090ef5143b8fb6ea98b0333f2ef9c3a137bab2dc3cf7307960476d5",
      "files": ["demo", "config/a.yaml"],
      "executables": ["demo"]
    }
  },
  "signing": { "macos_team_id": "HYAP444FN9", "notarized": false },
  "provides": [
    { "id": "demo-cap",
      "name": { "zh": "演示", "en": "Demo", "th": "สาธิต" },
      "summary": { "zh": "演示能力", "en": "A demo capability", "th": "สาธิต" },
      "tools": ["demo.run"] }
  ],
  "requires": { "kernel": ["events"], "operations": [] },
  "assurance": []
}"#;

#[cfg(test)]
mod tests;
