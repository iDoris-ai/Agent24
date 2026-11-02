//! Pinned iDoris release metadata and launcher contract compatibility.

pub mod keychain;
pub mod keys;
#[cfg(unix)]
pub mod omlx_probe;
#[cfg(unix)]
pub mod setup;
pub mod supervisor;

pub mod download;
pub mod install;

use agent24_components::lock::{ComponentLock, LockError as ComponentLockError};
use semver::Version;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

const EMBEDDED: &str = include_str!("../idoris.lock.json");

/// iDoris's view of its schema-2 `ComponentLock`: the generic lock stays the
/// source of truth, and this keeps the shape the supervisor and host already use
/// (assets keyed by Rust target triple, L1-L6 under `capabilities`).
#[derive(Debug, Clone)]
pub struct IdorisLock {
    pub schema: u32,
    pub tag: String,
    pub commit: String,
    pub assets: BTreeMap<String, LockedAsset>,
    pub team_id: String,
    pub contract_major: u64,
    pub min_contract_minor: u64,
    pub tested_contract_version: String,
    /// Assurance levels L1-L6 (not capabilities in the ADR-039 sense; the name is
    /// kept for the existing callers). Lives in `ComponentLock::assurance`.
    pub capabilities: Vec<Capability>,
    pub release_url_base: String,
    component: ComponentLock,
}

#[derive(Debug, Clone)]
pub struct LockedAsset {
    pub name: String,
    pub sha256: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Capability {
    pub id: String,
    pub source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("invalid iDoris lock JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid iDoris lock field: {0}")]
    Invalid(&'static str),
    #[error("iDoris lock rejected: {0}")]
    Component(ComponentLockError),
}

const REPO: &str = "https://github.com/iDoris-ai/iDoris";
const FILES: [&str; 5] = [
    "idoris",
    "config/catalog.yaml",
    "config/routing-policy.yaml",
    "config/components/omlx.yaml",
    "config/components/subscription.yaml",
];

/// Platform keys iDoris publishes for, with the Rust triple used in asset names.
const PLATFORM_TRIPLES: [(&str, &str); 4] = [
    ("darwin-arm64", "aarch64-apple-darwin"),
    ("darwin-x64", "x86_64-apple-darwin"),
    ("linux-arm64", "aarch64-unknown-linux-gnu"),
    ("linux-x64", "x86_64-unknown-linux-gnu"),
];

/// Platform key (`darwin-arm64`) for a Rust target triple, if iDoris knows it.
pub fn platform_of(triple: &str) -> Option<&'static str> {
    PLATFORM_TRIPLES
        .iter()
        .find(|(_, t)| *t == triple)
        .map(|(p, _)| *p)
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("expected iDoris contract major {expected_major}, minor >= {min_minor}; actual {actual:?}")]
pub struct ContractMismatch {
    pub expected_major: u64,
    pub min_minor: u64,
    pub actual: String,
}

impl IdorisLock {
    pub fn embedded() -> &'static Self {
        static LOCK: OnceLock<IdorisLock> = OnceLock::new();
        LOCK.get_or_init(|| {
            Self::parse(EMBEDDED).unwrap_or_else(|e| panic!("invalid embedded lock: {e}"))
        })
    }

    pub fn parse(json: &str) -> Result<Self, LockError> {
        // C1a: the shipped lock is still schema 1, read through the migration helper.
        // C1b' switches the file to schema 2 and removes this branch.
        if matches!(
            ComponentLock::parse(json),
            Err(ComponentLockError::Schema(1))
        ) {
            let component = ComponentLock::migrate_schema1(
                json,
                "idoris",
                "idoris-http",
                Vec::new(),
                Default::default(),
                &["idoris"],
            )
            .map_err(LockError::Component)?;
            let mut lock = Self::from_component(component)?;
            lock.schema = 1;
            return Ok(lock);
        }
        let component = ComponentLock::parse(json).map_err(|e| match e {
            ComponentLockError::Json(e) => LockError::Json(e),
            other => LockError::Component(other),
        })?;
        Self::from_component(component)
    }

    /// iDoris-specific rules on top of the generic validation.
    pub fn from_component(component: ComponentLock) -> Result<Self, LockError> {
        require(component.id == "idoris", "id")?;
        require(component.source.repo == REPO, "source.repo")?;
        require(component.contract.name == "idoris-http", "contract.name")?;
        let signing = component
            .signing
            .as_ref()
            .ok_or(LockError::Invalid("signing"))?;
        let mut assets = BTreeMap::new();
        for (platform, asset) in &component.assets {
            let (_, triple) = PLATFORM_TRIPLES
                .iter()
                .find(|(p, _)| p == platform)
                .ok_or(LockError::Invalid("assets.platform"))?;
            require(
                asset.name == format!("idoris-{}-{triple}.tar.gz", component.version),
                "assets.name",
            )?;
            require(asset.files == FILES, "assets.files")?;
            require(asset.executables == ["idoris"], "assets.executables")?;
            assets.insert(
                (*triple).to_owned(),
                LockedAsset {
                    name: asset.name.clone(),
                    sha256: asset.sha256.clone(),
                    files: asset.files.clone(),
                },
            );
        }
        let ids: BTreeSet<_> = component.assurance.iter().map(|a| a.id.as_str()).collect();
        require(
            component.assurance.len() == 6
                && ids == BTreeSet::from(["L1", "L2", "L3", "L4", "L5", "L6"]),
            "assurance",
        )?;
        Ok(Self {
            schema: component.schema,
            tag: component.version.clone(),
            commit: component.source.commit.clone(),
            assets,
            team_id: signing.macos_team_id.clone(),
            contract_major: component.contract.major,
            min_contract_minor: component.contract.min_minor,
            tested_contract_version: component.contract.tested.clone(),
            capabilities: component
                .assurance
                .iter()
                .map(|a| Capability {
                    id: a.id.clone(),
                    source: a.source.clone(),
                })
                .collect(),
            release_url_base: component.release_url_base.clone(),
            component,
        })
    }

    /// The generic lock the downloader and installer consume, rebuilt from the
    /// public fields so a mutated view is what gets installed (and is re-validated
    /// at the installation boundary).
    pub fn to_component(&self) -> ComponentLock {
        let mut lock = self.component.clone();
        lock.version = self.tag.clone();
        lock.source.commit = self.commit.clone();
        lock.release_url_base = self.release_url_base.clone();
        lock.contract.major = self.contract_major;
        lock.contract.min_minor = self.min_contract_minor;
        lock.contract.tested = self.tested_contract_version.clone();
        if let Some(signing) = &mut lock.signing {
            signing.macos_team_id = self.team_id.clone();
        }
        for (triple, view) in &self.assets {
            let Some(platform) = platform_of(triple) else {
                continue;
            };
            if let Some(asset) = lock.assets.get_mut(platform) {
                asset.name = view.name.clone();
                asset.sha256 = view.sha256.clone();
                asset.files = view.files.clone();
            }
        }
        // Platforms removed from the view are removed from the lock.
        lock.assets
            .retain(|platform, _| self.assets.keys().any(|t| platform_of(t) == Some(platform)));
        lock
    }

    pub fn asset_for(&self, target_triple: &str) -> Option<&LockedAsset> {
        self.assets.get(target_triple)
    }

    pub fn asset_url(&self, asset: &LockedAsset) -> String {
        let base = self.release_url_base.trim_end_matches('/');
        format!("{base}/{}/{}", self.tag, asset.name)
    }

    pub fn check_contract(&self, contract_version: &str) -> Result<(), ContractMismatch> {
        if Version::parse(contract_version)
            .is_ok_and(|v| v.major == self.contract_major && v.minor >= self.min_contract_minor)
        {
            Ok(())
        } else {
            Err(ContractMismatch {
                expected_major: self.contract_major,
                min_minor: self.min_contract_minor,
                actual: contract_version.into(),
            })
        }
    }
}

fn require(valid: bool, field: &'static str) -> Result<(), LockError> {
    valid.then_some(()).ok_or(LockError::Invalid(field))
}

#[cfg(test)]
mod tests;
