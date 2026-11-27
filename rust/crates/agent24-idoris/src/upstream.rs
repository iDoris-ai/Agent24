//! Loopback endpoint validation and ephemeral oMLX credentials.

use rustix::fs::{Mode, OFlags};
use serde::Deserialize;
use std::{fs::File, io::Read, path::Path};
use zeroize::Zeroizing;

const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Endpoint(url::Url);

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid loopback endpoint")]
pub(crate) struct InvalidEndpoint;

impl Endpoint {
    /// Accept an HTTP(S) origin with a literal loopback IP, never a DNS name.
    pub fn parse(input: &str) -> Result<Self, InvalidEndpoint> {
        let url = url::Url::parse(input).map_err(|_| InvalidEndpoint)?;
        // Check the original suffix too: URL parsing normalizes dot paths and
        // empty userinfo, which must not turn invalid input into a valid origin.
        let authority = input.split_once("://").ok_or(InvalidEndpoint)?.1;
        let suffix = authority.find(['/', '?', '#']).map(|i| &authority[i..]);
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if !matches!(url.scheme(), "http" | "https")
            || !loopback
            || input
                .contains(|c: char| c.is_whitespace() || c.is_control() || c == '\\' || c == '@')
            || !matches!(suffix, None | Some("/"))
            || url.port_or_known_default() == Some(0)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(InvalidEndpoint);
        }
        Ok(Self(url))
    }
    /// Configuration-only OpenAI-compatible origin; localhost is pinned to IPv4.
    pub fn parse_openai_compat(input: &str) -> Result<Self, InvalidEndpoint> {
        let normalized = input.replacen("://localhost", "://127.0.0.1", 1);
        let endpoint = Self::parse(&normalized)?;
        if endpoint.0.scheme() != "http"
            || !matches!(endpoint.0.host_str(), Some("127.0.0.1" | "[::1]"))
        {
            return Err(InvalidEndpoint);
        }
        Ok(endpoint)
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

pub struct ApiKey(Zeroizing<String>);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey([REDACTED])")
    }
}

impl ApiKey {
    /// Transfer the in-memory credential to setup's stdin writer.
    pub fn into_secret(self) -> Zeroizing<String> {
        self.0
    }

    /// The caller supplies the secret in memory, never via argv or environment.
    pub fn new(value: Zeroizing<String>) -> Result<Self, SettingsError> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.len() > 512 || trimmed.chars().any(char::is_control) {
            return Err(SettingsError::InvalidKey);
        }
        Ok(Self(Zeroizing::new(trimmed.to_owned())))
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("oMLX settings not found")]
    NotFound,
    #[error("oMLX settings cannot be read")]
    Unreadable,
    #[error("oMLX settings ownership, permissions or file type is unsafe")]
    UnsafeFile,
    #[error("oMLX settings exceed size limit")]
    TooLarge,
    #[error("oMLX settings contain invalid JSON")]
    InvalidJson,
    #[error("oMLX settings have no auth.api_key")]
    MissingKey,
    #[error("oMLX API key is invalid")]
    InvalidKey,
}

/// Resolve the effective user's home from the OS account database, not HOME.
pub fn read_settings_key() -> Result<ApiKey, SettingsError> {
    let uid = nix::unistd::geteuid();
    let user = nix::unistd::User::from_uid(uid)
        .map_err(|_| SettingsError::Unreadable)?
        .ok_or(SettingsError::Unreadable)?;
    read_settings_key_at(&user.dir.join(".omlx/settings.json"), uid.as_raw())
}

pub(crate) fn read_settings_key_at(path: &Path, euid: u32) -> Result<ApiKey, SettingsError> {
    let parent = path.parent().ok_or(SettingsError::UnsafeFile)?;
    let directory = rustix::fs::open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(settings_io)?;
    let meta = rustix::fs::fstat(&directory).map_err(settings_io)?;
    check_owner_permissions(meta.st_uid, meta.st_mode, euid)?;
    let name = path.file_name().ok_or(SettingsError::UnsafeFile)?;
    let fd = rustix::fs::openat(
        &directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(settings_io)?;
    let meta = rustix::fs::fstat(&fd).map_err(settings_io)?;
    check_owner_permissions(meta.st_uid, meta.st_mode, euid)?;
    if rustix::fs::FileType::from_raw_mode(meta.st_mode) != rustix::fs::FileType::RegularFile {
        return Err(SettingsError::UnsafeFile);
    }
    if meta.st_size < 0 || meta.st_size as u64 > MAX_SETTINGS_BYTES {
        return Err(SettingsError::TooLarge);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_SETTINGS_BYTES as usize + 1));
    // Cap the read independently of fstat: the file may grow after opening.
    File::from(fd)
        .take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SettingsError::Unreadable)?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(SettingsError::TooLarge);
    }
    let settings: Settings<'_> =
        serde_json::from_slice(&bytes).map_err(|_| SettingsError::InvalidJson)?;
    let raw = settings
        .auth
        .and_then(|auth| auth.api_key)
        .ok_or(SettingsError::MissingKey)?;
    let key = serde_json::from_str::<String>(raw.get()).map_err(|_| SettingsError::InvalidKey)?;
    ApiKey::new(Zeroizing::new(key))
}

fn check_owner_permissions(
    uid: u32,
    mode: rustix::fs::RawMode,
    euid: u32,
) -> Result<(), SettingsError> {
    if uid != euid || mode & 0o022 != 0 {
        Err(SettingsError::UnsafeFile)
    } else {
        Ok(())
    }
}

fn settings_io(error: rustix::io::Errno) -> SettingsError {
    match error {
        rustix::io::Errno::NOENT => SettingsError::NotFound,
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => SettingsError::UnsafeFile,
        _ => SettingsError::Unreadable,
    }
}

// Unknown fields are skipped by serde, never materialized as credential copies.
// RawValue borrows only auth.api_key from the zeroized input buffer.
#[derive(Deserialize)]
struct Settings<'a> {
    #[serde(borrow)]
    auth: Option<Auth<'a>>,
}

#[derive(Deserialize)]
struct Auth<'a> {
    #[serde(borrow)]
    api_key: Option<&'a serde_json::value::RawValue>,
}

#[cfg(test)]
mod tests;
