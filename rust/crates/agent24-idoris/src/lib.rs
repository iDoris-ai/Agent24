//! Pinned iDoris release metadata and launcher contract compatibility.

pub mod download;
pub mod install;
#[cfg(unix)]
pub mod omlx_probe;

use semver::Version;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

const EMBEDDED: &str = include_str!("../idoris.lock.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdorisLock {
    pub schema: u32,
    pub tag: String,
    pub commit: String,
    pub assets: BTreeMap<String, LockedAsset>,
    pub team_id: String,
    pub contract_major: u64,
    pub min_contract_minor: u64,
    pub tested_contract_version: String,
    pub capabilities: Vec<Capability>,
    pub release_url_base: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedAsset {
    pub name: String,
    pub sha256: String,
    pub files: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
        let lock: Self = serde_json::from_str(json)?;
        require(lock.schema == 1, "schema")?;
        let tag = lock
            .tag
            .strip_prefix('v')
            .ok_or(LockError::Invalid("tag"))?;
        require(
            Version::parse(tag).is_ok_and(|v| v.pre.is_empty() && v.build.is_empty()),
            "tag",
        )?;
        let commit = &lock.commit;
        require(
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b"a"[0]..=b"f"[0]).contains(&b)),
            "commit",
        )?;
        let team = &lock.team_id;
        let is_team_char = |b: u8| b.is_ascii_uppercase() || b.is_ascii_digit();
        require(
            team.len() == 10 && team.bytes().all(is_team_char),
            "team_id",
        )?;
        for (triple, asset) in &lock.assets {
            require(
                asset.name == format!("idoris-{}-{triple}.tar.gz", lock.tag),
                "assets.name",
            )?;
            let hash = &asset.sha256;
            let is_lower_hex = |b: u8| b.is_ascii_hexdigit() && !b.is_ascii_uppercase();
            require(
                hash.len() == 64 && hash.bytes().all(is_lower_hex),
                "assets.sha256",
            )?;
            let mut files = BTreeSet::new();
            require(!asset.files.is_empty(), "assets.files")?;
            for file in &asset.files {
                require(
                    file.split('/').all(|part| !matches!(part, "" | "." | ".."))
                        && files.insert(file),
                    "assets.files",
                )?;
            }
        }
        require(
            lock.check_contract(&lock.tested_contract_version).is_ok(),
            "tested_contract_version",
        )?;
        let ids: BTreeSet<_> = lock
            .capabilities
            .iter()
            .map(|cap| cap.id.as_str())
            .collect();
        require(
            lock.capabilities.len() == 6
                && ids == BTreeSet::from(["L1", "L2", "L3", "L4", "L5", "L6"]),
            "capabilities",
        )?;
        let base = &lock.release_url_base;
        let url = url::Url::parse(base).map_err(|_| LockError::Invalid("release_url_base"))?;
        require(
            url.scheme() == "https"
                && url.host_str() == Some("github.com")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && !base.contains(|c: char| c.is_whitespace() || c == '\\'),
            "release_url_base",
        )?;
        Ok(lock)
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
