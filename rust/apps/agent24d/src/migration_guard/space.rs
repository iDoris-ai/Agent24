//! Space budget (design D2, section 8 R2): `2*S + G + R`.
//!
//! S = main file + WAL of both databases, G = declared extra space of the
//! pending migrations, R = max(64 MiB, 10% of S). Free space is measured on
//! the filesystem holding `data_dir`. The restore budget is held by a really
//! allocated reservation file, never a sparse one.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const MIN_RESERVE: u64 = 64 * 1024 * 1024;

/// Declared extra space of one migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GDecl {
    Bytes(u64),
    /// Conservative multiple of S (until a large-database fixture peak exists).
    MultipleOfS(u64),
}

/// Default for an undeclared migration (section 8 R2): G = 2*S.
pub const DEFAULT_G: GDecl = GDecl::MultipleOfS(2);

/// G declaration table: (database, migration version, G). Entries for the
/// rewrite/FTS migrations are conservative multiples, NOT yet measured on a
/// large-database fixture: 待大库夹具实测替换 (replace with measured peaks).
pub const G_DECLARATIONS: &[(&str, i64, GDecl)] = &[
    ("memory.db", 5, GDecl::MultipleOfS(2)), // FTS backfill from mem_assertions
    ("memory.db", 10, GDecl::MultipleOfS(2)), // table rewrite
    ("memory.db", 11, GDecl::MultipleOfS(2)), // table rewrite (mem_events)
    ("memory.db", 13, GDecl::MultipleOfS(2)), // table rewrite
    ("memory.db", 15, GDecl::MultipleOfS(2)), // table rewrite
    ("memory.db", 17, GDecl::MultipleOfS(4)), // CJK bigram FTS rebuild
];

#[derive(Clone, Copy, Debug)]
pub struct MigrationRef<'a> {
    pub database: &'static str,
    pub version: i64,
    pub sql: &'a str,
}

#[must_use]
pub fn declared_g(database: &str, version: i64) -> Option<GDecl> {
    G_DECLARATIONS
        .iter()
        .find(|(d, v, _)| *d == database && *v == version)
        .map(|(_, _, g)| *g)
}

/// FTS rebuild or whole-table rewrite (SQL comments ignored): must carry an explicit G declaration.
#[must_use]
pub fn requires_declaration(sql: &str) -> bool {
    let s: String = sql
        .lines()
        .map(|l| l.split("--").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    s.contains("fts") || s.contains("rebuild") || s.contains("drop table")
}

/// CI gate: every migration needing a declaration has one. Returns the
/// offenders as `database:version`.
pub fn check_declarations(all: &[MigrationRef<'_>]) -> Result<(), Vec<String>> {
    let bad: Vec<String> = all
        .iter()
        .filter(|m| requires_declaration(m.sql) && declared_g(m.database, m.version).is_none())
        .map(|m| format!("{}:{}", m.database, m.version))
        .collect();
    if bad.is_empty() { Ok(()) } else { Err(bad) }
}

/// Digest of the migration manifest (database, version, SQL hash), for the
/// snapshot manifest.
#[must_use]
pub fn migrations_digest(all: &[MigrationRef<'_>]) -> String {
    let mut h = Sha256::new();
    for m in all {
        h.update(format!("{}:{}:", m.database, m.version));
        h.update(Sha256::digest(m.sql.as_bytes()));
    }
    format!("{:x}", h.finalize())
}

#[must_use]
pub fn resolve_g(pending: &[MigrationRef<'_>], s: u64) -> u64 {
    pending
        .iter()
        .map(
            |m| match declared_g(m.database, m.version).unwrap_or(DEFAULT_G) {
                GDecl::Bytes(b) => b,
                GDecl::MultipleOfS(k) => k.saturating_mul(s),
            },
        )
        .max()
        .unwrap_or(0)
}

#[must_use]
pub fn reserve_r(s: u64) -> u64 {
    MIN_RESERVE.max(s / 10)
}

#[must_use]
pub fn required_bytes(s: u64, g: u64) -> u64 {
    s.saturating_mul(2)
        .saturating_add(g)
        .saturating_add(reserve_r(s))
}

/// S: main file + `-wal` of each path. Missing files count 0. A database
/// that exists but is read-only is reported as an error (dock).
pub fn measure_s(dbs: &[&Path]) -> Result<u64, String> {
    let mut total = 0u64;
    for db in dbs {
        match fs::metadata(db) {
            Ok(m) => {
                if m.permissions().readonly() {
                    return Err(format!("{} is read-only", db.display()));
                }
                total = total.saturating_add(m.len());
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", db.display())),
        }
        let mut wal = db.as_os_str().to_owned();
        wal.push("-wal");
        match fs::metadata(&wal) {
            Ok(m) => total = total.saturating_add(m.len()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", Path::new(&wal).display())),
        }
    }
    Ok(total)
}

pub trait FreeSpace {
    fn available(&self, dir: &Path) -> io::Result<u64>;
}

/// `statvfs` on the filesystem holding the directory. Uses `f_bavail` (blocks
/// available to unprivileged processes), not `f_bfree` (which includes the
/// root-reserved blocks we cannot actually use).
#[derive(Debug, Default, Clone, Copy)]
pub struct StatvfsFree;

impl FreeSpace for StatvfsFree {
    fn available(&self, dir: &Path) -> io::Result<u64> {
        let st = rustix::fs::statvfs(dir)?;
        Ok(st.f_bavail.saturating_mul(st.f_frsize))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SpaceVerdict {
    Ok {
        required: u64,
        available: u64,
    },
    Insufficient {
        required: u64,
        available: u64,
    },
    /// Cannot measure (read-only db, statvfs failure): dock.
    Unmeasurable(String),
}

pub fn precheck(
    data_dir: &Path,
    dbs: &[&Path],
    pending: &[MigrationRef<'_>],
    fs_free: &dyn FreeSpace,
) -> SpaceVerdict {
    let s = match measure_s(dbs) {
        Ok(s) => s,
        Err(e) => return SpaceVerdict::Unmeasurable(e),
    };
    let required = required_bytes(s, resolve_g(pending, s));
    match fs_free.available(data_dir) {
        Err(e) => SpaceVerdict::Unmeasurable(format!("statvfs {}: {e}", data_dir.display())),
        Ok(available) if available >= required => SpaceVerdict::Ok {
            required,
            available,
        },
        Ok(available) => SpaceVerdict::Insufficient {
            required,
            available,
        },
    }
}

/// A really allocated reservation file holding the restore budget.
#[derive(Debug)]
pub struct Reservation {
    path: PathBuf,
}

impl Reservation {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Delete the file, freeing the budget (call before restoring on ENOSPC).
    pub fn release(self) -> io::Result<()> {
        fs::remove_file(&self.path)
    }
}

/// Allocate `bytes` of real blocks at `path` (0600, exclusive). Linux uses
/// `fallocate`; elsewhere (no safe preallocate API under `forbid(unsafe)`)
/// real zero bytes are written, which also allocate blocks. Known cost: on
/// macOS this writes `bytes` of zeros (seconds per few GB at disk speed), so
/// a very large S delays the pre-migration step; there is no safe-Rust
/// `F_PREALLOCATE`.
pub fn reserve(path: &Path, bytes: u64) -> io::Result<Reservation> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let res = allocate(&mut f, bytes).and_then(|()| f.sync_all());
    if let Err(e) = res {
        drop(f);
        let _ = fs::remove_file(path);
        return Err(e);
    }
    Ok(Reservation {
        path: path.to_path_buf(),
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn allocate(f: &mut fs::File, bytes: u64) -> io::Result<()> {
    rustix::fs::fallocate(&*f, rustix::fs::FallocateFlags::empty(), 0, bytes)?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn allocate(f: &mut fs::File, bytes: u64) -> io::Result<()> {
    use std::io::Write;
    let chunk = vec![0u8; 1 << 20];
    let mut left = bytes;
    while left > 0 {
        let n = left.min(chunk.len() as u64) as usize;
        f.write_all(&chunk[..n])?;
        left -= n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::os::unix::fs::MetadataExt;

    struct Fixed(io::Result<u64>);
    impl FreeSpace for Fixed {
        fn available(&self, _: &Path) -> io::Result<u64> {
            match &self.0 {
                Ok(v) => Ok(*v),
                Err(e) => Err(io::Error::new(e.kind(), "x")),
            }
        }
    }

    fn db_of(dir: &Path, len: u64) -> PathBuf {
        let p = dir.join("agent24.db");
        fs::write(&p, vec![1u8; len as usize]).unwrap();
        p
    }

    #[test]
    fn formula_boundaries() {
        assert_eq!(reserve_r(0), MIN_RESERVE);
        assert_eq!(reserve_r(MIN_RESERVE * 100), MIN_RESERVE * 10);
        let s = 1000;
        assert_eq!(required_bytes(s, 500), 2000 + 500 + MIN_RESERVE);
        assert_eq!(required_bytes(u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn precheck_one_byte_short_fails_exact_passes() {
        let td = tempfile::tempdir().unwrap();
        let db = db_of(td.path(), 1000);
        let need = required_bytes(1000, 2000); // undeclared => G = 2*S
        let pend = [MigrationRef {
            database: "agent24.db",
            version: 1,
            sql: "select 1",
        }];
        let run = |avail| precheck(td.path(), &[&db], &pend, &Fixed(Ok(avail)));
        assert_eq!(
            run(need),
            SpaceVerdict::Ok {
                required: need,
                available: need
            }
        );
        assert_eq!(
            run(need - 1),
            SpaceVerdict::Insufficient {
                required: need,
                available: need - 1
            }
        );
    }

    #[test]
    fn unmeasurable_cases_dock() {
        let td = tempfile::tempdir().unwrap();
        let db = db_of(td.path(), 10);
        let v = precheck(td.path(), &[&db], &[], &Fixed(Err(io::Error::other("e"))));
        assert!(matches!(v, SpaceVerdict::Unmeasurable(_)));
        let mut perm = fs::metadata(&db).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&db, perm).unwrap();
        let v = precheck(td.path(), &[&db], &[], &Fixed(Ok(u64::MAX)));
        assert!(matches!(v, SpaceVerdict::Unmeasurable(_)));
    }

    #[test]
    fn s_counts_wal_and_missing_is_zero() {
        let td = tempfile::tempdir().unwrap();
        let db = db_of(td.path(), 100);
        fs::write(td.path().join("agent24.db-wal"), vec![0u8; 50]).unwrap();
        let none = td.path().join("memory.db");
        assert_eq!(measure_s(&[&db, &none]).unwrap(), 150);
    }

    #[test]
    fn real_statvfs_is_positive() {
        let td = tempfile::tempdir().unwrap();
        assert!(StatvfsFree.available(td.path()).unwrap() > 0);
    }

    #[test]
    fn declaration_gate_covers_embedded_migrations() {
        let store = sqlx::migrate!("../../crates/agent24-store/migrations");
        let memory = sqlx::migrate!("../../crates/agent24-memory/migrations");
        let mut all = vec![];
        for (db, m) in [("agent24.db", &store), ("memory.db", &memory)] {
            for mg in m.iter() {
                all.push(MigrationRef {
                    database: db,
                    version: mg.version,
                    sql: &mg.sql,
                });
            }
        }
        assert!(all.len() > 30);
        check_declarations(&all).unwrap();
        // positive control: the real FTS rebuild migration is detected.
        assert!(
            all.iter()
                .any(|m| m.version == 17 && requires_declaration(m.sql))
        );
        // every table entry names an existing migration.
        for (d, v, _) in G_DECLARATIONS {
            assert!(
                all.iter().any(|m| m.database == *d && m.version == *v),
                "{d}:{v}"
            );
        }
    }

    #[test]
    fn undeclared_fts_migration_fails_gate() {
        let fake = [
            MigrationRef {
                database: "memory.db",
                version: 999,
                sql: "INSERT INTO x_fts(x_fts) VALUES('rebuild')",
            },
            MigrationRef {
                database: "memory.db",
                version: 17,
                sql: "drop table t",
            },
        ];
        assert_eq!(check_declarations(&fake).unwrap_err(), ["memory.db:999"]);
    }

    #[test]
    fn g_resolution_uses_declared_else_default() {
        let m17 = MigrationRef {
            database: "memory.db",
            version: 17,
            sql: "",
        };
        let plain = MigrationRef {
            database: "agent24.db",
            version: 1,
            sql: "",
        };
        assert_eq!(resolve_g(&[plain], 10), 20);
        assert_eq!(resolve_g(&[plain, m17], 10), 40);
        assert_eq!(resolve_g(&[], 10), 0);
    }

    #[test]
    fn digest_changes_with_sql() {
        let a = [MigrationRef {
            database: "d",
            version: 1,
            sql: "a",
        }];
        let b = [MigrationRef {
            database: "d",
            version: 1,
            sql: "b",
        }];
        assert_ne!(migrations_digest(&a), migrations_digest(&b));
    }

    #[test]
    fn reservation_is_really_allocated_and_released() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("restore.reserve");
        let bytes = 3 * 1024 * 1024;
        let r = reserve(&p, bytes).unwrap();
        let md = fs::metadata(&p).unwrap();
        assert_eq!(md.len(), bytes);
        assert!(md.blocks() * 512 >= bytes, "sparse: blocks={}", md.blocks());
        assert!(reserve(&p, 1).is_err(), "exclusive create");
        r.release().unwrap();
        assert!(!p.exists());
    }
}
