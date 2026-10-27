//! One request to the daemon and its reply, as the import logic sees them.

use std::future::Future;

use crate::response::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// A request under `/api/v1/documents`. `path` is the full path from the
/// root (`/api/v1/documents/uploads`, ...); the transport adds the bearer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// The daemon's reply, or a transport failure the transport describes
/// itself: `backend_unreachable` (not sent, or the reply was cut short),
/// `backend_timeout` (no reply in time: the outcome is unknown) or
/// `invalid_response` (a reply too large).
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// `headers` are the reply's, names in lower case; only a rendered page
    /// reads them (its content type and scale).
    Http {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Failed {
        status: u16,
        error: ApiError,
    },
}

/// Sends one request. It never retries: whether a failure may be sent
/// again is the caller's decision (ADR-DOC-02 §6).
pub trait Transport {
    fn send(&self, request: Request) -> impl Future<Output = Reply> + Send;
}
