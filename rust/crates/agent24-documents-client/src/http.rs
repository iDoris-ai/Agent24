//! The [`Transport`] to a daemon on this machine, as the desktop's
//! `httpSender` was: plain HTTP to 127.0.0.1 only, the bearer added here,
//! no proxy (the bearer must not reach one), no redirects, a total deadline
//! per request and a cap on the reply.

use std::time::Duration;

use crate::response::ApiError;
use crate::transport::{Method, Reply, Request, Transport};

/// Longer than the kernel proxy's 30 s total, so the proxy answers first.
const TIMEOUT: Duration = Duration::from_secs(35);
/// Above the proxy's 1 MiB response cap: only a broken daemon reaches it.
const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;

pub struct HttpTransport {
    base: String,
    token: Option<String>,
    client: reqwest::Client,
    max_reply: usize,
}

impl HttpTransport {
    /// `base` must be `http://127.0.0.1:<port>` and nothing more.
    pub fn new(base: &str, token: Option<String>) -> Result<Self, String> {
        Self::with_limits(base, token, TIMEOUT, MAX_REPLY_BYTES)
    }

    pub fn with_limits(
        base: &str,
        token: Option<String>,
        timeout: Duration,
        max_reply: usize,
    ) -> Result<Self, String> {
        let port = base
            .strip_prefix("http://127.0.0.1:")
            .filter(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p != 0);
        if port.is_none() {
            return Err("the daemon address must be http://127.0.0.1:<port>".to_owned());
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| "the HTTP client could not be set up".to_owned())?;
        Ok(Self {
            base: base.to_owned(),
            token,
            client,
            max_reply,
        })
    }
}

fn failed(status: u16, code: &str, message: &str) -> Reply {
    Reply::Failed {
        status,
        error: ApiError::new(code, message),
    }
}

fn timed_out() -> Reply {
    failed(
        504,
        "backend_timeout",
        "no reply in time; the outcome is unknown",
    )
}

impl Transport for HttpTransport {
    async fn send(&self, request: Request) -> Reply {
        let url = format!("{}{}", self.base, request.path);
        let mut builder = match request.method {
            Method::Get => self.client.get(url),
            Method::Post => self.client.post(url),
        };
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(token) = &self.token {
            builder = builder.bearer_auth(token);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        // Never the error's own text: it names the URL, and nothing here
        // needs more than which way it failed.
        let mut response = match builder.send().await {
            Ok(response) => response,
            Err(err) if err.is_timeout() => return timed_out(),
            Err(_) => {
                return failed(
                    503,
                    "backend_unreachable",
                    "the daemon could not be reached",
                );
            }
        };
        let status = response.status().as_u16();
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > self.max_reply {
                        return failed(502, "invalid_response", "the reply is too large");
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(err) if err.is_timeout() => return timed_out(),
                Err(_) => return failed(502, "backend_unreachable", "the reply was cut short"),
            }
        }
        Reply::Http { status, body }
    }
}

#[cfg(test)]
mod tests;
