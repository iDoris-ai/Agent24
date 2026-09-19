//! Private v1 NDJSON messages exchanged by the sidecar host and helper.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, path::Path};

pub const PROTOCOL_VERSION: u8 = 1;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Launch {
        version: u8,
        request_id: u64,
        executable: String,
        cwd: String,
        argv: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Signal {
        version: u8,
        request_id: u64,
        force: bool,
    },
    IsEmpty {
        version: u8,
        request_id: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Owned {
        version: u8,
        request_id: u64,
    },
    Result {
        version: u8,
        request_id: u64,
    },
    Empty {
        version: u8,
        request_id: u64,
        empty: bool,
    },
    Error {
        version: u8,
        request_id: u64,
        code: ErrorCode,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Ready {
        protocol: u8,
        port: u16,
        token: String,
        version: String,
    },
    Exit {
        protocol: u8,
        code: Option<i32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    LaunchFailed,
    SignalFailed,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidMessage,
    WrongSequence,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid sidecar message")
    }
}
impl std::error::Error for ProtocolError {}

fn content(s: &str) -> bool {
    s.len() <= 4096 && s.chars().all(|c| !c.is_control())
}
fn nonempty_content(s: &str) -> bool {
    !s.is_empty() && content(s)
}
fn version(v: u8) -> Result<(), ProtocolError> {
    if v == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::InvalidMessage)
    }
}
pub fn validate_request(
    request: &Request,
    launched: bool,
    previous_id: Option<u64>,
) -> Result<(), ProtocolError> {
    match request {
        Request::Launch {
            version: v,
            request_id,
            executable,
            cwd,
            argv,
            env,
        } => {
            version(*v)?;
            if *request_id == 0 {
                return Err(ProtocolError::InvalidMessage);
            }
            if previous_id.is_some_and(|old| *request_id <= old) || launched {
                return Err(ProtocolError::WrongSequence);
            }
            if !Path::new(executable).is_absolute()
                || !nonempty_content(executable)
                || !Path::new(cwd).is_absolute()
                || !nonempty_content(cwd)
                || argv.iter().any(|arg| !content(arg))
                || env.iter().any(|(k, v)| !nonempty_content(k) || !content(v))
            {
                return Err(ProtocolError::InvalidMessage);
            }
        }
        Request::Signal {
            version: v,
            request_id,
            ..
        }
        | Request::IsEmpty {
            version: v,
            request_id,
        } => {
            version(*v)?;
            if *request_id == 0 {
                return Err(ProtocolError::InvalidMessage);
            }
            if !launched || previous_id.is_some_and(|old| *request_id <= old) {
                return Err(ProtocolError::WrongSequence);
            }
        }
    }
    Ok(())
}
pub fn validate_reply(reply: &Reply) -> Result<(), ProtocolError> {
    let (v, id) = match reply {
        Reply::Owned {
            version,
            request_id,
        }
        | Reply::Result {
            version,
            request_id,
        }
        | Reply::Empty {
            version,
            request_id,
            ..
        }
        | Reply::Error {
            version,
            request_id,
            ..
        } => (version, request_id),
    };
    version(*v)?;
    if *id == 0 {
        return Err(ProtocolError::InvalidMessage);
    }
    Ok(())
}
pub fn validate_event(event: &Event) -> Result<(), ProtocolError> {
    match event {
        Event::Ready {
            protocol,
            port,
            token,
            version: target_version,
        } => {
            version(*protocol)?;
            if *port == 0
                || token.len() < 32
                || !nonempty_content(token)
                || !nonempty_content(target_version)
                || target_version.len() > 128
            {
                return Err(ProtocolError::InvalidMessage);
            }
        }
        Event::Exit { protocol, .. } => version(*protocol)?,
    }
    Ok(())
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Launch {
                version,
                request_id,
                ..
            } => f
                .debug_struct("Launch")
                .field("version", version)
                .field("request_id", request_id)
                .field("executable", &"<redacted>")
                .field("cwd", &"<redacted>")
                .field("argv", &"<redacted>")
                .field("env", &"<redacted>")
                .finish(),
            Self::Signal {
                version,
                request_id,
                force,
            } => f
                .debug_struct("Signal")
                .field("version", version)
                .field("request_id", request_id)
                .field("force", force)
                .finish(),
            Self::IsEmpty {
                version,
                request_id,
            } => f
                .debug_struct("IsEmpty")
                .field("version", version)
                .field("request_id", request_id)
                .finish(),
        }
    }
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready {
                protocol,
                port,
                version,
                ..
            } => f
                .debug_struct("Ready")
                .field("protocol", protocol)
                .field("port", port)
                .field("token", &"<redacted>")
                .field("version", version)
                .finish(),
            Self::Exit { protocol, code } => f
                .debug_struct("Exit")
                .field("protocol", protocol)
                .field("code", code)
                .finish(),
        }
    }
}
