//! Private v1 NDJSON messages exchanged by the sidecar host and helper.

pub mod frame;

use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use std::{
    collections::BTreeMap,
    fmt,
    io::{self, Write},
    path::Path,
};

pub const MAX_CONTROL_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_TARGET_READY_FRAME_BYTES: usize = 16 * 1024;
pub const PROTOCOL_VERSION: u8 = 1;
const MAX_ARGV_ENTRIES: usize = 128;
const MAX_ENV_ENTRIES: usize = 64;

#[derive(Clone, PartialEq, Eq, Serialize)]
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

const ARGV_LIMIT_ERROR: &str = "sidecar argv limit exceeded";

struct ArgvSeed;
impl<'de> DeserializeSeed<'de> for ArgvSeed {
    type Value = Vec<String>;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(ArgvVisitor)
    }
}

struct ArgvVisitor;
struct RejectSeed;
impl<'de> DeserializeSeed<'de> for RejectSeed {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, _deserializer: D) -> Result<(), D::Error> {
        Err(de::Error::custom(ARGV_LIMIT_ERROR))
    }
}
impl<'de> Visitor<'de> for ArgvVisitor {
    type Value = Vec<String>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a bounded argv array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut argv = Vec::new();
        argv.try_reserve_exact(MAX_ARGV_ENTRIES)
            .map_err(|_| de::Error::custom(ARGV_LIMIT_ERROR))?;
        while argv.len() < MAX_ARGV_ENTRIES {
            let Some(arg) = seq.next_element::<String>()? else {
                return Ok(argv);
            };
            argv.push(arg);
        }
        let _ = seq.next_element_seed(RejectSeed)?;
        Ok(argv)
    }
}

struct RequestVisitor;
impl<'de> Visitor<'de> for RequestVisitor {
    type Value = Request;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a sidecar request")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Request, A::Error> {
        let (mut kind, mut version, mut request_id) = (None, None, None);
        let (mut executable, mut cwd, mut argv, mut env, mut force) =
            (None, None, None, None, None);
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" if kind.is_none() => kind = Some(map.next_value::<String>()?),
                "version" if version.is_none() => version = Some(map.next_value::<u8>()?),
                "request_id" if request_id.is_none() => request_id = Some(map.next_value::<u64>()?),
                "executable" if executable.is_none() => {
                    executable = Some(map.next_value::<String>()?)
                }
                "cwd" if cwd.is_none() => cwd = Some(map.next_value::<String>()?),
                "argv" if argv.is_none() => argv = Some(map.next_value_seed(ArgvSeed)?),
                "env" if env.is_none() => env = Some(map.next_value::<BTreeMap<String, String>>()?),
                "force" if force.is_none() => force = Some(map.next_value::<bool>()?),
                "type" => return Err(de::Error::duplicate_field("type")),
                "version" => return Err(de::Error::duplicate_field("version")),
                "request_id" => return Err(de::Error::duplicate_field("request_id")),
                "executable" => return Err(de::Error::duplicate_field("executable")),
                "cwd" => return Err(de::Error::duplicate_field("cwd")),
                "argv" => return Err(de::Error::duplicate_field("argv")),
                "env" => return Err(de::Error::duplicate_field("env")),
                "force" => return Err(de::Error::duplicate_field("force")),
                _ => {
                    return Err(de::Error::unknown_field(
                        &key,
                        &[
                            "type",
                            "version",
                            "request_id",
                            "executable",
                            "cwd",
                            "argv",
                            "env",
                            "force",
                        ],
                    ));
                }
            }
        }
        let kind = kind.ok_or_else(|| de::Error::missing_field("type"))?;
        let version = version.ok_or_else(|| de::Error::missing_field("version"))?;
        let request_id = request_id.ok_or_else(|| de::Error::missing_field("request_id"))?;
        match kind.as_str() {
            "launch" if force.is_none() => Ok(Request::Launch {
                version,
                request_id,
                executable: executable.ok_or_else(|| de::Error::missing_field("executable"))?,
                cwd: cwd.ok_or_else(|| de::Error::missing_field("cwd"))?,
                argv: argv.ok_or_else(|| de::Error::missing_field("argv"))?,
                env: env.ok_or_else(|| de::Error::missing_field("env"))?,
            }),
            "signal"
                if executable.is_none() && cwd.is_none() && argv.is_none() && env.is_none() =>
            {
                Ok(Request::Signal {
                    version,
                    request_id,
                    force: force.ok_or_else(|| de::Error::missing_field("force"))?,
                })
            }
            "is_empty"
                if executable.is_none()
                    && cwd.is_none()
                    && argv.is_none()
                    && env.is_none()
                    && force.is_none() =>
            {
                Ok(Request::IsEmpty {
                    version,
                    request_id,
                })
            }
            "launch" | "signal" | "is_empty" => {
                Err(de::Error::custom("invalid sidecar request fields"))
            }
            _ => Err(de::Error::unknown_variant(
                &kind,
                &["launch", "signal", "is_empty"],
            )),
        }
    }
}
impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RequestVisitor)
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestSequence {
    phase: SequencePhase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SequencePhase {
    AwaitLaunch,
    Running { last_id: u64 },
}

impl Default for RequestSequence {
    fn default() -> Self {
        Self::new()
    }
}
impl RequestSequence {
    pub const fn new() -> Self {
        Self {
            phase: SequencePhase::AwaitLaunch,
        }
    }

    pub fn validate(&self, request: &Request) -> Result<(), ProtocolError> {
        validate_request_data(request)?;
        match (&self.phase, request) {
            (SequencePhase::AwaitLaunch, Request::Launch { .. }) => Ok(()),
            (
                SequencePhase::Running { last_id },
                Request::Signal { request_id, .. } | Request::IsEmpty { request_id, .. },
            ) if request_id > last_id => Ok(()),
            _ => Err(ProtocolError::WrongSequence),
        }
    }

    pub fn accept(&mut self, request: &Request) -> Result<(), ProtocolError> {
        self.validate(request)?;
        self.advance(request);
        Ok(())
    }

    fn advance(&mut self, request: &Request) {
        let request_id = match request {
            Request::Launch { request_id, .. }
            | Request::Signal { request_id, .. }
            | Request::IsEmpty { request_id, .. } => *request_id,
        };
        self.phase = SequencePhase::Running {
            last_id: request_id,
        };
    }
}

fn content(s: &str) -> bool {
    s.len() <= 4096 && s.chars().all(|c| !c.is_control())
}
fn nonempty_content(s: &str) -> bool {
    !s.is_empty() && content(s)
}
fn secret(s: &str) -> bool {
    s.len() >= 32 && nonempty_content(s) && !s.chars().any(char::is_whitespace)
}
fn version(v: u8) -> Result<(), ProtocolError> {
    if v == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::InvalidMessage)
    }
}
fn validate_request_data(request: &Request) -> Result<(), ProtocolError> {
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
            if *request_id == 0
                || !Path::new(executable).is_absolute()
                || !nonempty_content(executable)
                || !Path::new(cwd).is_absolute()
                || !nonempty_content(cwd)
                || argv.len() > MAX_ARGV_ENTRIES
                || argv.iter().any(|arg| !content(arg))
                || env.len() > MAX_ENV_ENTRIES
                || env
                    .iter()
                    .any(|(k, v)| !nonempty_content(k) || k.contains('=') || !content(v))
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
                || !secret(token)
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

struct CappedWriter {
    bytes: Vec<u8>,
    limit: usize,
    overflowed: bool,
    allocation_failed: bool,
}

impl CappedWriter {
    fn new(limit: usize) -> io::Result<Self> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(limit.min(4096))
            .map_err(|_| io::Error::other("frame allocation failed"))?;
        Ok(Self {
            bytes,
            limit,
            overflowed: false,
            allocation_failed: false,
        })
    }
}

impl Write for CappedWriter {
    fn write(&mut self, incoming: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if incoming.len() > remaining {
            self.overflowed = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "frame limit exceeded",
            ));
        }
        if incoming.len() > self.bytes.capacity().saturating_sub(self.bytes.len())
            && self.bytes.try_reserve_exact(incoming.len()).is_err()
        {
            self.allocation_failed = true;
            return Err(io::Error::other("frame allocation failed"));
        }
        self.bytes.extend_from_slice(incoming);
        Ok(incoming.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_frame<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = CappedWriter::new(limit).map_err(|_| ProtocolError::InvalidMessage)?;
    let result = match serde_json::to_writer(&mut writer, value) {
        Ok(()) => writer.write_all(b"\n").map_err(|_| ()),
        Err(_) => Err(()),
    };
    match result {
        Ok(()) => Ok(writer.bytes),
        Err(_) if writer.overflowed => Err(ProtocolError::TooLarge),
        Err(_) if writer.allocation_failed => Err(ProtocolError::InvalidMessage),
        Err(_) => Err(ProtocolError::InvalidMessage),
    }
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
    sequence: &mut RequestSequence,
) -> Result<Vec<u8>, ProtocolError> {
    sequence.validate(request)?;
    let bytes = encode_frame(request, MAX_CONTROL_FRAME_BYTES)?;
    sequence.advance(request);
    Ok(bytes)
}
pub fn decode_request(
    bytes: &[u8],
    sequence: &mut RequestSequence,
) -> Result<Request, ProtocolError> {
    if bytes.len() > MAX_CONTROL_FRAME_BYTES {
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
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let request = Request::deserialize(&mut deserializer).map_err(request_decode_error)?;
    deserializer.end().map_err(request_decode_error)?;
    sequence.accept(&request)?;
    Ok(request)
}
fn request_decode_error(error: serde_json::Error) -> ProtocolError {
    if error.to_string().contains(ARGV_LIMIT_ERROR) {
        ProtocolError::InvalidMessage
    } else {
        ProtocolError::InvalidJson
    }
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

    #[cfg(windows)]
    fn exe() -> &'static str {
        r"C:\opt\sidecar.exe"
    }
    #[cfg(not(windows))]
    fn exe() -> &'static str {
        "/opt/sidecar"
    }
    #[cfg(windows)]
    fn cwd() -> &'static str {
        r"C:\tmp\sidecar"
    }
    #[cfg(not(windows))]
    fn cwd() -> &'static str {
        "/tmp/sidecar"
    }

    fn launch(id: u64) -> Request {
        Request::Launch {
            version: 1,
            request_id: id,
            executable: exe().into(),
            cwd: cwd().into(),
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

    fn boundary_launch(extra_len: usize) -> Request {
        let mut argv = vec!["a".repeat(4096); 15];
        argv.push("a".repeat(extra_len));
        custom(
            1,
            exe(),
            cwd(),
            argv.iter().map(String::as_str).collect(),
            vec![],
        )
    }

    #[test]
    fn capped_writer_growth_stays_within_logical_limit() {
        let limit = 5_000;
        let mut writer = CappedWriter::new(limit).unwrap();
        writer.write_all(b"x").unwrap();
        writer.write_all(&[b'x'; 4_096]).unwrap();
        assert_eq!(writer.bytes.len(), 4_097);
        assert!(writer.bytes.capacity() <= limit);
    }

    #[test]
    fn golden_request_and_ready_frames() {
        let request = launch(1);
        let mut sequence = RequestSequence::new();
        let bytes = encode_request(&request, &mut sequence).unwrap();
        let expected = format!(
            r#"{{"type":"launch","version":1,"request_id":1,"executable":{},"cwd":{},"argv":[],"env":{{"A":"value with spaces"}}}}
"#,
            serde_json::to_string(exe()).unwrap(),
            serde_json::to_string(cwd()).unwrap()
        );
        assert_eq!(bytes, expected.into_bytes());
        let mut decoded_sequence = RequestSequence::new();
        assert_eq!(decode_request(&bytes, &mut decoded_sequence), Ok(request));
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
            RequestSequence::new().validate(&launch(0)),
            Err(ProtocolError::InvalidMessage)
        );
        let mut sequence = RequestSequence::new();
        sequence.accept(&launch(1)).unwrap();
        assert_eq!(
            sequence.validate(&launch(1)),
            Err(ProtocolError::WrongSequence)
        );
    }

    #[test]
    fn validation_and_debug_redact_sensitive_values() {
        let request = Request::Launch {
            version: 1,
            request_id: 1,
            executable: exe().into(),
            cwd: cwd().into(),
            argv: vec!["argv-secret".into()],
            env: [("ENV_SECRET", "env-secret")]
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        };
        let shown = format!("{request:?}");
        for value in [exe(), cwd(), "argv-secret", "env-secret"] {
            assert!(!shown.contains(value));
        }
        let ready = Event::Ready {
            protocol: 1,
            port: 1,
            token: "token-secret".repeat(3),
            version: "v".into(),
        };
        assert!(!format!("{ready:?}").contains("token-secret"));
        assert_eq!(RequestSequence::new().validate(&request), Ok(()));
        assert_eq!(
            RequestSequence::new().validate(&custom(1, "relative", cwd(), vec![], vec![])),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            RequestSequence::new().validate(&custom(1, exe(), cwd(), vec!["\0"], vec![])),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            RequestSequence::new().validate(&custom(
                1,
                exe(),
                cwd(),
                vec![],
                vec![("K", "\u{0085}")]
            )),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            RequestSequence::new().validate(&custom(
                1,
                exe(),
                cwd(),
                vec!["space arg", ""],
                vec![("K", "")]
            )),
            Ok(())
        );
    }

    #[test]
    fn version_and_sequence_rules_are_fail_closed() {
        assert_eq!(
            RequestSequence::new().validate(&Request::Signal {
                version: 2,
                request_id: 1,
                force: false
            }),
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
            RequestSequence::new().validate(&Request::Signal {
                version: 1,
                request_id: 0,
                force: false
            }),
            Err(ProtocolError::InvalidMessage)
        );
        let mut sequence = RequestSequence::new();
        sequence.accept(&launch(1)).unwrap();
        assert_eq!(
            sequence.validate(&Request::Signal {
                version: 1,
                request_id: 4,
                force: false
            }),
            Ok(())
        );
        assert_eq!(
            sequence.validate(&launch(2)),
            Err(ProtocolError::WrongSequence)
        );
        assert_eq!(
            sequence.accept(&Request::IsEmpty {
                version: 1,
                request_id: 5
            }),
            Ok(())
        );
    }

    #[test]
    fn empty_reply_and_malformed_fields_are_strict() {
        let reply = Reply::Empty {
            version: 1,
            request_id: 2,
            empty: true,
        };
        let bytes = encode_reply(&reply).unwrap();
        assert_eq!(decode_reply(&bytes), Ok(reply));
        assert_eq!(
            decode_reply(
                br#"{"type":"empty","version":1,"request_id":2,"empty":"yes"}
"#
            ),
            Err(ProtocolError::InvalidJson)
        );
        assert_eq!(
            decode_reply(
                br#"{"type":"empty","version":1,"request_id":2}
"#
            ),
            Err(ProtocolError::InvalidJson)
        );
        assert_eq!(decode_request(br#"{"type":"launch","version":1,"request_id":1,"executable":"/x","cwd":"/y","env":{}}
"#, &mut RequestSequence::new()), Err(ProtocolError::InvalidJson));
        assert_eq!(decode_request(br#"{"type":"launch","version":1,"request_id":1,"executable":"/x","cwd":"/y","argv":[]}
"#, &mut RequestSequence::new()), Err(ProtocolError::InvalidJson));
        assert_eq!(
            RequestSequence::new().validate(&custom(1, exe(), "relative", vec![], vec![])),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            decode_reply(
                br#"{"type":"result","version":1,"request_id":1}garbage
"#
            ),
            Err(ProtocolError::InvalidJson)
        );
    }

    #[test]
    fn secret_rules_and_failed_encode_do_not_advance_state() {
        for token in [
            "",
            &"t".repeat(31),
            &format!("{} ", "t".repeat(31)),
            &format!("{} ", "t".repeat(31)),
        ] {
            assert_eq!(
                validate_event(&Event::Ready {
                    protocol: 1,
                    port: 1,
                    token: token.to_string(),
                    version: "v".into()
                }),
                Err(ProtocolError::InvalidMessage)
            );
        }
        let huge = Request::Launch {
            version: 1,
            request_id: 1,
            executable: exe().into(),
            cwd: cwd().into(),
            argv: vec!["x".repeat(4096); 20],
            env: BTreeMap::new(),
        };
        let mut sequence = RequestSequence::new();
        assert_eq!(
            encode_request(&huge, &mut sequence),
            Err(ProtocolError::TooLarge)
        );
        assert_eq!(sequence.validate(&launch(1)), Ok(()));
    }

    #[test]
    fn launch_argument_and_environment_counts_are_bounded() {
        let at_limits = Request::Launch {
            version: 1,
            request_id: 1,
            executable: exe().into(),
            cwd: cwd().into(),
            argv: (0..MAX_ARGV_ENTRIES).map(|n| format!("arg-{n}")).collect(),
            env: (0..MAX_ENV_ENTRIES)
                .map(|n| (format!("KEY_{n}"), format!("value-{n}")))
                .collect(),
        };
        let mut sequence = RequestSequence::new();
        let encoded = encode_request(&at_limits, &mut sequence).unwrap();
        assert_eq!(
            decode_request(&encoded, &mut RequestSequence::new()),
            Ok(at_limits)
        );

        for invalid in [
            Request::Launch {
                version: 1,
                request_id: 1,
                executable: exe().into(),
                cwd: cwd().into(),
                argv: (0..=MAX_ARGV_ENTRIES).map(|n| format!("arg-{n}")).collect(),
                env: BTreeMap::new(),
            },
            Request::Launch {
                version: 1,
                request_id: 1,
                executable: exe().into(),
                cwd: cwd().into(),
                argv: Vec::new(),
                env: (0..=MAX_ENV_ENTRIES)
                    .map(|n| (format!("KEY_{n}"), format!("value-{n}")))
                    .collect(),
            },
        ] {
            let mut sequence = RequestSequence::new();
            assert_eq!(
                encode_request(&invalid, &mut sequence),
                Err(ProtocolError::InvalidMessage)
            );
            assert!(encode_request(&launch(1), &mut sequence).is_ok());
        }
    }

    #[test]
    fn decoded_argv_limit_is_enforced_during_sequence_visitation() {
        let at_limit = custom(1, exe(), cwd(), vec!["arg"; MAX_ARGV_ENTRIES], vec![]);
        let mut valid_sequence = RequestSequence::new();
        let valid_frame = encode_request(&at_limit, &mut valid_sequence).unwrap();
        assert_eq!(
            decode_request(&valid_frame, &mut RequestSequence::new()),
            Ok(at_limit.clone())
        );

        let over_limit = custom(1, exe(), cwd(), vec!["arg"; MAX_ARGV_ENTRIES + 1], vec![]);
        let mut value = serde_json::to_value(&over_limit).unwrap();
        value["argv"][MAX_ARGV_ENTRIES] = serde_json::json!({"unparsed": [1, 2, 3]});
        let mut frame = serde_json::to_vec(&value).unwrap();
        frame.push(b'\n');
        let mut decode_sequence = RequestSequence::new();
        assert_eq!(
            decode_request(&frame, &mut decode_sequence),
            Err(ProtocolError::InvalidMessage)
        );
        assert_eq!(
            decode_request(&valid_frame, &mut decode_sequence),
            Ok(at_limit)
        );
    }

    #[test]
    fn environment_keys_reject_equals_before_encode_and_after_decode() {
        let raw_equals = custom(1, exe(), cwd(), vec![], vec![("A=B", "value")]);
        let mut encode_sequence = RequestSequence::new();
        assert_eq!(
            encode_request(&raw_equals, &mut encode_sequence),
            Err(ProtocolError::InvalidMessage)
        );
        let valid = encode_request(&launch(1), &mut encode_sequence).unwrap();

        let json = serde_json::to_string(&raw_equals).unwrap();
        let mut escaped_equals = json.replace("A=B", r"A\u003dB").into_bytes();
        escaped_equals.push(b'\n');
        let mut decode_sequence = RequestSequence::new();
        assert_eq!(
            decode_request(&escaped_equals, &mut decode_sequence),
            Err(ProtocolError::InvalidMessage)
        );
        assert!(decode_request(&valid, &mut decode_sequence).is_ok());
        let signal = encode_frame(
            &Request::Signal {
                version: 1,
                request_id: 2,
                force: false,
            },
            MAX_CONTROL_FRAME_BYTES,
        )
        .unwrap();
        assert_eq!(
            decode_request(&signal, &mut decode_sequence),
            Ok(Request::Signal {
                version: 1,
                request_id: 2,
                force: false,
            })
        );
    }

    #[test]
    fn bounded_encoding_handles_escaping_and_exact_newline_boundary() {
        let prefix = boundary_launch(0);
        let prefix_len = serde_json::to_vec(&prefix).unwrap().len();
        let extra_len = MAX_CONTROL_FRAME_BYTES - 1 - prefix_len;
        assert!(extra_len <= 4096);

        let exact = boundary_launch(extra_len);
        let mut sequence = RequestSequence::new();
        let bytes = encode_request(&exact, &mut sequence).unwrap();
        assert_eq!(bytes.len(), MAX_CONTROL_FRAME_BYTES);
        assert_eq!(bytes.last(), Some(&b'\n'));

        let mut unchanged = RequestSequence::new();
        let raw = "\\".repeat(4096);
        let escaped = custom(1, exe(), cwd(), vec![raw.as_str(); 15], vec![]);
        assert!(raw.len() * 15 < MAX_CONTROL_FRAME_BYTES);
        assert_eq!(
            encode_request(&escaped, &mut unchanged),
            Err(ProtocolError::TooLarge)
        );
        assert_eq!(unchanged.validate(&launch(1)), Ok(()));

        let over = boundary_launch(extra_len + 1);
        assert_eq!(
            encode_request(&over, &mut unchanged),
            Err(ProtocolError::TooLarge)
        );
        assert_eq!(unchanged.validate(&launch(1)), Ok(()));
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
