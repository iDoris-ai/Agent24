//! Private v1 NDJSON messages exchanged by the sidecar host and helper.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, path::Path};

pub const MAX_CONTROL_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_TARGET_READY_FRAME_BYTES: usize = 16 * 1024;
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
    TooLarge,
    MissingNewline,
    TrailingData,
    InvalidJson,
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

fn encode_frame<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, ProtocolError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| ProtocolError::InvalidMessage)?;
    bytes.push(b'\n');
    if bytes.len() > limit {
        return Err(ProtocolError::TooLarge);
    }
    Ok(bytes)
}
fn decode_frame<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    limit: usize,
) -> Result<T, ProtocolError> {
    if bytes.len() > limit {
        return Err(ProtocolError::TooLarge);
    }
    if bytes.last() != Some(&b'\n') {
        return Err(ProtocolError::MissingNewline);
    }
    let body = &bytes[..bytes.len() - 1];
    if body.is_empty() {
        return Err(ProtocolError::InvalidJson);
    }
    if body.contains(&b'\n') {
        return Err(ProtocolError::TrailingData);
    }
    serde_json::from_slice(body).map_err(|_| ProtocolError::InvalidJson)
}

pub fn encode_request(
    request: &Request,
    launched: bool,
    previous_id: Option<u64>,
) -> Result<Vec<u8>, ProtocolError> {
    validate_request(request, launched, previous_id)?;
    encode_frame(request, MAX_CONTROL_FRAME_BYTES)
}
pub fn decode_request(
    bytes: &[u8],
    launched: bool,
    previous_id: Option<u64>,
) -> Result<Request, ProtocolError> {
    let request: Request = decode_frame(bytes, MAX_CONTROL_FRAME_BYTES)?;
    validate_request(&request, launched, previous_id)?;
    Ok(request)
}
pub fn encode_reply(reply: &Reply) -> Result<Vec<u8>, ProtocolError> {
    validate_reply(reply)?;
    encode_frame(reply, MAX_CONTROL_FRAME_BYTES)
}
pub fn decode_reply(bytes: &[u8]) -> Result<Reply, ProtocolError> {
    let reply: Reply = decode_frame(bytes, MAX_CONTROL_FRAME_BYTES)?;
    validate_reply(&reply)?;
    Ok(reply)
}
pub fn encode_event(event: &Event) -> Result<Vec<u8>, ProtocolError> {
    validate_event(event)?;
    encode_frame(
        event,
        if matches!(event, Event::Ready { .. }) {
            MAX_TARGET_READY_FRAME_BYTES
        } else {
            MAX_CONTROL_FRAME_BYTES
        },
    )
}
pub fn decode_event(bytes: &[u8]) -> Result<Event, ProtocolError> {
    let event: Event = decode_frame(bytes, MAX_CONTROL_FRAME_BYTES)?;
    validate_event(&event)?;
    if matches!(event, Event::Ready { .. }) && bytes.len() > MAX_TARGET_READY_FRAME_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    Ok(event)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::items_after_test_module)]
mod tests {
    use super::*;

    fn launch(id: u64) -> Request {
        Request::Launch {
            version: 1,
            request_id: id,
            executable: "/opt/sidecar".into(),
            cwd: "/tmp/sidecar".into(),
            argv: vec![],
            env: [("A", "value with spaces")]
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    fn custom(
        id: u64,
        executable: &str,
        cwd: &str,
        argv: Vec<&str>,
        env: Vec<(&str, &str)>,
    ) -> Request {
        Request::Launch {
            version: 1,
            request_id: id,
            executable: executable.into(),
            cwd: cwd.into(),
            argv: argv.into_iter().map(Into::into).collect(),
            env: env.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
        }
    }

    #[test]
    fn golden_request_and_ready_frames() {
        let request = launch(1);
        let bytes = encode_request(&request, false, None).unwrap();
        assert_eq!(bytes, br#"{"type":"launch","version":1,"request_id":1,"executable":"/opt/sidecar","cwd":"/tmp/sidecar","argv":[],"env":{"A":"value with spaces"}}
"#);
        assert_eq!(decode_request(&bytes, false, None), Ok(request));
        let ready = Event::Ready {
            protocol: 1,
            port: 8080,
            token: "t".repeat(32),
            version: "target-1".into(),
        };
        let encoded = encode_event(&ready).unwrap();
        assert_eq!(
            encoded,
            format!(
                r#"{{"type":"ready","protocol":1,"port":8080,"token":"{}","version":"target-1"}}
"#,
                "t".repeat(32)
            )
            .into_bytes()
        );
        assert_eq!(decode_event(&encoded), Ok(ready));
    }

    #[test]
    fn adversarial_frames_fail_closed_without_echoing() {
        assert_eq!(
            decode_reply(br#"{"type":"result","version":1,"request_id":1}"#),
            Err(ProtocolError::MissingNewline)
        );
        assert_eq!(
            decode_reply(
                br#"{"type":"result","version":1,"request_id":1}
{"type":"result","version":1,"request_id":2}
"#
            ),
            Err(ProtocolError::TrailingData)
        );
        assert_eq!(
            decode_reply(
                br#"{"type":"result","version":1,"request_id":1,"raw":"bad"}
"#
            ),
            Err(ProtocolError::InvalidJson)
        );
        assert_eq!(
            decode_reply(&vec![b' '; MAX_CONTROL_FRAME_BYTES + 1]),
            Err(ProtocolError::TooLarge)
        );
        let padded = format!(
            r#"{{"type":"ready","protocol":1,"port":1,"token":"{}","version":"v"}}{}
"#,
            "t".repeat(32),
            " ".repeat(MAX_TARGET_READY_FRAME_BYTES)
        );
        assert_eq!(
            decode_event(padded.as_bytes()),
            Err(ProtocolError::TooLarge)
        );
        let token = "secret-token".repeat(4);
        assert!(!format!("{}", ProtocolError::InvalidMessage).contains(&token));
        assert_eq!(
            validate_request(&launch(0), false, None),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            validate_request(&launch(1), true, Some(1)),
            Err(ProtocolError::WrongSequence)
        );
    }

    #[test]
    fn validation_and_debug_redact_sensitive_values() {
        let request = Request::Launch {
            version: 1,
            request_id: 1,
            executable: "/secret/executable".into(),
            cwd: "/secret/cwd".into(),
            argv: vec!["argv-secret".into()],
            env: [("ENV_SECRET", "env-secret")]
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        };
        let shown = format!("{request:?}");
        for value in [
            "/secret/executable",
            "/secret/cwd",
            "argv-secret",
            "env-secret",
        ] {
            assert!(!shown.contains(value));
        }
        let ready = Event::Ready {
            protocol: 1,
            port: 1,
            token: "token-secret".repeat(3),
            version: "v".into(),
        };
        assert!(!format!("{ready:?}").contains("token-secret"));
        assert_eq!(validate_request(&request, false, None), Ok(()));
        assert_eq!(
            validate_request(&custom(1, "relative", "/ok", vec![], vec![]), false, None),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            validate_request(&custom(1, "/ok", "/ok", vec!["\0"], vec![]), false, None),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            validate_request(
                &custom(1, "/ok", "/ok", vec![], vec![("K", "\u{0085}")]),
                false,
                None
            ),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            validate_request(
                &custom(1, "/ok", "/ok", vec!["space arg", ""], vec![("K", "")]),
                false,
                None
            ),
            Ok(())
        );
    }

    #[test]
    fn version_and_sequence_rules_are_fail_closed() {
        assert_eq!(
            validate_request(
                &Request::Signal {
                    version: 2,
                    request_id: 1,
                    force: false
                },
                true,
                None
            ),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            validate_reply(&Reply::Result {
                version: 2,
                request_id: 1
            }),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            validate_event(&Event::Exit {
                protocol: 2,
                code: None
            }),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            validate_request(
                &Request::Signal {
                    version: 1,
                    request_id: 0,
                    force: false
                },
                true,
                None
            ),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            validate_request(
                &Request::Signal {
                    version: 1,
                    request_id: 4,
                    force: false
                },
                true,
                Some(4)
            ),
            Err(ProtocolError::WrongSequence)
        );
        assert_eq!(
            validate_request(&launch(2), true, Some(1)),
            Err(ProtocolError::WrongSequence)
        );
        assert_eq!(validate_request(&launch(1), false, None), Ok(()));
    }
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
