//! Dock type contract (design D5/D6, Q1/Q3; 5f-1). Types and serialization
//! only: no route, no startup wiring (`ENABLED=false`).
//!
//! - [`ReplayOutcome::verdict`] folds the replay result into go / dock.
//! - [`DockedHealth`] is the `GET /api/v1/health` body of a docked daemon:
//!   CORE-4 fields (`protocol_version`, `migrations.{store,memory}`) keep
//!   their shape; `startup_state` and `reason` are added (Q1). `migrations`
//!   is the real post-restore value, or `0` (CORE-4's "nothing to report")
//!   with `reason.migrations = "unknown"` when it cannot be trusted.
//! - [`MigrationMarker`] is the `migration_failed` file (D5 table).

use std::path::{Path, PathBuf};

use agent24_protocol::HealthMigrations;
use serde::{Deserialize, Serialize};

use super::atomic::FileOps;
use super::guard_log::Phase;
use super::manifest::{
    BinaryIdentity, FORMAT_VERSION, StateError, parse_json, read_optional_capped, write_json,
};
use super::replay::{Dock, ReplayOutcome};

pub const MARKER_FILE: &str = "migration_failed";

/// Stable public reason code (D5). Never carries paths or raw errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockCode {
    /// Stopped before the first migration (databases unmigrated).
    Interrupted,
    /// The upgrade failed and the pair was restored from the snapshot.
    MigrationFailed,
    /// The restore itself failed; both databases stay disabled.
    RollbackFailed,
    /// Guard log corrupt, unknown format or unreadable.
    GuardInvalid,
    /// `Restoring` could not be persisted.
    PersistFailed,
    /// The marker file is corrupt or of an unknown format.
    MarkerInvalid,
    /// Free space below the budget (D2).
    SpaceInsufficient,
    /// No declared migration budget G, or space cannot be measured (D2).
    SpaceBudgetUnknown,
}

impl DockCode {
    /// The only `error_summary` a marker may carry (5f-3 gate): a closed set
    /// keyed by the stable code, never raw error text, paths or SQL.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::Interrupted => "upgrade interrupted before migration; databases unchanged",
            Self::MigrationFailed => "migration failed; databases restored from snapshot",
            Self::RollbackFailed => "restore failed; databases kept disabled, evidence retained",
            Self::GuardInvalid => "migration guard log unreadable; nothing was touched",
            Self::PersistFailed => "migration state could not be persisted",
            Self::MarkerInvalid => "migration marker unreadable or of unknown format",
            Self::SpaceInsufficient => "free space below the migration budget",
            Self::SpaceBudgetUnknown => "migration space budget unknown or unmeasurable",
        }
    }
}

/// Where the upgrade stopped (marker `phase`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockPhase {
    Preflight,
    Migrating,
    FtsRebuild,
    Restoring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackState {
    NotNeeded,
    InProgress,
    Restored,
    Failed,
}

/// Whether `migrations` in the health body is a verified post-restore value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationsState {
    Verified,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupState {
    Docked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockStatus {
    MigrationFailed,
}

/// Public `reason` object of the docked health body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockReason {
    pub code: DockCode,
    pub phase: DockPhase,
    pub rollback: RollbackState,
    /// Q2: always `false` until 2b/2d and the docked enrol channel exist.
    pub retry_allowed: bool,
    pub migrations: MigrationsState,
}

/// What startup does next. Only `Proceed` may continue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Proceed,
    Dock(DockReason),
}

impl ReplayOutcome {
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        use DockPhase as P;
        use MigrationsState::{Unknown, Verified};
        use RollbackState as R;
        let (code, phase, rollback, migrations) = match self {
            Self::Proceed => return Verdict::Proceed,
            Self::Restored { from, .. } => {
                let p = if *from == Phase::Migrating {
                    P::Migrating
                } else {
                    P::Restoring
                };
                (DockCode::MigrationFailed, p, R::Restored, Verified)
            }
            Self::Docked(Dock::Interrupted(_)) => {
                (DockCode::Interrupted, P::Preflight, R::NotNeeded, Verified)
            }
            // Unknown state: never claim a restore or a watermark.
            Self::Docked(Dock::GuardUnusable(_)) => {
                (DockCode::GuardInvalid, P::Restoring, R::Failed, Unknown)
            }
            Self::Docked(Dock::Persist(_)) => (
                DockCode::PersistFailed,
                P::Migrating,
                R::InProgress,
                Unknown,
            ),
            Self::Docked(Dock::RollbackFailed { .. }) => {
                (DockCode::RollbackFailed, P::Restoring, R::Failed, Unknown)
            }
        };
        Verdict::Dock(DockReason {
            code,
            phase,
            rollback,
            retry_allowed: false,
            migrations,
        })
    }
}

/// `GET /api/v1/health` while docked: HTTP 503, `status="migration_failed"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockedHealth {
    pub status: DockStatus,
    pub startup_state: StartupState,
    pub version: String,
    pub backend: String,
    pub protocol_version: u32,
    pub migrations: HealthMigrations,
    pub reason: DockReason,
}

impl DockedHealth {
    /// `observed` is only reported when the reason says it is verified;
    /// otherwise `0/0` (CORE-4 "nothing to report"), never the compiled-in max.
    #[must_use]
    pub fn new(
        reason: DockReason,
        version: String,
        backend: String,
        protocol_version: u32,
        observed: HealthMigrations,
    ) -> Self {
        let migrations = match reason.migrations {
            MigrationsState::Verified => observed,
            MigrationsState::Unknown => HealthMigrations {
                store: 0,
                memory: 0,
            },
        };
        Self {
            status: DockStatus::MigrationFailed,
            startup_state: StartupState::Docked,
            version,
            backend,
            protocol_version,
            migrations,
            reason: DockReason {
                retry_allowed: false,
                ..reason
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailedDb {
    Store,
    Memory,
}

/// One database's watermark; `version: None` = not verifiable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbMark {
    pub existed: bool,
    pub version: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairMark {
    pub store: DbMark,
    pub memory: DbMark,
}

/// `<data_dir>/migration_failed` (D5). No paths, SQL params or user content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationMarker {
    pub format_version: u32,
    pub attempt_id: String,
    /// `None` for a preflight failure (no snapshot taken).
    pub snapshot_id: Option<String>,
    /// RFC 3339 UTC.
    pub created_at: String,
    pub failed_binary: BinaryIdentity,
    pub previous_binary: Option<BinaryIdentity>,
    pub database: Option<FailedDb>,
    pub migration_version: Option<i64>,
    pub phase: DockPhase,
    pub error_code: DockCode,
    /// Always [`DockCode::summary`] of `error_code`; [`write_marker`] refuses
    /// anything else.
    pub error_summary: String,
    pub rollback: RollbackState,
    pub before_migrations: PairMark,
    /// `None` = unknown (restore failed).
    pub observed_migrations: Option<PairMark>,
}

/// Longest identifier a marker may carry (attempt id, snapshot id, version).
const ID_MAX: usize = 128;

impl MigrationMarker {
    /// Identifiers are short and plain: they reach the diagnostics body and
    /// name a directory, so anything else is a hostile marker.
    fn check_identifiers(&self) -> Result<(), StateError> {
        let ok = |s: &str, extra: &[u8]| {
            !s.is_empty()
                && s.len() <= ID_MAX
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
        };
        let good = ok(&self.attempt_id, b"-")
            && self.snapshot_id.as_deref().is_none_or(|s| ok(s, b"-"))
            && ok(&self.failed_binary.version, b"-.+");
        if good {
            Ok(())
        } else {
            Err(StateError::Invalid(
                "marker identifier out of bounds".into(),
            ))
        }
    }
}

#[must_use]
pub fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MARKER_FILE)
}

pub fn write_marker(
    ops: &dyn FileOps,
    data_dir: &Path,
    m: &MigrationMarker,
) -> Result<(), StateError> {
    if m.error_summary != m.error_code.summary() {
        return Err(StateError::Invalid(
            "error_summary is not the closed-set text for error_code".into(),
        ));
    }
    write_json(ops, data_dir, &marker_path(data_dir), m)
}

/// A real marker is well under 2 KiB; anything past this is not one.
pub const MARKER_MAX_BYTES: u64 = 16 * 1024;

/// `Ok(None)` only when the marker is genuinely absent. Corrupt, oversize,
/// unknown format or unreadable is `Err` = dock `marker_invalid`, never
/// "no marker".
pub fn read_marker(data_dir: &Path) -> Result<Option<MigrationMarker>, StateError> {
    let Some(bytes) = read_optional_capped(&marker_path(data_dir), MARKER_MAX_BYTES)? else {
        return Ok(None);
    };
    let m: MigrationMarker = parse_json(&bytes)?;
    if m.format_version != FORMAT_VERSION {
        return Err(StateError::Invalid(format!(
            "unknown marker format_version {}",
            m.format_version
        )));
    }
    m.check_identifiers()?;
    Ok(Some(m))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::super::atomic::RealOps;
    use super::super::guard_log::GuardError;
    use super::*;
    use std::fs;

    fn reason(code: DockCode, rollback: RollbackState, m: MigrationsState) -> DockReason {
        DockReason {
            code,
            phase: DockPhase::Migrating,
            rollback,
            retry_allowed: false,
            migrations: m,
        }
    }

    fn health(r: DockReason, observed: (i64, i64)) -> DockedHealth {
        DockedHealth::new(
            r,
            "0.6.0".into(),
            "rust".into(),
            3,
            HealthMigrations {
                store: observed.0,
                memory: observed.1,
            },
        )
    }

    #[test]
    fn health_serialization_shape_restored_reports_real_watermark() {
        let h = health(
            reason(
                DockCode::MigrationFailed,
                RollbackState::Restored,
                MigrationsState::Verified,
            ),
            (14, 9),
        );
        assert_eq!(
            serde_json::to_string(&h).unwrap(),
            r#"{"status":"migration_failed","startup_state":"docked","version":"0.6.0","backend":"rust","protocol_version":3,"migrations":{"store":14,"memory":9},"reason":{"code":"migration_failed","phase":"migrating","rollback":"restored","retry_allowed":false,"migrations":"verified"}}"#
        );
        let back: DockedHealth = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert_eq!(back, h);
    }

    /// Q1/T14: when the watermark cannot be trusted the shape is unchanged,
    /// the value is 0/0 and `reason.migrations` says unknown; the observed
    /// numbers handed in (e.g. the compiled-in max) are never reported.
    #[test]
    fn health_unknown_keeps_shape_and_hides_observed_numbers() {
        let h = health(
            reason(
                DockCode::RollbackFailed,
                RollbackState::Failed,
                MigrationsState::Unknown,
            ),
            (99, 99),
        );
        let v = serde_json::to_value(&h).unwrap();
        assert_eq!(v["migrations"], serde_json::json!({"store":0,"memory":0}));
        assert_eq!(v["reason"]["migrations"], "unknown");
        assert_eq!(v["reason"]["rollback"], "failed");
    }

    #[test]
    fn retry_is_never_allowed_whatever_the_reason_says() {
        let mut r = reason(
            DockCode::Interrupted,
            RollbackState::NotNeeded,
            MigrationsState::Verified,
        );
        r.retry_allowed = true;
        assert!(!health(r, (1, 1)).reason.retry_allowed);
    }

    #[test]
    fn unknown_wire_values_are_rejected() {
        assert!(serde_json::from_str::<DockCode>(r#""bogus""#).is_err());
        assert!(serde_json::from_str::<StartupState>(r#""ready""#).is_err());
        assert!(serde_json::from_str::<DockStatus>(r#""ok""#).is_err());
    }

    #[test]
    fn verdict_maps_every_replay_outcome() {
        use MigrationsState::{Unknown, Verified};
        use RollbackState as R;
        let v = |o: ReplayOutcome| o.verdict();
        let d = |c, p, r, m| {
            Verdict::Dock(DockReason {
                code: c,
                phase: p,
                rollback: r,
                retry_allowed: false,
                migrations: m,
            })
        };
        assert_eq!(v(ReplayOutcome::Proceed), Verdict::Proceed);
        assert_eq!(
            v(ReplayOutcome::Restored {
                from: Phase::Migrating,
                leftover_cleanup: 0
            }),
            d(
                DockCode::MigrationFailed,
                DockPhase::Migrating,
                R::Restored,
                Verified
            )
        );
        assert_eq!(
            v(ReplayOutcome::Restored {
                from: Phase::Restoring,
                leftover_cleanup: 3
            }),
            d(
                DockCode::MigrationFailed,
                DockPhase::Restoring,
                R::Restored,
                Verified
            )
        );
        assert_eq!(
            v(ReplayOutcome::Docked(Dock::Interrupted(Phase::Snapshotted))),
            d(
                DockCode::Interrupted,
                DockPhase::Preflight,
                R::NotNeeded,
                Verified
            )
        );
        assert_eq!(
            v(ReplayOutcome::Docked(Dock::GuardUnusable("x".into()))),
            d(
                DockCode::GuardInvalid,
                DockPhase::Restoring,
                R::Failed,
                Unknown
            )
        );
        assert_eq!(
            v(ReplayOutcome::Docked(Dock::Persist(GuardError::Invalid(
                "x".into()
            )))),
            d(
                DockCode::PersistFailed,
                DockPhase::Migrating,
                R::InProgress,
                Unknown
            )
        );
        assert_eq!(
            v(ReplayOutcome::Docked(Dock::RollbackFailed {
                reason: "x".into(),
                persisted: false
            })),
            d(
                DockCode::RollbackFailed,
                DockPhase::Restoring,
                R::Failed,
                Unknown
            )
        );
    }

    fn marker() -> MigrationMarker {
        let id = |v: &str| BinaryIdentity {
            version: v.into(),
            sha256: "h".into(),
        };
        let ok = DbMark {
            existed: true,
            version: Some(5),
        };
        MigrationMarker {
            format_version: FORMAT_VERSION,
            attempt_id: "a1".into(),
            snapshot_id: None,
            created_at: "2026-10-28T00:00:00Z".into(),
            failed_binary: id("0.6.0"),
            previous_binary: None,
            database: Some(FailedDb::Memory),
            migration_version: None,
            phase: DockPhase::Preflight,
            error_code: DockCode::SpaceInsufficient,
            error_summary: DockCode::SpaceInsufficient.summary().into(),
            rollback: RollbackState::NotNeeded,
            before_migrations: PairMark {
                store: ok,
                memory: DbMark {
                    existed: false,
                    version: None,
                },
            },
            observed_migrations: None,
        }
    }

    #[test]
    fn marker_roundtrip_and_shape() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(read_marker(td.path()).unwrap(), None);
        write_marker(&RealOps, td.path(), &marker()).unwrap();
        assert_eq!(read_marker(td.path()).unwrap(), Some(marker()));
        let v = serde_json::to_value(marker()).unwrap();
        assert_eq!(v["error_code"], "space_insufficient");
        assert_eq!(v["rollback"], "not_needed");
        assert!(v["observed_migrations"].is_null() && v["snapshot_id"].is_null());
    }

    /// Unreadable or invalid marker is `marker_invalid`, never "no marker".
    #[test]
    fn marker_read_errors_are_not_absence() {
        let td = tempfile::tempdir().unwrap();
        let p = marker_path(td.path());
        let mut v = serde_json::to_value(marker()).unwrap();
        v["format_version"] = 9.into();
        let mut extra = serde_json::to_value(marker()).unwrap();
        extra["unexpected"] = 1.into();
        for bad in [
            b"{trunc".to_vec(),
            vec![],
            serde_json::to_vec(&v).unwrap(),
            serde_json::to_vec(&extra).unwrap(),
        ] {
            fs::write(&p, bad).unwrap();
            assert!(read_marker(td.path()).is_err());
        }
        fs::remove_file(&p).unwrap();
        fs::create_dir(&p).unwrap();
        assert!(read_marker(td.path()).is_err(), "directory");
        fs::remove_dir(&p).unwrap();
        std::os::unix::fs::symlink(td.path().join("nowhere"), &p).unwrap();
        assert!(read_marker(td.path()).is_err(), "dangling symlink");
    }

    /// 5f-3 gate: the writer refuses any summary outside the closed set.
    #[test]
    fn writer_refuses_a_summary_outside_the_closed_set() {
        let td = tempfile::tempdir().unwrap();
        let mut m = marker();
        m.error_summary = "RAW-ERR /Users/jason/.agent24/agent24.db".into();
        assert!(write_marker(&RealOps, td.path(), &m).is_err());
        assert!(!marker_path(td.path()).exists());
        // Another code's text is not this code's text either.
        m.error_summary = DockCode::RollbackFailed.summary().into();
        assert!(write_marker(&RealOps, td.path(), &m).is_err());
        let all = [
            DockCode::Interrupted,
            DockCode::MigrationFailed,
            DockCode::RollbackFailed,
            DockCode::GuardInvalid,
            DockCode::PersistFailed,
            DockCode::MarkerInvalid,
            DockCode::SpaceInsufficient,
            DockCode::SpaceBudgetUnknown,
        ];
        let texts: std::collections::HashSet<_> = all.iter().map(|c| c.summary()).collect();
        assert_eq!(texts.len(), all.len(), "one distinct text per code");
        assert!(texts.iter().all(|t| t.len() <= 80 && !t.contains('/')));
    }

    /// 5f-3 gate: size cap, exactly at and over the limit; FIFO never blocks.
    #[test]
    fn read_marker_enforces_a_size_cap() {
        let td = tempfile::tempdir().unwrap();
        let p = marker_path(td.path());
        let json = serde_json::to_string(&marker()).unwrap();
        let pad = " ".repeat(MARKER_MAX_BYTES as usize - json.len());
        fs::write(&p, format!("{json}{pad}")).unwrap();
        assert!(read_marker(td.path()).unwrap().is_some(), "at the cap");
        fs::write(&p, format!("{json}{pad} ")).unwrap();
        assert!(read_marker(td.path()).is_err(), "one byte over");
        fs::remove_file(&p).unwrap();
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&p)
                .status()
                .unwrap()
                .success()
        );
        assert!(read_marker(td.path()).is_err(), "fifo");
    }

    #[test]
    fn hostile_identifiers_are_marker_invalid() {
        let td = tempfile::tempdir().unwrap();
        let p = marker_path(td.path());
        let edits: [fn(&mut MigrationMarker); 5] = [
            |m| m.attempt_id = "../../etc".into(),
            |m| m.attempt_id = "a".repeat(129),
            |m| m.snapshot_id = Some(String::new()),
            |m| m.snapshot_id = Some("x/y".into()),
            |m| m.failed_binary.version = "1.0 /Users/jason".into(),
        ];
        for edit in edits {
            let mut m = marker();
            edit(&mut m);
            fs::write(&p, serde_json::to_vec(&m).unwrap()).unwrap();
            assert!(read_marker(td.path()).is_err());
        }
        fs::write(&p, serde_json::to_vec(&marker()).unwrap()).unwrap();
        assert!(read_marker(td.path()).unwrap().is_some());
    }
}
