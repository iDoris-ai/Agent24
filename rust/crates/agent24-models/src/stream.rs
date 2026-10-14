//! Reading a chat completion as it is written (ME4-S2a §2.3): an answer is
//! waited for as long as it keeps coming, not for a fixed total. Its first
//! event must come within `first_chunk`, then one within every `stall`, and
//! all of it before the call's `deadline`. Only `data:` events count as
//! coming: a comment (`: keep-alive`), an `event:` or an `id:` line moves no
//! timer. A stream that ends before `[DONE]` and before any `finish_reason`
//! is a backend that broke off, not an answer.

use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{MAX_CHAT_RESPONSE_BYTES, ModelError, OaUsage, Progress};

/// What a stream said: the text of its deltas, the model the first event
/// named, and the usage the last event reported.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Streamed {
    pub content: String,
    pub model: Option<String>,
    pub usage: Option<OaUsage>,
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<OaUsage>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: Option<Delta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
}

/// When the next event must have come by, and what it is called if it has not.
fn due(p: &Progress, started: Instant, last: Option<Instant>) -> (Instant, String) {
    let (at, what) = match last {
        None => (
            started.checked_add(p.first_chunk),
            format!("no output within {}s", p.first_chunk.as_secs()),
        ),
        Some(t) => (
            t.checked_add(p.stall),
            format!("no output for {}s", p.stall.as_secs()),
        ),
    };
    match at {
        Some(at) if at < p.deadline => (at, what),
        // Past the deadline, or a timer too long to add: the deadline decides.
        _ => (p.deadline, "past the call's deadline".to_owned()),
    }
}

/// Waits for `fut` until `until`, or for `cancel`.
pub(crate) async fn until<T>(
    fut: impl Future<Output = T>,
    until: Instant,
    cancel: &CancellationToken,
) -> Result<Option<T>, ModelError> {
    tokio::select! {
        v = fut => Ok(Some(v)),
        () = tokio::time::sleep_until(until.into()) => Ok(None),
        () = cancel.cancelled() => Err(ModelError::Cancelled),
    }
}

/// The event-stream body of `response`, read to its end under `p`.
pub(crate) async fn read(
    mut response: reqwest::Response,
    p: &Progress,
    started: Instant,
    cancel: &CancellationToken,
    name: &str,
) -> Result<Streamed, ModelError> {
    let mut out = Streamed::default();
    let (mut pending, mut data, mut total) = (Vec::<u8>::new(), None::<String>, 0usize);
    // How much of `pending` is known to hold no line break: a long line coming
    // in many chunks is scanned once, not once a chunk.
    let mut scanned = 0usize;
    let (mut last, mut finished) = (None::<Instant>, false);
    loop {
        let (at, what) = due(p, started, last);
        let Some(chunk) = until(response.chunk(), at, cancel).await? else {
            return Err(ModelError::Unavailable(format!("{name}: {what}")));
        };
        // A body that breaks off mid-chunk (a closed connection inside chunked
        // framing, as Ollama streams) is the backend going away, like an early end.
        let chunk = chunk
            .map_err(|e| ModelError::Unavailable(format!("{name}: the stream broke off: {e}")))?;
        let Some(bytes) = chunk else {
            // The end. Without [DONE] or a finish_reason, the backend broke off.
            // An event not ended by a blank line is no event (the SSE rule), so
            // a `[DONE]` written without one does not count either.
            return if finished {
                Ok(out)
            } else {
                Err(ModelError::Unavailable(format!(
                    "{name}: the stream ended before the answer did"
                )))
            };
        };
        total += bytes.len();
        if total > MAX_CHAT_RESPONSE_BYTES {
            return Err(ModelError::Provider(format!(
                "{name} response exceeds {MAX_CHAT_RESPONSE_BYTES} bytes"
            )));
        }
        pending.extend_from_slice(&bytes);
        while let Some(i) = pending[scanned..].iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = pending.drain(..=scanned + i).collect();
            scanned = 0;
            // A line is decoded whole, so a character split across chunks is not
            // broken; bytes that are no UTF-8 become U+FFFD rather than an error.
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                // A blank line ends an event: only one with data counts.
                // An event with no data, or empty data, says nothing: it moves no timer.
                let Some(event) = data.take().filter(|e| !e.is_empty()) else {
                    continue;
                };
                last = Some(Instant::now());
                if event == "[DONE]" {
                    return Ok(out);
                }
                finished |= take(&mut out, &event, name)?;
            } else if let Some(v) = line.strip_prefix("data:") {
                let v = v.strip_prefix(' ').unwrap_or(v);
                // Several data lines of one event are one value, joined by line breaks.
                match &mut data {
                    Some(d) => {
                        d.push('\n');
                        d.push_str(v);
                    }
                    None => data = Some(v.to_owned()),
                }
            }
            // A comment, `event:`, `id:` or `retry:` line says nothing of the answer.
        }
        scanned = pending.len();
    }
}

/// Adds one event to `out`; whether it finished the answer.
fn take(out: &mut Streamed, event: &str, name: &str) -> Result<bool, ModelError> {
    let chunk: Chunk = serde_json::from_str(event).map_err(|e| {
        ModelError::Provider(format!("{name} returned an event that is not JSON: {e}"))
    })?;
    if chunk.error.is_some() {
        return Err(ModelError::Unavailable(format!(
            "{name}: the backend failed while it wrote"
        )));
    }
    if out.model.is_none() {
        out.model = chunk.model.filter(|m| !m.is_empty());
    }
    if chunk.usage.is_some() {
        out.usage = chunk.usage;
    }
    let mut finished = false;
    for c in chunk.choices {
        if let Some(text) = c.delta.and_then(|d| d.content) {
            out.content.push_str(&text);
        }
        finished |= c.finish_reason.is_some();
    }
    Ok(finished)
}

/// The time left before `p`'s deadline, from `now`.
pub(crate) fn left(p: &Progress, now: Instant) -> Duration {
    p.deadline.saturating_duration_since(now)
}

#[cfg(test)]
mod tests;
