//! A reply as the caller sees it: the resource, or the error envelope.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transport::Reply;

/// The OS's error envelope (`ErrorBody`: `{code, message, hint?, details?}`),
/// or one the client describes itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    /// What to do about it (ERR-1), shown to the user as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ApiError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_owned(),
            message: message.to_owned(),
            hint: None,
            details: None,
        }
    }
}

/// Serialized as `{"ok":true,"status":..,"data":..}` or
/// `{"ok":false,"status":..,"error":{..}}`, the shape the desktop's pages
/// already read (`DocumentsResponse`).
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Ok { status: u16, data: Value },
    Err { status: u16, error: ApiError },
}

impl Response {
    pub fn fail(status: u16, code: &str, message: &str) -> Self {
        Self::Err {
            status,
            error: ApiError::new(code, message),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    pub fn status(&self) -> u16 {
        match self {
            Self::Ok { status, .. } | Self::Err { status, .. } => *status,
        }
    }

    /// The error code, if this is an error.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Ok { .. } => None,
            Self::Err { error, .. } => Some(&error.code),
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Ok { status, data } => {
                serde_json::json!({"ok": true, "status": status, "data": data})
            }
            Self::Err { status, error } => {
                serde_json::json!({"ok": false, "status": status, "error": error})
            }
        }
    }
}

/// The reply as the caller sees it, as the desktop's `toResponse` did: a 2xx
/// JSON body (not `null`) is the resource; a 4xx or 5xx carries the error
/// envelope (`code` and `message` strings); anything else (a redirect, a
/// body that is not JSON, an error without its envelope) is
/// `invalid_response` with the reply's status and the message
/// `unexpected <status> reply from the daemon`.
pub fn to_response(reply: Reply) -> Response {
    fn envelope(value: &Value) -> Option<ApiError> {
        let obj = value.as_object()?;
        let code = obj.get("code")?.as_str()?.to_owned();
        let message = obj.get("message")?.as_str()?.to_owned();
        let hint = obj.get("hint").and_then(Value::as_str).map(str::to_owned);
        let details = obj.get("details").filter(|d| !d.is_null()).cloned();
        Some(ApiError {
            code,
            message,
            hint,
            details,
        })
    }

    match reply {
        Reply::Failed { status, error } => Response::Err { status, error },
        Reply::Http { status, body } => {
            let parsed = serde_json::from_slice::<Value>(&body).ok();
            if (200..300).contains(&status)
                && let Some(data) = parsed.as_ref().filter(|v| !v.is_null())
            {
                return Response::Ok {
                    status,
                    data: data.clone(),
                };
            }
            if (400..600).contains(&status)
                && let Some(error) = parsed
                    .as_ref()
                    .and_then(|v| v.get("error"))
                    .and_then(envelope)
            {
                return Response::Err { status, error };
            }
            Response::fail(
                status,
                "invalid_response",
                &format!("unexpected {status} reply from the daemon"),
            )
        }
    }
}
