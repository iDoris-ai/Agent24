//! Loopback oMLX discovery and ephemeral credentials. No setup invocation.

use base64::{Engine, engine::general_purpose};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use rustix::fs::{Mode, OFlags};
use serde::Deserialize;
use std::{fs::File, io::Read, path::Path, time::Duration};
use zeroize::Zeroizing;

const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint(url::Url);

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid loopback endpoint")]
pub struct InvalidEndpoint;

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

    fn reflected_in(&self, value: &str) -> bool {
        let bytes = self.0.as_bytes();
        let hex = Zeroizing::new(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
        [
            self.0.clone(),
            Zeroizing::new(general_purpose::STANDARD_NO_PAD.encode(bytes)),
            Zeroizing::new(general_purpose::URL_SAFE.encode(bytes)),
            Zeroizing::new(hex.to_uppercase()),
            hex,
        ]
        .iter()
        .any(|encoded| value.contains(encoded.as_str()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceIdentity {
    Omlx,
    UnknownService,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProbeStatus {
    Ready { models: Vec<String> },
    AuthRequired,
    AuthRejected,
    UnknownService,
    Unreachable,
    InvalidResponse,
    HttpError(u16),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ProbeResult {
    pub endpoint: Endpoint,
    /// Independent of authentication: an unknown service may require auth.
    pub identity: ServiceIdentity,
    pub status: ProbeStatus,
}

pub struct OmlxProbe {
    client: reqwest::Client,
}

impl OmlxProbe {
    pub fn new() -> Result<Self, ProbeError> {
        Self::with_timeout(Duration::from_secs(2))
    }
    fn with_timeout(timeout: Duration) -> Result<Self, ProbeError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .map_err(|_| ProbeError)?;
        Ok(Self { client })
    }

    /// Probe all candidates in order. A key is eligible only for `confirmed`,
    /// and only after this call independently verifies that endpoint's identity.
    /// The returned list never implies a selected endpoint or default model.
    pub async fn probe(
        &self,
        confirmed: Option<&Endpoint>,
        key: Option<&ApiKey>,
    ) -> Vec<ProbeResult> {
        self.probe_endpoints(&candidates(confirmed), confirmed, key)
            .await
    }
    async fn probe_endpoints(
        &self,
        endpoints: &[Endpoint],
        confirmed: Option<&Endpoint>,
        key: Option<&ApiKey>,
    ) -> Vec<ProbeResult> {
        let mut results = Vec::with_capacity(endpoints.len());
        for endpoint in endpoints {
            let mut identity = ServiceIdentity::UnknownService;
            let status = match self.get(endpoint, "/v1/models", None).await {
                Err(status) => status,
                Ok(mut reply) => {
                    if self.identify(endpoint).await {
                        identity = ServiceIdentity::Omlx;
                    }
                    let mut used_key = None;
                    if identity == ServiceIdentity::Omlx
                        && confirmed == Some(endpoint)
                        && matches!(reply.code, 200 | 401 | 403)
                        && let Some(key) = key
                    {
                        used_key = Some(key);
                        match self.get(endpoint, "/v1/models", Some(key)).await {
                            Ok(authenticated) => reply = authenticated,
                            Err(status) => {
                                results.push(ProbeResult {
                                    endpoint: endpoint.clone(),
                                    identity,
                                    status,
                                });
                                continue;
                            }
                        }
                    }
                    classify(reply, identity, key, used_key.is_some())
                }
            };
            results.push(ProbeResult {
                endpoint: endpoint.clone(),
                identity,
                status,
            });
        }
        results
    }

    async fn identify(&self, endpoint: &Endpoint) -> bool {
        // Source: jundot/omlx@76e142bd30c5fa859014119032aec1d348814b72,
        // omlx/server.py: app = FastAPI(title="oMLX API"), /api/status,
        // /health and /v1/models. OpenAI compatibility or owned_by alone is
        // insufficient. A missing/disabled schema fails closed. This is a
        // service fingerprint, not cryptographic process authentication.
        let Ok(reply) = self.get(endpoint, "/openapi.json", None).await else {
            return false;
        };
        if reply.code != 200 {
            return false;
        }
        let Ok(schema) = serde_json::from_slice::<serde_json::Value>(&reply.body) else {
            return false;
        };
        schema
            .get("openapi")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v.starts_with("3."))
            && schema.pointer("/info/title").and_then(|v| v.as_str()) == Some("oMLX API")
            && schema
                .get("paths")
                .and_then(|v| v.as_object())
                .is_some_and(|paths| {
                    ["/v1/models", "/health", "/api/status"].iter().all(|path| {
                        paths
                            .get(*path)
                            .and_then(|p| p.get("get"))
                            .is_some_and(|get| get.is_object())
                    })
                })
    }

    async fn get(
        &self,
        endpoint: &Endpoint,
        path: &str,
        key: Option<&ApiKey>,
    ) -> Result<Reply, ProbeStatus> {
        let mut url = endpoint.0.clone();
        url.set_path(path);
        let mut request = self.client.get(url).header("Accept", "application/json");
        if let Some(key) = key {
            let bearer = Zeroizing::new(format!("Bearer {}", key.0.as_str()));
            let mut header =
                HeaderValue::from_str(&bearer).map_err(|_| ProbeStatus::InvalidResponse)?;
            header.set_sensitive(true);
            request = request.header(AUTHORIZATION, header);
        }
        // Transport errors and server bodies never enter public diagnostics.
        let mut response = request.send().await.map_err(|_| ProbeStatus::Unreachable)?;
        let code = response.status().as_u16();
        let mut body = Zeroizing::new(Vec::new());
        if response.status().is_success() {
            if response
                .content_length()
                .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
            {
                return Err(ProbeStatus::InvalidResponse);
            }
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| ProbeStatus::Unreachable)?
            {
                if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
                    return Err(ProbeStatus::InvalidResponse);
                }
                body.extend_from_slice(&chunk);
            }
        }
        Ok(Reply { code, body })
    }
}

struct Reply {
    code: u16,
    body: Zeroizing<Vec<u8>>,
}

#[derive(Deserialize)]
struct Models {
    data: Vec<Model>,
}
#[derive(Deserialize)]
struct Model {
    #[serde(deserialize_with = "zeroizing_string")]
    id: Zeroizing<String>,
}

fn zeroizing_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(deserializer).map(Zeroizing::new)
}

fn classify(
    reply: Reply,
    identity: ServiceIdentity,
    key: Option<&ApiKey>,
    authenticated: bool,
) -> ProbeStatus {
    match reply.code {
        401 | 403 => {
            if authenticated {
                ProbeStatus::AuthRejected
            } else {
                ProbeStatus::AuthRequired
            }
        }
        200 if identity == ServiceIdentity::Omlx => {
            let Ok(models) = serde_json::from_slice::<Models>(&reply.body) else {
                return ProbeStatus::InvalidResponse;
            };
            if models
                .data
                .iter()
                .any(|m| m.id.is_empty() || key.is_some_and(|k| k.reflected_in(&m.id)))
            {
                return ProbeStatus::InvalidResponse;
            }
            ProbeStatus::Ready {
                models: models.data.iter().map(|m| m.id.to_string()).collect(),
            }
        }
        200 => ProbeStatus::UnknownService,
        code => ProbeStatus::HttpError(code),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("could not initialize oMLX probe")]
pub struct ProbeError;

fn candidates(confirmed: Option<&Endpoint>) -> Vec<Endpoint> {
    let mut endpoints = confirmed.into_iter().cloned().collect::<Vec<_>>();
    for port in [8088, 8000, 8001] {
        // Fixed origins are validated through the same boundary as user input.
        if let Ok(endpoint) = Endpoint::parse(&format!("http://127.0.0.1:{port}"))
            && !endpoints.contains(&endpoint)
        {
            endpoints.push(endpoint);
        }
    }
    endpoints
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
/// The caller must explicitly bind this ephemeral key to a selected endpoint
/// when calling `probe`; reading settings never initiates an HTTP request.
pub fn read_settings_key() -> Result<ApiKey, SettingsError> {
    let uid = nix::unistd::geteuid();
    let user = nix::unistd::User::from_uid(uid)
        .map_err(|_| SettingsError::Unreadable)?
        .ok_or(SettingsError::Unreadable)?;
    read_settings_key_at(&user.dir.join(".omlx/settings.json"), uid.as_raw())
}

fn read_settings_key_at(path: &Path, euid: u32) -> Result<ApiKey, SettingsError> {
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
