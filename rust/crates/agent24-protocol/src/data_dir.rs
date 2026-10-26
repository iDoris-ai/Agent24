//! The single platform data-directory resolver (ADR-037 decision 3).
//!
//! Shell and daemon share one rule, mirrored by `apps/desktop/src/main/data-dir.ts`
//! and locked to it by `protocol/fixtures/data-dir/cases.json`:
//!
//! 1. `AGENT24_DATA_DIR` (non-empty, absolute for the target platform) wins, as-is.
//! 2. macOS / Linux: `$HOME/.agent24` (unchanged since v0.1; no XDG migration).
//! 3. Windows: `%LOCALAPPDATA%\Agent24`. A missing / empty / relative
//!    `LOCALAPPDATA` is an error — it never falls back to `HOME`.
//!
//! Everything lives under it: discovery file, endpoints, logs, `os/<name>/`
//! (a manifest's `~/.agent24/os/<name>/` means `<data_dir>/os/<name>/`).

use std::ffi::OsString;
use std::path::PathBuf;

/// Environment variable that overrides the data root (MAINT-03).
pub const DATA_DIR_ENV: &str = "AGENT24_DATA_DIR";

/// Target platform of a resolution. Explicit so tests (and the shared
/// fixture) can exercise every branch on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Linux,
    Windows,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DataDirError {
    #[error("HOME is not set (or empty); set it or AGENT24_DATA_DIR")]
    HomeUnset,
    #[error("LOCALAPPDATA is not set (or empty / relative); set it or AGENT24_DATA_DIR")]
    LocalAppDataUnset,
}

impl DataDirError {
    /// Stable code shared with the TS implementation and the fixture.
    pub const fn code(self) -> &'static str {
        match self {
            Self::HomeUnset => "home_unset",
            Self::LocalAppDataUnset => "localappdata_unset",
        }
    }
}

/// Absolute-ness for the *target* platform, independent of the host.
fn is_absolute(platform: Platform, s: &str) -> bool {
    match platform {
        Platform::Windows => {
            let b = s.as_bytes();
            let drive = b.len() >= 3
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && (b[2] == b'\\' || b[2] == b'/');
            drive || s.starts_with("\\\\")
        }
        _ => s.starts_with('/'),
    }
}

/// Pure resolution. Non-UTF-8 values are treated as unset.
pub fn data_dir_from(
    platform: Platform,
    data_dir: Option<OsString>,
    home: Option<OsString>,
    local_app_data: Option<OsString>,
) -> Result<PathBuf, DataDirError> {
    let text = |v: Option<OsString>| {
        v.and_then(|v| v.into_string().ok())
            .filter(|v| !v.is_empty())
    };
    if let Some(dir) = text(data_dir)
        && is_absolute(platform, &dir)
    {
        return Ok(PathBuf::from(dir));
    }
    match platform {
        Platform::Windows => {
            let base = text(local_app_data)
                .filter(|b| is_absolute(platform, b))
                .ok_or(DataDirError::LocalAppDataUnset)?;
            let base = base.trim_end_matches(['\\', '/']);
            Ok(PathBuf::from(format!("{base}\\Agent24")))
        }
        Platform::Macos | Platform::Linux => {
            let home = text(home).ok_or(DataDirError::HomeUnset)?;
            Ok(PathBuf::from(home).join(".agent24"))
        }
    }
}

/// The data root for this process (reads the real environment).
pub fn data_dir() -> Result<PathBuf, DataDirError> {
    data_dir_from(
        Platform::current(),
        std::env::var_os(DATA_DIR_ENV),
        std::env::var_os("HOME"),
        std::env::var_os("LOCALAPPDATA"),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[derive(serde::Deserialize)]
    struct Fixture {
        cases: Vec<Case>,
    }
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        platform: String,
        env: std::collections::HashMap<String, String>,
        expect: Expect,
    }
    #[derive(serde::Deserialize)]
    struct Expect {
        path: Option<String>,
        error: Option<String>,
    }

    #[test]
    fn matches_shared_cross_language_fixture() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../protocol/fixtures/data-dir/cases.json");
        let fx: Fixture = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        assert!(fx.cases.len() >= 20, "fixture unexpectedly small");
        for c in fx.cases {
            let platform = match c.platform.as_str() {
                "macos" => Platform::Macos,
                "linux" => Platform::Linux,
                "windows" => Platform::Windows,
                other => panic!("{}: unknown platform {other}", c.name),
            };
            let get = |k: &str| c.env.get(k).map(OsString::from);
            let got = data_dir_from(
                platform,
                get(DATA_DIR_ENV),
                get("HOME"),
                get("LOCALAPPDATA"),
            );
            match (&c.expect.path, &c.expect.error) {
                (Some(path), None) => assert_eq!(
                    got.as_ref().map(|p| p.to_str().unwrap().to_owned()),
                    Ok(path.clone()),
                    "{}",
                    c.name
                ),
                (None, Some(code)) => {
                    assert_eq!(got.unwrap_err().code(), code, "{}", c.name);
                }
                _ => panic!("{}: expect needs exactly one of path/error", c.name),
            }
        }
    }

    /// Regression: existing mac/Linux users keep `~/.agent24`.
    #[test]
    fn default_path_is_unchanged_for_mac_and_linux() {
        for platform in [Platform::Macos, Platform::Linux] {
            assert_eq!(
                data_dir_from(platform, None, Some("/home/u".into()), None),
                Ok(PathBuf::from("/home/u/.agent24"))
            );
        }
    }

    #[test]
    fn data_dir_reads_the_real_environment() {
        assert_eq!(
            data_dir(),
            data_dir_from(
                Platform::current(),
                std::env::var_os(DATA_DIR_ENV),
                std::env::var_os("HOME"),
                std::env::var_os("LOCALAPPDATA"),
            )
        );
    }
}
