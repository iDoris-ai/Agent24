//! The transport seam (KB-C1-02a, last slice): one byte-level request to the
//! knowledge server. The transport only moves bytes; decoding, the version and
//! capability checks, the echo and scope checks and the length check before
//! sending all belong to the Provider, so every transport is held to them once.
//!
//! Where a request goes is fixed by the transport's own trusted configuration
//! (base URL, credential), never by the caller: the trait takes an operation, not
//! a URL. A real transport therefore never follows a redirect (the credential
//! header would go along to wherever it points; a 3xx comes back as a
//! [`TransportResponse`]), and never resends on its own: a retry is a new request
//! with a new `request_id` (R6.6), decided by the Provider.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;

/// The four v1 operations, each with a fixed method and path under the API base
/// (`.../api/v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KnowledgeOperation {
    /// `GET /system/capabilities`; the declaration is
    /// `data.integration.agent24_knowledge` inside the upstream `{code, msg, data}`
    /// wrapper (R9.1, R9.2).
    Capabilities,
    /// `POST /agent24/knowledge/ready`.
    Ready,
    /// `POST /agent24/knowledge/search`.
    Search,
    /// `POST /agent24/knowledge/read`.
    Read,
}

impl KnowledgeOperation {
    pub const ALL: [Self; 4] = [Self::Capabilities, Self::Ready, Self::Search, Self::Read];

    pub fn method(self) -> &'static str {
        match self {
            Self::Capabilities => "GET",
            Self::Ready | Self::Search | Self::Read => "POST",
        }
    }

    /// Path relative to the API base, starting with `/`.
    pub fn path(self) -> &'static str {
        match self {
            Self::Capabilities => "/system/capabilities",
            Self::Ready => "/agent24/knowledge/ready",
            Self::Search => "/agent24/knowledge/search",
            Self::Read => "/agent24/knowledge/read",
        }
    }

    /// Whether the request carries a JSON body (`POST`).
    pub fn has_body(self) -> bool {
        self.method() == "POST"
    }
}

/// What the server answered: any HTTP status with its body, unparsed. A non-2xx
/// status is a response, not a [`TransportError`]: the error envelope is in the
/// body and the Provider decodes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Why no response came back. Carries no URL, header or body text, so it is safe
/// to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The deadline passed before the whole response arrived.
    Timeout,
    /// No connection could be made, or it broke before a response.
    Unreachable,
    /// The response was larger than the transport accepts.
    ResponseTooLarge,
    /// Anything else, as a fixed category name.
    Other(&'static str),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("knowledge transport: deadline exceeded"),
            Self::Unreachable => f.write_str("knowledge transport: server unreachable"),
            Self::ResponseTooLarge => f.write_str("knowledge transport: response too large"),
            Self::Other(what) => write!(f, "knowledge transport: {what}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// One request to the knowledge server. `body` is sent as-is with
/// `Content-Type: application/json` when [`KnowledgeOperation::has_body`], and
/// must be empty otherwise. The whole exchange must finish within `deadline`,
/// or the call returns [`TransportError::Timeout`]. Dropping the future abandons
/// the request.
#[async_trait]
pub trait KnowledgeTransport: Send + Sync {
    async fn send(
        &self,
        op: KnowledgeOperation,
        body: Vec<u8>,
        deadline: Duration,
    ) -> Result<TransportResponse, TransportError>;
}
