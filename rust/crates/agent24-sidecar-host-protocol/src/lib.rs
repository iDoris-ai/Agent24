//! Private v1 NDJSON messages exchanged by the sidecar host and helper.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

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
