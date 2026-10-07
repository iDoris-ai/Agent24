//! Model catalog (D0-3, `docs/agent/PLAN-DECIDE.md` §1.1): `decide-models.catalog.json`
//! lives at the crate root. Every entry pins a HF repo to an exact
//! `revision` + `sha256` — "pin 版本 + sha256，同 Open Design 组件机制" — so
//! loading rejects any entry missing either field rather than silently
//! treating it as "latest" (which is exactly the kind of drift pinning
//! exists to prevent).
//!
//! The bundled file starts with an empty `entries` array on purpose:
//! `PLAN-DECIDE.md` §1.1 is explicit that the latency/memory numbers in the
//! catalog "只能来自 D0 实测，不抄模型卡" — D0-5/D0-6 fill it in once there is
//! real measurement, not before.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::tier::Tier;

/// Which runtime loads a catalog entry's weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Ort,
    Omlx,
    Ollaya,
}

/// One real measurement of a catalog entry, from D0-6's two-machine pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub machine: String,
    pub p95_ms: u64,
    pub rss_bytes: u64,
}

/// A validated catalog entry. Construction only happens through
/// [`ModelCatalog::load_str`], which is what enforces `revision`/`sha256`
/// being present — there is no public constructor that bypasses it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CatalogEntry {
    pub id: String,
    pub hf_repo: String,
    pub revision: String,
    pub sha256: String,
    pub license: String,
    pub download_bytes: u64,
    pub resident_bytes: u64,
    pub runtime: Runtime,
    pub points: Vec<String>,
    pub tiers: Vec<Tier>,
    pub measured: Option<Vec<Measurement>>,
}

/// Raw, unvalidated shape straight off the wire — `revision`/`sha256` are
/// optional here precisely so [`ModelCatalog::load_str`] can tell "absent"
/// apart from "present but empty" and reject both with a specific error
/// naming the entry, instead of a generic deserialize failure.
#[derive(Debug, Deserialize)]
struct RawEntry {
    id: String,
    hf_repo: String,
    revision: Option<String>,
    sha256: Option<String>,
    license: String,
    download_bytes: u64,
    resident_bytes: u64,
    runtime: Runtime,
    #[serde(default)]
    points: Vec<String>,
    #[serde(default)]
    tiers: Vec<Tier>,
    #[serde(default)]
    measured: Option<Vec<Measurement>>,
}

#[derive(Debug, Deserialize)]
struct RawCatalog {
    entries: Vec<RawEntry>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CatalogError {
    #[error("invalid catalog JSON: {0}")]
    Parse(String),
    #[error("entry {id:?} is missing revision (pin required, no \"latest\")")]
    MissingRevision { id: String },
    #[error("entry {id:?} is missing sha256 (pin required, no \"latest\")")]
    MissingSha256 { id: String },
}

/// A loaded, validated model catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCatalog {
    entries: Vec<CatalogEntry>,
}

impl ModelCatalog {
    /// Parses and validates a catalog. Fails the WHOLE load on the first
    /// entry missing `revision` or `sha256` — a catalog is either entirely
    /// pinned or rejected, never "mostly pinned, one entry silently
    /// skipped" (that would hide exactly the kind of mistake pinning exists
    /// to catch).
    pub fn load_str(json: &str) -> Result<Self, CatalogError> {
        let raw: RawCatalog =
            serde_json::from_str(json).map_err(|e| CatalogError::Parse(e.to_string()))?;
        let mut entries = Vec::with_capacity(raw.entries.len());
        for e in raw.entries {
            let revision = e
                .revision
                .filter(|s| !s.is_empty())
                .ok_or_else(|| CatalogError::MissingRevision { id: e.id.clone() })?;
            let sha256 = e
                .sha256
                .filter(|s| !s.is_empty())
                .ok_or_else(|| CatalogError::MissingSha256 { id: e.id.clone() })?;
            entries.push(CatalogEntry {
                id: e.id,
                hf_repo: e.hf_repo,
                revision,
                sha256,
                license: e.license,
                download_bytes: e.download_bytes,
                resident_bytes: e.resident_bytes,
                runtime: e.runtime,
                points: e.points,
                tiers: e.tiers,
                measured: e.measured,
            });
        }
        Ok(Self { entries })
    }

    /// The catalog bundled with this crate (`decide-models.catalog.json`).
    /// Starts empty — see module docs.
    pub fn bundled() -> Result<Self, CatalogError> {
        Self::load_str(include_str!("../decide-models.catalog.json"))
    }

    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn bundled_catalog_loads_and_starts_empty() {
        let catalog = ModelCatalog::bundled().expect("bundled catalog must be valid JSON");
        assert!(
            catalog.entries().is_empty(),
            "D0-1/D0-3 ship no measured entries yet"
        );
    }

    #[test]
    fn valid_entry_round_trips() {
        let json = r#"{
            "entries": [{
                "id": "qwen3guard-0.6b",
                "hf_repo": "Qwen/Qwen3Guard-0.6B",
                "revision": "abc123",
                "sha256": "deadbeef",
                "license": "Apache-2.0",
                "download_bytes": 1200000000,
                "resident_bytes": 1400000000,
                "runtime": "ort",
                "points": ["guardian.risk"],
                "tiers": ["t2", "t3"]
            }]
        }"#;
        let catalog = ModelCatalog::load_str(json).expect("valid entry must load");
        assert_eq!(catalog.entries().len(), 1);
        assert_eq!(catalog.entries()[0].revision, "abc123");
        assert_eq!(catalog.entries()[0].tiers, vec![Tier::T2, Tier::T3]);
    }

    #[test]
    fn entry_missing_revision_is_rejected() {
        let json = r#"{
            "entries": [{
                "id": "no-revision",
                "hf_repo": "org/model",
                "sha256": "deadbeef",
                "license": "Apache-2.0",
                "download_bytes": 1,
                "resident_bytes": 1,
                "runtime": "omlx"
            }]
        }"#;
        let err = ModelCatalog::load_str(json).unwrap_err();
        assert_eq!(
            err,
            CatalogError::MissingRevision {
                id: "no-revision".to_owned()
            }
        );
    }

    #[test]
    fn entry_missing_sha256_is_rejected() {
        let json = r#"{
            "entries": [{
                "id": "no-sha",
                "hf_repo": "org/model",
                "revision": "main",
                "license": "Apache-2.0",
                "download_bytes": 1,
                "resident_bytes": 1,
                "runtime": "ollaya"
            }]
        }"#;
        let err = ModelCatalog::load_str(json).unwrap_err();
        assert_eq!(
            err,
            CatalogError::MissingSha256 {
                id: "no-sha".to_owned()
            }
        );
    }

    #[test]
    fn empty_string_revision_is_treated_as_missing() {
        let json = r#"{
            "entries": [{
                "id": "blank-revision",
                "hf_repo": "org/model",
                "revision": "",
                "sha256": "deadbeef",
                "license": "Apache-2.0",
                "download_bytes": 1,
                "resident_bytes": 1,
                "runtime": "ort"
            }]
        }"#;
        let err = ModelCatalog::load_str(json).unwrap_err();
        assert_eq!(
            err,
            CatalogError::MissingRevision {
                id: "blank-revision".to_owned()
            }
        );
    }

    #[test]
    fn one_bad_entry_fails_the_whole_load_not_just_that_entry() {
        let json = r#"{
            "entries": [
                {
                    "id": "good",
                    "hf_repo": "org/good",
                    "revision": "main",
                    "sha256": "deadbeef",
                    "license": "Apache-2.0",
                    "download_bytes": 1,
                    "resident_bytes": 1,
                    "runtime": "ort"
                },
                {
                    "id": "bad",
                    "hf_repo": "org/bad",
                    "sha256": "deadbeef",
                    "license": "Apache-2.0",
                    "download_bytes": 1,
                    "resident_bytes": 1,
                    "runtime": "ort"
                }
            ]
        }"#;
        let err = ModelCatalog::load_str(json).unwrap_err();
        assert_eq!(
            err,
            CatalogError::MissingRevision {
                id: "bad".to_owned()
            },
            "a bad entry must fail the whole catalog, not be silently dropped"
        );
    }
}
