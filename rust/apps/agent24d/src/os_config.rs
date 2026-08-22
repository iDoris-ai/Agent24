//! The domain-OS registry file — `~/.agent24/os.json` (ME-2).
//!
//! ME-1 made the kernel able to mount a module without knowing its name. This
//! makes WHICH modules are active a user decision instead of a build-time one,
//! which is what "换装是纯配置" means in practice.
//!
//! Same shape and home as `mcp.json` (see [`crate::mcp`]) on purpose: one place
//! to look, one format to learn.
//!
//! ```json
//! {
//!   "domainOs": {
//!     "sin90": { "enabled": true },
//!     "cos72": { "enabled": false }
//!   }
//! }
//! ```
//!
//! **A module absent from the file is ENABLED by default.** Every existing user
//! has no `os.json` at all, and defaulting to disabled would silently switch Sin90
//! off for all of them on upgrade — the file is a place to say NO, not a
//! registration requirement, and the fresh-install path needs no file at all. A
//! user who wants an allow-list sets `"default": "disabled"`, which matters more
//! as ME-3 lets modules arrive from outside the build: enumerating today's modules
//! as `false` cannot protect against one added tomorrow.
//!
//! Under the default `enabled` policy, a name the build does not provide that is
//! set to `false` is an ERROR: `sin09: false` looks exactly like a working config
//! while leaving `sin90` running — the same "a mistake silently keeps something on"
//! failure that justifies rejecting malformed JSON. Under `"default": "disabled"`
//! the same typo is harmless (the real module is unlisted, so it is off anyway) and
//! is not an error.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsConfig {
    /// What to do with a module the file does not mention. Defaults to
    /// [`DefaultPolicy::Enabled`] for compatibility (see the module doc), but is
    /// settable so a user CAN run deny-by-default — listing today's modules as
    /// `false` would not protect them from one added by a future build, and
    /// ME-3's out-of-process modules make that a real exposure rather than a
    /// theoretical one.
    #[serde(default)]
    default: DefaultPolicy,
    #[serde(rename = "domainOs", default)]
    modules: BTreeMap<String, ModuleEntry>,
}

/// What an UNLISTED module gets.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultPolicy {
    /// Anything not named in the file runs. The compatible default: nobody has an
    /// `os.json` today, so anything else would switch Sin90 off for every existing
    /// user at once.
    #[default]
    Enabled,
    /// Only modules explicitly listed with `enabled: true` run. What a user who
    /// wants an allow-list sets, and what ME-3 should encourage once modules can
    /// arrive from outside the build.
    Disabled,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleEntry {
    /// Set false to keep the entry in the file without mounting the module —
    /// mirroring `mcp.json`'s `enabled`, so "turn this off but remember it" is
    /// spelled the same way everywhere.
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

/// An exclusive, cross-process lock over the registry file.
///
/// `fs2` on a dedicated lock file — the same primitive the daemon singleton uses
/// (`state_file::try_acquire_singleton`), for the same reason: it is the only
/// kind that still holds when the second writer is a different PROCESS. Released
/// on drop, and by the OS if the holder dies, so a crashed writer cannot wedge
/// the registry.
///
/// It BLOCKS rather than failing fast. The critical section is one small read and
/// one small write; making a user re-run `agent24 os disable` because another
/// toggle was mid-flight would be worse than waiting a few milliseconds.
struct ConfigLock(std::fs::File);

impl ConfigLock {
    fn acquire(dir: &Path) -> Result<Self, String> {
        use fs2::FileExt;
        let path = dir.join("os.json.lock");
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

pub fn config_path() -> Option<PathBuf> {
    agent24_protocol::state_file::state_dir().map(|d| d.join("os.json"))
}

impl OsConfig {
    /// Read the registry. A MISSING file is the empty (all-default) config, not
    /// an error — that is the normal state for everyone who has never disabled a
    /// module. A malformed file IS an error: silently falling back to defaults
    /// would mount modules the user had explicitly switched off, which is the one
    /// mistake a config parser must not make.
    pub fn load(path: &Path) -> Result<Self, String> {
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

    /// Whether `name` should be mounted, given the file's [`default_policy`].
    ///
    /// [`default_policy`]: DefaultPolicy
    pub fn is_enabled(&self, name: &str) -> bool {
        match self.modules.get(name) {
            Some(e) => e.enabled,
            None => matches!(self.default, DefaultPolicy::Enabled),
        }
    }

    /// Names the file mentions. Used to report entries no build provides.
    pub fn named(&self) -> impl Iterator<Item = &str> {
        self.modules.keys().map(String::as_str)
    }

    /// Record a decision for `name` and write the file.
    ///
    /// The whole load-modify-write runs under an EXCLUSIVE, CROSS-PROCESS lock,
    /// and that is not belt-and-braces. An earlier version justified a lock-free
    /// write with "the daemon is a singleton", which is false twice over: axum
    /// handlers run concurrently, so two PATCHes in ONE daemon can both read the
    /// old file and the second silently drops the first; and ephemeral daemons are
    /// deliberately exempt from the singleton lock, so two CLI invocations with no
    /// persistent daemon are two PROCESSES writing the same file. Only a file lock
    /// covers both, which is why this is `fs2` rather than a `Mutex`.
    ///
    /// The write itself is temp-file-plus-rename, because this file's EMPTY state
    /// means "enable everything": a crash partway through a naive
    /// truncate-and-write would silently turn every disabled module back on.
    ///
    /// Returns the config as written.
    pub fn set_enabled(path: &Path, name: &str, enabled: bool) -> Result<Self, String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        let _guard = ConfigLock::acquire(parent)?;

        let mut cfg = Self::load(path)?;
        cfg.modules.insert(name.to_owned(), ModuleEntry { enabled });
        cfg.write_atomically(path, parent)?;
        Ok(cfg)
    }

    /// Caller must hold [`ConfigLock`].
    fn write_atomically(&self, path: &Path, parent: &Path) -> Result<(), String> {
        use std::io::Write;

        let body = serde_json::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize os.json: {e}"))?;

        // Beside the destination so the rename stays on one filesystem, and
        // `create_new` so a file already at that path — or a symlink planted at a
        // predictable name — is an error rather than something we write THROUGH,
        // truncating whatever it points at.
        let tmp = parent.join(format!("os.json.writing.{}", std::process::id()));
        // Two guards, doing DIFFERENT jobs — worth saying, because a test that
        // conflates them proves neither:
        //
        // - the clearing below handles a leftover from an interrupted write. A
        //   pid-keyed name makes that hazard worse exactly where it matters: a
        //   long-lived daemon keeps one pid, so without this every later PATCH
        //   would fail on `create_new` with no path to recovery. It is safe because
        //   the config lock is held, so nothing else can be mid-write to this name;
        //   and it removes a symlink rather than following it, so a planted link's
        //   target is untouched.
        // - `create_new` below is what closes the gap BETWEEN that removal and the
        //   open. It is not decoration, but it is also not what a test can easily
        //   reach now that the clearing runs first.
        // `symlink_metadata`, not `exists`: the latter FOLLOWS the link, so a
        // DANGLING symlink at this path would look absent, survive the clearing,
        // and then fail `create_new` forever.
        if std::fs::symlink_metadata(&tmp).is_ok() {
            tracing::warn!(
                "removing a stale {} left by an earlier interrupted write",
                tmp.display()
            );
            std::fs::remove_file(&tmp)
                .map_err(|e| format!("cannot clear stale {}: {e}", tmp.display()))?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;

        // Every failure past this point removes the temp file. Leaving a stale one
        // behind would make the NEXT write fail on `create_new`, turning one
        // transient error into a permanently stuck registry.
        let written = (|| -> std::io::Result<()> {
            f.write_all(body.as_bytes())?;
            f.write_all(b"\n")?;
            // The CONTENTS must be on disk before the rename publishes the name.
            // Syncing only the directory would publish a name pointing at data that
            // has not landed.
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
        // And the directory entry, so a power loss cannot lose a change the user
        // was told had been applied. PROPAGATED, not swallowed: reporting success
        // for a write that may not survive a reboot is the same class of lie the
        // atomic rename exists to avoid.
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

    /// Entries that say `enabled: false` for a name nothing provides, AND that
    /// therefore leave something running that the user meant to switch off.
    ///
    /// **Both halves of that sentence matter.** `sin09: false` looks exactly like a
    /// working config while `sin90` keeps serving — a config mistake that silently
    /// keeps something ON, the same failure that justifies rejecting malformed
    /// JSON, so it deserves the same answer rather than a warning nobody reads.
    ///
    /// But that danger exists ONLY under [`DefaultPolicy::Enabled`]. Under an
    /// allow-list, a misspelled name leaves the real module unlisted and therefore
    /// already disabled — the user's intent is satisfied, and failing the whole
    /// registry over a harmless tombstone would be worse than the bug. So this
    /// returns nothing under `Disabled`.
    ///
    /// An unknown ENABLED entry is harmless under either policy: it asks for
    /// something absent, and nothing happens. That stays a warning.
    ///
    /// This still costs a legitimate case under `Enabled`: downgrading to a build
    /// without a module you had disabled, or keeping a tombstone for one you plan
    /// to reinstall, both become errors. That is deliberate — the file cannot tell
    /// those apart from a typo — and the failure names the exact entry, so the fix
    /// is to delete that line or switch to `"default": "disabled"`.
    pub fn unknown_disabled<'a>(&'a self, provided: &'a BTreeSet<&str>) -> Vec<&'a str> {
        if matches!(self.default, DefaultPolicy::Disabled) {
            return Vec::new();
        }
        self.modules
            .iter()
            .filter(|(n, e)| !e.enabled && !provided.contains(n.as_str()))
            .map(|(n, _)| n.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Anything matching the production temp pattern, whatever pid it carries.
    fn leftover_temps(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("os.json.writing"))
            .collect()
    }

    fn write(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("os.json");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn a_missing_file_enables_everything() {
        // The upgrade path: nobody has an os.json today, and defaulting to
        // disabled would switch Sin90 off for every existing user at once.
        // An absent path INSIDE a temp dir, not a hardcoded global one: the latter
        // asserts that something does not exist on the machine running the test.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = OsConfig::load(&tmp.path().join("never-written.json")).unwrap();
        assert!(cfg.is_enabled("sin90"));
        assert!(cfg.is_enabled("anything-at-all"));
        assert_eq!(cfg.named().count(), 0);
    }

    #[test]
    fn an_empty_file_is_the_default_config() {
        let tmp = tempfile::tempdir().unwrap();
        for body in ["", "   \n\t "] {
            let p = write(tmp.path(), body);
            assert!(OsConfig::load(&p).unwrap().is_enabled("sin90"));
        }
    }

    #[test]
    fn deny_by_default_inverts_the_unlisted_case() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            r#"{"default": "disabled", "domainOs": {"wanted": {"enabled": true}}}"#,
        );
        let cfg = OsConfig::load(&p).unwrap();
        assert!(cfg.is_enabled("wanted"));
        assert!(
            !cfg.is_enabled("anything-else"),
            "an allow-list must not admit what it does not list"
        );
    }

    #[test]
    fn only_an_unknown_disabled_entry_is_flagged() {
        // The asymmetry is the point: an unknown `false` leaves the real module
        // RUNNING, an unknown `true` does nothing at all.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            r#"{"domainOs": {"sin09": {"enabled": false}, "cos07": {"enabled": true},
                            "sin90": {"enabled": false}}}"#,
        );
        let cfg = OsConfig::load(&p).unwrap();
        let provided: BTreeSet<&str> = ["sin90"].into_iter().collect();
        assert_eq!(cfg.unknown_disabled(&provided), vec!["sin09"]);
    }

    #[test]
    fn a_disabled_entry_is_honored_and_others_still_default_on() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            r#"{"domainOs": {"sin90": {"enabled": false}, "cos72": {}}}"#,
        );
        let cfg = OsConfig::load(&p).unwrap();
        assert!(!cfg.is_enabled("sin90"));
        assert!(cfg.is_enabled("cos72"), "an entry defaults to enabled");
        assert!(cfg.is_enabled("never-mentioned"));
        assert_eq!(cfg.named().collect::<Vec<_>>(), vec!["cos72", "sin90"]);
    }

    #[test]
    fn set_enabled_round_trips_and_leaves_no_temp_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("os.json");

        // From nothing at all — the first toggle a user ever makes.
        let cfg = OsConfig::set_enabled(&p, "sin90", false).unwrap();
        assert!(!cfg.is_enabled("sin90"));
        assert!(!OsConfig::load(&p).unwrap().is_enabled("sin90"));
        // Match what production actually names its temp (`os.json.writing.<pid>`):
        // an earlier version of this assertion looked for `os.json.writing`, which
        // no code has ever created, so it could not have caught a leak.
        assert!(
            leftover_temps(tmp.path()).is_empty(),
            "no temp file may survive a successful write: {:?}",
            leftover_temps(tmp.path())
        );

        // And back, without disturbing other entries.
        OsConfig::set_enabled(&p, "cos72", false).unwrap();
        let cfg = OsConfig::set_enabled(&p, "sin90", true).unwrap();
        assert!(cfg.is_enabled("sin90"));
        assert!(!cfg.is_enabled("cos72"), "the other entry survived");
        assert_eq!(
            OsConfig::load(&p).unwrap().named().collect::<Vec<_>>(),
            vec!["cos72", "sin90"]
        );
    }

    #[test]
    fn concurrent_writers_do_not_lose_an_update() {
        // The failure the lock exists to prevent, exercised for real: without it
        // both threads read the same starting file and the second rename silently
        // discards the first's entry. Threads rather than tasks, because the lock
        // is a BLOCKING file lock — the thing under test is that two OS-level
        // writers serialize.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("os.json");
        std::fs::write(&p, "{}").unwrap();

        let names = ["alpha", "beta", "gamma", "delta"];
        std::thread::scope(|scope| {
            for n in names {
                let p = p.clone();
                scope.spawn(move || {
                    OsConfig::set_enabled(&p, n, false).unwrap();
                });
            }
        });

        let cfg = OsConfig::load(&p).unwrap();
        for n in names {
            assert!(!cfg.is_enabled(n), "{n} lost its update");
        }
        assert!(
            leftover_temps(tmp.path()).is_empty(),
            "and no temp file was left behind"
        );
    }

    #[test]
    fn a_stale_temp_from_an_interrupted_write_does_not_wedge_the_registry() {
        // `create_new` is what stops a write following a planted symlink, and the
        // price is that a leftover temp makes the NEXT write fail. With a pid-keyed
        // name a long-lived daemon keeps failing forever, which is the state the
        // guard's own comment says it exists to avoid — so the leftover is cleared.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("os.json");
        let stale = tmp
            .path()
            .join(format!("os.json.writing.{}", std::process::id()));
        std::fs::write(&stale, b"garbage from a crashed write").unwrap();

        let cfg = OsConfig::set_enabled(&p, "sin90", false).unwrap();
        assert!(!cfg.is_enabled("sin90"));
        assert!(leftover_temps(tmp.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_temp_path_is_removed_not_followed() {
        // Named for what it PROVES. An earlier version of this test claimed to pin
        // `create_new` as load-bearing, and it did not: the staleness clearing runs
        // first and removes the link, so mutating `create_new` away left the test
        // green. Mutating the CLEARING away is what breaks it.
        //
        // `remove_file` unlinks the symlink itself, never its target — which is why
        // the victim below survives.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("os.json");
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, b"do not touch").unwrap();
        std::os::unix::fs::symlink(
            &victim,
            tmp.path()
                .join(format!("os.json.writing.{}", std::process::id())),
        )
        .unwrap();

        OsConfig::set_enabled(&p, "sin90", false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not touch",
            "the symlink target must not have been written through"
        );
        assert!(
            !std::fs::symlink_metadata(&p)
                .unwrap()
                .file_type()
                .is_symlink(),
            "and os.json must be a real file, not a link"
        );
        assert!(!OsConfig::load(&p).unwrap().is_enabled("sin90"));
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_at_the_temp_path_does_not_wedge_the_registry() {
        // The case `exists()` cannot see: it follows the link, so a link to nothing
        // reads as absent, survives the clearing, and then fails `create_new` on
        // every subsequent write — permanently, for a daemon that keeps its pid.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("os.json");
        std::os::unix::fs::symlink(
            tmp.path().join("nothing-here"),
            tmp.path()
                .join(format!("os.json.writing.{}", std::process::id())),
        )
        .unwrap();

        OsConfig::set_enabled(&p, "sin90", false).unwrap();
        assert!(!OsConfig::load(&p).unwrap().is_enabled("sin90"));
        assert!(
            !tmp.path().join("nothing-here").exists(),
            "and nothing was created through the link"
        );
        assert!(leftover_temps(tmp.path()).is_empty());
    }

    #[test]
    fn set_enabled_preserves_the_default_policy() {
        // Losing this on a write would silently flip an allow-list back to
        // allow-everything — the exact failure the policy exists to prevent.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), r#"{"default": "disabled", "domainOs": {}}"#);
        OsConfig::set_enabled(&p, "sin90", true).unwrap();
        let cfg = OsConfig::load(&p).unwrap();
        assert!(cfg.is_enabled("sin90"));
        assert!(
            !cfg.is_enabled("something-else"),
            "the allow-list policy must survive the write"
        );
    }

    #[test]
    fn a_write_refuses_rather_than_truncating_an_unreadable_file() {
        // The file's EMPTY state means "enable everything", so a partial write
        // would silently turn every disabled module back on. `set_enabled` reads
        // first and fails without touching anything.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "{ not json");
        assert!(OsConfig::set_enabled(&p, "sin90", false).is_err());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "{ not json",
            "the unreadable file is left exactly as found"
        );
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_silent_default() {
        // Falling back to defaults here would MOUNT a module the user had
        // explicitly switched off — the one failure mode a config parser must not
        // have. Better to refuse and say which file is wrong.
        let tmp = tempfile::tempdir().unwrap();
        for body in [
            "{ not json",
            r#"{"domainOs": {"sin90": {"enable": false}}}"#, // typo'd key
            r#"{"domain_os": {}}"#,                          // wrong top-level key
            r#"{"domainOs": {"sin90": true}}"#,              // wrong entry shape
        ] {
            let p = write(tmp.path(), body);
            let err = OsConfig::load(&p).unwrap_err();
            assert!(err.contains("os.json"), "{body}: {err}");
        }
    }

    #[test]
    fn an_unreadable_file_is_an_error_not_a_missing_one() {
        // A directory where the file should be: `read_to_string` fails with
        // something other than NotFound, and treating that as "no config" would
        // again mount a disabled module.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("os.json");
        std::fs::create_dir(&p).unwrap();
        assert!(OsConfig::load(&p).is_err());
    }
}
