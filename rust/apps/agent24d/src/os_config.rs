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

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Deserialize)]
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
