//! [`KnowledgeTransport`] over HTTP (KB-C1-02b). The transport boundary of
//! CONTRACT-READONLY-v1: the credential goes only to the configured base URL
//! (R1.7), so the client takes no proxy from the environment and follows no
//! redirect; the whole exchange is bounded by the deadline and the body by a
//! byte limit, read as a stream and dropped as soon as it goes over.
//!
//! Cancelling is dropping the future returned by [`KnowledgeTransport::send`]:
//! the request runs inside that future (nothing is spawned), so dropping it
//! closes the connection and stops reading. Matching `request_id` and
//! `generation` and discarding late responses (R7.3) belong to the Provider.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue};
use reqwest::{Client, Method, Url, redirect, tls};

use url::Host;

use crate::{KnowledgeOperation, KnowledgeTransport, TransportError, TransportResponse};

/// The credential the transport presents on every request. `Debug` never
/// prints the secret.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// `Authorization: Bearer <token>` (a signed token or JWT).
    Bearer(String),
    /// `X-API-Key: <key>` (a tenant key).
    ApiKey(String),
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("Credential::Bearer(<redacted>)"),
            Self::ApiKey(_) => f.write_str("Credential::ApiKey(<redacted>)"),
        }
    }
}

/// The transport's trusted configuration; the caller of `send` cannot change
/// where a request goes.
#[derive(Debug, Clone)]
pub struct HttpTransportConfig {
    /// The API base, e.g. `https://kb.example.com/api/v1`. `https`, or `http`
    /// only to a loopback IP literal (`127.0.0.0/8`, `::1`); no user info,
    /// query or fragment.
    pub base_url: String,
    pub credential: Credential,
    /// Largest response body accepted, in bytes.
    pub max_response_bytes: usize,
    /// Bound on making the TCP and TLS connection; the deadline passed to
    /// `send` still bounds the whole exchange.
    pub connect_timeout: Duration,
}

/// Why a configuration was refused. Carries no URL or secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpConfigError {
    /// Not an absolute URL that can carry a path.
    InvalidUrl,
    /// `http` to anything but a loopback IP literal, or another scheme.
    InsecureScheme,
    /// The URL has a user name or password.
    UrlHasUserInfo,
    /// The URL has a query or fragment.
    UrlHasQueryOrFragment,
    /// The credential is empty or not a valid header value.
    InvalidCredential,
    /// `max_response_bytes` is 0.
    InvalidLimit,
    /// The HTTP client could not be built.
    Client,
}

impl fmt::Display for HttpConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self {
            Self::InvalidUrl => "base URL is not an absolute URL",
            Self::InsecureScheme => "base URL must be https, or http to a loopback IP",
            Self::UrlHasUserInfo => "base URL must not carry user info",
            Self::UrlHasQueryOrFragment => "base URL must not carry a query or fragment",
            Self::InvalidCredential => "credential is empty or not a valid header value",
            Self::InvalidLimit => "max_response_bytes must be positive",
            Self::Client => "HTTP client could not be built",
        };
        write!(f, "knowledge transport config: {what}")
    }
}

impl std::error::Error for HttpConfigError {}

/// The HTTP [`KnowledgeTransport`]. Build it once and share it.
pub struct HttpTransport {
    client: Client,
    /// Normalized base without a trailing `/`; an operation path is appended.
    base: String,
    auth_name: HeaderName,
    auth_value: HeaderValue,
    max_response_bytes: usize,
}

impl fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpTransport")
            .field("base", &self.base)
            .field("auth_name", &self.auth_name)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl HttpTransport {
    pub fn new(config: HttpTransportConfig) -> Result<Self, HttpConfigError> {
        let base = validate_base_url(&config.base_url)?;
        if config.max_response_bytes == 0 {
            return Err(HttpConfigError::InvalidLimit);
        }
        let (auth_name, auth_value) = credential_header(&config.credential)?;
        let client = Client::builder()
            .use_rustls_tls()
            .min_tls_version(tls::Version::TLS_1_2)
            .no_proxy()
            .redirect(redirect::Policy::none())
            .referer(false)
            .connect_timeout(config.connect_timeout)
            .build()
            .map_err(|_| HttpConfigError::Client)?;
        Ok(Self {
            client,
            base,
            auth_name,
            auth_value,
            max_response_bytes: config.max_response_bytes,
        })
    }

    async fn exchange(
        &self,
        op: KnowledgeOperation,
        body: Vec<u8>,
    ) -> Result<TransportResponse, TransportError> {
        let method = if op.has_body() {
            Method::POST
        } else {
            Method::GET
        };
        let mut request = self
            .client
            .request(method, format!("{}{}", self.base, op.path()))
            .header(ACCEPT, "application/json")
            .header(self.auth_name.clone(), self.auth_value.clone());
        if op.has_body() {
            request = request.header(CONTENT_TYPE, "application/json").body(body);
        }
        let mut response = request.send().await.map_err(|e| send_error(&e))?;
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|n| n > self.max_response_bytes as u64)
        {
            return Err(TransportError::ResponseTooLarge);
        }
        let mut out = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| body_error(&e))? {
            if chunk.len() > self.max_response_bytes - out.len() {
                return Err(TransportError::ResponseTooLarge);
            }
            out.extend_from_slice(&chunk);
        }
        Ok(TransportResponse { status, body: out })
    }
}

#[async_trait]
impl KnowledgeTransport for HttpTransport {
    async fn send(
        &self,
        op: KnowledgeOperation,
        body: Vec<u8>,
        deadline: Duration,
    ) -> Result<TransportResponse, TransportError> {
        if !op.has_body() && !body.is_empty() {
            return Err(TransportError::Other("body on a request without one"));
        }
        tokio::time::timeout(deadline, self.exchange(op, body))
            .await
            .unwrap_or(Err(TransportError::Timeout))
    }
}

/// Checks the base URL and returns it without a trailing `/`.
fn validate_base_url(raw: &str) -> Result<String, HttpConfigError> {
    let url = Url::parse(raw).map_err(|_| HttpConfigError::InvalidUrl)?;
    if url.cannot_be_a_base() {
        return Err(HttpConfigError::InvalidUrl);
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_literal(&url) => {}
        _ => return Err(HttpConfigError::InsecureScheme),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(HttpConfigError::UrlHasUserInfo);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(HttpConfigError::UrlHasQueryOrFragment);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// A loopback IP literal. A host name such as `localhost` does not count: it
/// is resolved at connect time and could point elsewhere.
fn is_loopback_literal(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        _ => false,
    }
}

fn credential_header(
    credential: &Credential,
) -> Result<(HeaderName, HeaderValue), HttpConfigError> {
    let (name, value) = match credential {
        Credential::Bearer(token) if !token.is_empty() => {
            (AUTHORIZATION, format!("Bearer {token}"))
        }
        Credential::ApiKey(key) if !key.is_empty() => {
            (HeaderName::from_static("x-api-key"), key.clone())
        }
        _ => return Err(HttpConfigError::InvalidCredential),
    };
    let mut value =
        HeaderValue::from_str(&value).map_err(|_| HttpConfigError::InvalidCredential)?;
    value.set_sensitive(true);
    Ok((name, value))
}

/// Maps a failure before the response head arrived. reqwest's error text holds
/// the URL, so only a category goes out.
fn send_error(e: &reqwest::Error) -> TransportError {
    if e.is_timeout() {
        TransportError::Timeout
    } else if e.is_connect() || e.is_request() {
        TransportError::Unreachable
    } else {
        TransportError::Other("request failed")
    }
}

/// Maps a failure while reading the body.
fn body_error(e: &reqwest::Error) -> TransportError {
    if e.is_timeout() {
        TransportError::Timeout
    } else {
        TransportError::Other("response body interrupted")
    }
}
