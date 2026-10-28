//! CORE-2 slice S4 (2f-CLI): the CLI side of the capabilities-mode enrolment
//! protocol (`docs/agent/CORE-2-AGENTEAR-SLICE.md` §3.1).
//!
//! In capabilities mode `daemon.json` holds no bearer. It advertises an
//! `enroll_endpoint` (a Unix socket) instead. The CLI connects, speaks
//! protocol v1 (one JSON line per frame, at most 16 KiB), and receives a
//! short-lived day-to-day token. The connection stays open for the life of the
//! process because an elevation request (`elevate`) is only accepted on the
//! connection the token was issued on.
//!
//! Neither the token nor an elevation grant is ever printed or logged, and no
//! error produced here embeds either of them.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use zeroize::Zeroizing;

const PROTOCOL_V: u64 = 1;
/// Frame size cap, both directions (plan §3.1).
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
/// Per-step budget during the handshake (plan §3.1).
const HANDSHAKE_STEP: Duration = Duration::from_secs(5);
/// The daemon waits up to 60 s for the presence proof; leave headroom for the
/// final `elevated` frame to arrive.
const ELEVATION_WAIT: Duration = Duration::from_secs(65);
const SUMMARY_MAX_CHARS: usize = 200;

/// A failure while enrolling or elevating. `code` is what `--json` prints in
/// `{"error":{"code","message"}}` (A3 §3.6); neither field ever carries a
/// credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollError {
    pub code: String,
    pub message: String,
}

impl EnrollError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

impl fmt::Display for EnrollError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

struct Conn {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

/// An enrolled CLI session: the token plus the live connection.
pub struct Session {
    token: Zeroizing<String>,
    conn: tokio::sync::Mutex<Conn>,
}

/// Lowercase hex sha256 of the exact request body bytes (empty body hashes
/// the empty string).
pub fn body_sha256(body: &[u8]) -> String {
    use std::fmt::Write;
    let digest = Sha256::digest(body);
    let mut out = String::with_capacity(64);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn io_err(e: &std::io::Error) -> EnrollError {
    EnrollError::new(
        "daemon_unavailable",
        format!("enrolment socket I/O failed: {e}"),
    )
}

async fn write_frame(w: &mut OwnedWriteHalf, frame: &Value) -> Result<(), EnrollError> {
    let mut bytes = serde_json::to_vec(frame)
        .map_err(|e| EnrollError::new("bad_frame", format!("cannot encode frame: {e}")))?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err(EnrollError::new("bad_frame", "frame exceeds 16 KiB"));
    }
    bytes.push(b'\n');
    w.write_all(&bytes).await.map_err(|e| io_err(&e))?;
    w.flush().await.map_err(|e| io_err(&e))
}

/// Read one newline-terminated frame of at most 16 KiB.
async fn read_frame(r: &mut BufReader<OwnedReadHalf>) -> Result<Value, EnrollError> {
    let mut line = Vec::new();
    let mut limited = (&mut *r).take(MAX_FRAME_BYTES as u64 + 1);
    let n = limited
        .read_until(b'\n', &mut line)
        .await
        .map_err(|e| io_err(&e))?;
    if n == 0 {
        return Err(EnrollError::new(
            "daemon_unavailable",
            "the daemon closed the enrolment connection",
        ));
    }
    if n > MAX_FRAME_BYTES {
        return Err(EnrollError::new("bad_frame", "daemon frame exceeds 16 KiB"));
    }
    if line.last() != Some(&b'\n') {
        return Err(EnrollError::new(
            "daemon_unavailable",
            "the daemon closed the enrolment connection mid-frame",
        ));
    }
    line.pop();
    let frame: Value = serde_json::from_slice(&line)
        .map_err(|_| EnrollError::new("bad_frame", "daemon sent a malformed frame"))?;
    if frame["v"].as_u64() != Some(PROTOCOL_V) {
        return Err(EnrollError::new(
            "unsupported_version",
            "daemon speaks a different enrolment protocol version",
        ));
    }
    Ok(frame)
}

async fn read_step(
    r: &mut BufReader<OwnedReadHalf>,
    budget: Duration,
    on_timeout: (&str, &str),
) -> Result<Value, EnrollError> {
    match tokio::time::timeout(budget, read_frame(r)).await {
        Ok(frame) => frame,
        Err(_) => Err(EnrollError::new(on_timeout.0, on_timeout.1)),
    }
}

fn suffix(msg: &str) -> String {
    if msg.is_empty() {
        ".".to_owned()
    } else {
        format!(": {msg}")
    }
}

/// Convert a daemon `error` frame into an [`EnrollError`]. The daemon's
/// message is kept where it helps (it is written for humans and never holds a
/// secret); the presence outcomes get a fixed message that tells the user the
/// concrete next step.
fn error_from_frame(frame: &Value) -> EnrollError {
    let code = frame["code"].as_str().unwrap_or("bad_frame");
    let daemon_msg = frame["message"].as_str().unwrap_or("");
    let message = match code {
        "presence_unavailable" => {
            "Approval requires the Agent24 desktop app to be open: open Agent24 and try again."
                .to_owned()
        }
        "not_elevated_operation" => "This action can't be approved from the CLI yet".to_owned(),
        "presence_denied" => "The request was denied in the Agent24 app.".to_owned(),
        "presence_timeout" => "Timed out waiting for approval in the Agent24 app.".to_owned(),
        "generation_mismatch" => {
            "The daemon restarted since this command read daemon.json; run it again.".to_owned()
        }
        "identity_rejected" => format!(
            "The daemon rejected this agent24 binary's identity{}",
            suffix(daemon_msg)
        ),
        _ if daemon_msg.is_empty() => format!("enrolment failed ({code})"),
        _ => daemon_msg.to_owned(),
    };
    EnrollError::new(code, message)
}

/// Length of the challenge nonce in hex digits (128 random bits, §3.1).
const NONCE_HEX_LEN: usize = 32;

/// The nonce of a daemon `challenge` frame, if it is well formed: exactly 32
/// hex digits. Anything else is a protocol violation and the client must not
/// answer it with a `hello`.
fn challenge_nonce(frame: &Value) -> Option<&str> {
    frame["nonce"]
        .as_str()
        .filter(|n| n.len() == NONCE_HEX_LEN && n.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Connect to `socket`, run `challenge` → `hello` → `enrolled`, and return the
/// session. The daemon speaks first (design §10 R8): the client reads the
/// `challenge`, then sends `hello` echoing its nonce verbatim; nothing is
/// written before the challenge arrives, because bytes queued before the
/// daemon's accept are refused. `generation` is `daemon.json`'s; the daemon
/// refuses a mismatch.
pub async fn enroll(socket: &Path, generation: &str) -> Result<Session, EnrollError> {
    let stream = tokio::time::timeout(HANDSHAKE_STEP, UnixStream::connect(socket))
        .await
        .map_err(|_| EnrollError::new("daemon_unavailable", "timed out connecting to the daemon"))?
        .map_err(|e| {
            EnrollError::new(
                "daemon_unavailable",
                format!("cannot connect to the enrolment socket: {e}"),
            )
        })?;
    let (read, write) = stream.into_split();
    let mut conn = Conn {
        reader: BufReader::new(read),
        writer: write,
    };
    let timeout = ("timeout", "timed out during the enrolment handshake");
    let challenge = read_step(&mut conn.reader, HANDSHAKE_STEP, timeout).await?;
    match challenge["op"].as_str() {
        Some("challenge") => {}
        Some("error") => return Err(error_from_frame(&challenge)),
        _ => return Err(EnrollError::new("bad_frame", "expected a challenge frame")),
    }
    let nonce = challenge_nonce(&challenge)
        .ok_or_else(|| EnrollError::new("bad_frame", "challenge frame carries no valid nonce"))?;
    write_frame(
        &mut conn.writer,
        &json!({"v": PROTOCOL_V, "op": "hello", "generation": generation, "nonce": nonce}),
    )
    .await?;
    let enrolled = read_step(&mut conn.reader, HANDSHAKE_STEP, timeout).await?;
    match enrolled["op"].as_str() {
        Some("enrolled") => {}
        Some("error") => return Err(error_from_frame(&enrolled)),
        _ => return Err(EnrollError::new("bad_frame", "expected an enrolled frame")),
    }
    let token = enrolled["token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| EnrollError::new("bad_frame", "enrolled frame carries no token"))?;
    Ok(Session {
        token: Zeroizing::new(token.to_owned()),
        conn: tokio::sync::Mutex::new(conn),
    })
}

impl Session {
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Ask the daemon for a one-shot elevation grant for exactly this request
    /// (`method`, `path`, `body`) on the enrolled connection. Blocks until the
    /// human decides, up to ~65 s. Prints one English notice to stderr.
    pub async fn elevate(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<Zeroizing<String>, EnrollError> {
        let mut conn = self.conn.lock().await;
        let summary: String = format!("agent24 CLI: {method} {path}")
            .chars()
            .take(SUMMARY_MAX_CHARS)
            .collect();
        write_frame(
            &mut conn.writer,
            &json!({
                "v": PROTOCOL_V,
                "op": "elevate",
                "method": method,
                "path": path,
                "body_sha256": body_sha256(body),
                "summary": summary,
            }),
        )
        .await?;
        eprintln!("Waiting for approval in the Agent24 app…");
        let deadline = tokio::time::Instant::now() + ELEVATION_WAIT;
        let mut pending_id: Option<String> = None;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let frame = read_step(
                &mut conn.reader,
                remaining,
                (
                    "presence_timeout",
                    "Timed out waiting for approval in the Agent24 app.",
                ),
            )
            .await?;
            match frame["op"].as_str() {
                Some("elevation_pending") => {
                    if pending_id.is_some() {
                        return Err(EnrollError::new(
                            "bad_frame",
                            "duplicate elevation_pending frame",
                        ));
                    }
                    let id = frame["challenge_id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| {
                            EnrollError::new("bad_frame", "pending frame carries no challenge id")
                        })?;
                    pending_id = Some(id.to_owned());
                }
                Some("elevated") => {
                    let matches_pending = pending_id
                        .as_deref()
                        .is_some_and(|id| frame["challenge_id"].as_str() == Some(id));
                    if !matches_pending {
                        return Err(EnrollError::new(
                            "bad_frame",
                            "elevated frame does not match the pending challenge",
                        ));
                    }
                    let grant = frame["grant"]
                        .as_str()
                        .filter(|g| !g.is_empty())
                        .ok_or_else(|| {
                            EnrollError::new("bad_frame", "elevated frame carries no grant")
                        })?;
                    return Ok(Zeroizing::new(grant.to_owned()));
                }
                Some("error") => return Err(error_from_frame(&frame)),
                _ => {
                    return Err(EnrollError::new(
                        "bad_frame",
                        "unexpected frame while waiting for approval",
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn not_elevated_operation_keeps_code_and_explains_cli_limit() {
        let err =
            error_from_frame(&json!({"code":"not_elevated_operation","message":"daemon text"}));
        assert_eq!(err.code, "not_elevated_operation");
        assert_eq!(
            err.message,
            "This action can't be approved from the CLI yet"
        );
    }

    async fn reject_elevation_frames(frames: Vec<Value>) {
        let (client, server) = UnixStream::pair().unwrap();
        let (read, write) = client.into_split();
        let session = Session {
            token: Zeroizing::new("test-token".to_owned()),
            conn: tokio::sync::Mutex::new(Conn {
                reader: BufReader::new(read),
                writer: write,
            }),
        };
        let (read, mut write) = server.into_split();
        let peer = tokio::spawn(async move {
            let mut request = String::new();
            BufReader::new(read).read_line(&mut request).await.unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap()["op"],
                "elevate"
            );
            for frame in frames {
                let _ = write.write_all(format!("{frame}\n").as_bytes()).await;
            }
        });
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            session.elevate("POST", "/api/v1/attached", b"{}"),
        )
        .await
        .unwrap();
        peer.await.unwrap();
        assert_eq!(result.unwrap_err().code, "bad_frame");
    }

    fn pending(id: Value) -> Value {
        json!({"v":1,"op":"elevation_pending","challenge_id":id})
    }
    fn granted(id: Value) -> Value {
        json!({"v":1,"op":"elevated","challenge_id":id,"grant":"test-grant"})
    }

    #[tokio::test]
    async fn elevated_requires_pending() {
        reject_elevation_frames(vec![granted(json!("a"))]).await;
    }
    #[tokio::test]
    async fn pending_requires_id() {
        reject_elevation_frames(vec![pending(Value::Null), granted(json!("a"))]).await;
    }
    #[tokio::test]
    async fn elevated_requires_id() {
        reject_elevation_frames(vec![pending(json!("a")), granted(Value::Null)]).await;
    }
    #[tokio::test]
    async fn pending_cannot_be_replaced() {
        reject_elevation_frames(vec![
            pending(json!("a")),
            pending(json!("b")),
            granted(json!("b")),
        ])
        .await;
    }
    #[tokio::test]
    async fn elevated_requires_matching_id() {
        reject_elevation_frames(vec![pending(json!("a")), granted(json!("b"))]).await;
    }

    async fn sized_frame(size: usize) -> Result<Value, EnrollError> {
        let (client, mut server) = UnixStream::pair().unwrap();
        let (read, _) = client.into_split();
        let mut bytes = br#"{"v":1,"padding":""}"#.to_vec();
        bytes.splice(
            bytes.len() - 2..bytes.len() - 2,
            std::iter::repeat_n(b'x', size - bytes.len() - 1),
        );
        bytes.push(b'\n');
        assert_eq!(bytes.len(), size);
        let writer = tokio::spawn(async move {
            server.write_all(&bytes).await.unwrap();
        });
        let result = read_frame(&mut BufReader::new(read)).await;
        writer.await.unwrap();
        result
    }

    #[tokio::test]
    async fn frame_including_newline_at_limit_is_accepted() {
        assert!(sized_frame(MAX_FRAME_BYTES).await.is_ok());
    }

    #[tokio::test]
    async fn frame_including_newline_over_limit_is_rejected() {
        assert_eq!(
            sized_frame(MAX_FRAME_BYTES + 1).await.unwrap_err().code,
            "bad_frame"
        );
    }

    #[test]
    fn body_hash_is_lowercase_hex_of_exact_bytes() {
        assert_eq!(
            body_sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            body_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn unavailable_message_names_the_desktop_app() {
        let e = error_from_frame(&json!({"code": "presence_unavailable", "message": "x"}));
        assert_eq!(e.code, "presence_unavailable");
        assert!(e.message.contains("desktop app"));
    }
}
