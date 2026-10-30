//! Agent24 release locations. Digests always come from the upstream lock.
use crate::{BinaryError, HyphaeLock, Sha256Digest};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(serde::Deserialize)]
pub struct HyphaeRelease {
    pub tag: String,
    pub release_url_base: String,
    pub assets: BTreeMap<String, Option<String>>,
}

impl HyphaeRelease {
    pub fn embedded() -> Result<Self, serde_json::Error> {
        serde_json::from_str(include_str!("../hyphae.release.json"))
    }

    pub fn asset_for(
        &self,
        lock: &HyphaeLock,
        platform: &str,
    ) -> Result<ReleaseAsset, BinaryError> {
        let expected = lock.expected_for(platform)?;
        let name = self
            .assets
            .get(platform)
            .and_then(Option::as_ref)
            .ok_or_else(|| BinaryError::NoLockForPlatform(platform.into()))?;
        Ok(ReleaseAsset {
            url: format!("{}/{}/{}", self.release_url_base, self.tag, name),
            name: name.clone(),
            expected,
        })
    }

    pub fn install_dir(&self, state_dir: &Path) -> PathBuf {
        state_dir.join("components/hyphae").join(&self.tag)
    }

    pub fn installed_path(&self, state_dir: &Path) -> PathBuf {
        self.install_dir(state_dir).join("hyphae")
    }
}

pub struct ReleaseAsset {
    pub url: String,
    pub name: String,
    pub expected: Sha256Digest,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn release_platform_keys_match_lock_and_null_is_never_installable() {
        let release = HyphaeRelease::embedded().unwrap();
        let lock = HyphaeLock::embedded().unwrap();
        assert_eq!(
            release.assets.keys().collect::<Vec<_>>(),
            lock.binaries.keys().collect::<Vec<_>>()
        );
        for (platform, hash) in &lock.binaries {
            assert_eq!(release.asset_for(&lock, platform).is_ok(), hash.is_some());
        }
        let mut lock = lock;
        lock.binaries.insert("darwin-arm64".into(), None);
        assert!(matches!(
            release.asset_for(&lock, "darwin-arm64"),
            Err(BinaryError::NoLockForPlatform(_))
        ));
    }
}
