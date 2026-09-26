//! NEW: the module-side half of the protocol (ME4-S3 §4).
//!
//! Before this module, proto had no module-side API at all — every module
//! (Sin90's `src/adapter_agent24/`) hand-wrote its own dial, `initialize`
//! request, and multiplexed callback transport (ME4-S3 §1.3). This module is
//! that code, moved here so any out-of-process module gets it for free:
//! [`ModuleEnv`] reads the four spawn variables (minus the listen fd, which
//! stays in `agent24-os-fd`, H1); [`Connection::connect_from_env`] dials the
//! callback socket and runs `initialize`; [`Connection`] is the multiplexed
//! connection past the handshake (single writer task, reader task
//! dispatching by id, 64 in-flight bound, cancel-on-drop — moved from
//! Sin90's `adapter_agent24::transport::Transport`, ME4-S3 §1.2 row 6).
//!
//! J-S1b (proto/os-fd must not leave the SDK's clippy list a back door): no
//! `pub type` and no `pub use` of anything outside this crate anywhere under
//! `module*` — a cross-crate alias is invisible to a downstream crate's own
//! `disallowed-types` list, so the rule here is structural (no alias to write)
//! rather than a name the SDK's `clippy.toml` would have to know about.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::initialize::{INITIALIZE_METHOD, InitializeParams, InitializeRequest, Offer};
use crate::launch;
use crate::version::VersionRange;

/// Runs exactly once when the connection is judged dead (no reconnect: one
/// callback connection per generation, D1).
///
/// A newtype, not `pub type … = Arc<dyn Fn()>`: J-S1b forbids every
/// `pub type` under `module*` so no alias can ever smuggle a socket type past
/// the SDK's clippy list (v3, H-2).
#[derive(Clone)]
pub struct FatalHook(Arc<dyn Fn() + Send + Sync>);

impl FatalHook {
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
    /// Called by the mux exactly once when the connection dies.
    pub fn fire(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for FatalHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FatalHook(..)")
    }
}

/// The spawn variables a module that starts children should
/// `Command::env_remove` (the SDK never mutates `environ`, §4.3).
pub const SPAWN_ENV_VARS: [&str; 4] = [
    launch::ENV_LISTEN_FD,
    launch::ENV_CALLBACK_SOCK,
    launch::ENV_HANDSHAKE_TOKEN,
    launch::ENV_DATA_DIR,
];

// os-fd cannot depend on proto (proto depends on it), so it restates the
// listen-fd number; this is where the two are held equal. The env VAR NAME
// is asserted equal at runtime instead, by a test below (`str::eq` is not
// `const`).
const _: () = assert!(agent24_os_fd::LISTEN_FD == launch::LISTEN_FD);

#[derive(Debug)]
pub enum EnvError {
    Missing(&'static str),
}

/// Three of the four spawn variables. The listen fd is deliberately NOT
/// here: only [`take_listener`] reads `A24_LISTEN_FD` (H1).
pub struct ModuleEnv {
    data_dir: PathBuf,
    callback_sock: PathBuf,
    handshake_token: String,
}

impl std::fmt::Debug for ModuleEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleEnv")
            .field("data_dir", &self.data_dir)
            .field("callback_sock", &self.callback_sock)
            .field("handshake_token", &"<redacted>")
            .finish()
    }
}

impl ModuleEnv {
    /// # Errors
    /// A variable is missing (token, unlike the paths, must also be UTF-8).
    pub fn from_env() -> Result<Self, EnvError> {
        Self::from_vars(|k| std::env::var_os(k))
    }

    /// Same parsing over any lookup — tests and fakes build an env without
    /// touching the process environment (M3).
    ///
    /// # Errors
    /// A variable is missing, or the token is not UTF-8.
    pub fn from_vars(mut get: impl FnMut(&str) -> Option<OsString>) -> Result<Self, EnvError> {
        let mut var = |n: &'static str| get(n).ok_or(EnvError::Missing(n));
        Ok(Self {
            data_dir: var(launch::ENV_DATA_DIR)?.into(),
            callback_sock: var(launch::ENV_CALLBACK_SOCK)?.into(),
            handshake_token: var(launch::ENV_HANDSHAKE_TOKEN)?
                .into_string()
                .map_err(|_| EnvError::Missing(launch::ENV_HANDSHAKE_TOKEN))?,
        })
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

/// What the module says in `initialize`, beyond the env-provided token.
#[derive(Debug, Clone)]
pub struct Hello<'a> {
    pub module: &'a str,
    pub manifest_bytes: &'a [u8],
    pub capabilities: &'a [&'a str],
    pub protocol_min: u32,
    pub protocol_max: u32,
}

/// Module-side parse of the handshake reply: same two fields as
/// [`crate::initialize::InitializeResult`] but WITHOUT `deny_unknown_fields`
/// — a kernel adding a reply field must not break modules built against an
/// older proto (§2.4 response rule, H2).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InitializeReply {
    pub protocol_version: u32,
    pub offer: Offer,
}

#[derive(Debug)]
pub enum ConnectError {
    Io(std::io::Error),
    Refused {
        code: i64,
        kind: Option<String>,
        message: String,
    },
    Protocol(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Refused {
                code,
                kind,
                message,
            } => write!(f, "handshake refused ({code} {kind:?}): {message}"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Kernel application error, parsed once (`code`, `data.kind`, message, raw
/// `data`).
#[derive(Debug, Clone, PartialEq)]
pub struct RpcErrorInfo {
    pub code: i64,
    pub kind: Option<String>,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcErrorInfo {
    fn from_json(err: &Value) -> Self {
        Self {
            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
            kind: err
                .get("data")
                .and_then(|d| d.get("kind"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            data: err.get("data").cloned(),
        }
    }
}

impl std::fmt::Display for RpcErrorInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            Some(kind) => write!(f, "{kind} ({}): {}", self.code, self.message),
            None => write!(f, "({}): {}", self.code, self.message),
        }
    }
}

/// Connection-level outcome of one call — same split as Sin90's
/// `TransportError` (NotSent = never left this process; ConnectionLost =
/// outcome unknown).
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    NotSent,
    ConnectionLost,
    Busy,
    FrameTooLarge,
    Timeout,
    IdCollision,
    Rpc(RpcErrorInfo),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSent => f.write_str("callback connection is closed; the call was never sent"),
            Self::ConnectionLost => f.write_str(
                "callback connection was lost while this call was in flight; its outcome is unknown",
            ),
            Self::Busy => write!(
                f,
                "{} calls already in flight on this connection",
                crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION
            ),
            Self::FrameTooLarge => write!(
                f,
                "params would serialize to more than {} bytes",
                crate::frame::MAX_FRAME_BYTES
            ),
            Self::Timeout => f.write_str("no response within the fallback deadline"),
            Self::IdCollision => f.write_str("internal bug: call id already in flight on this connection"),
            Self::Rpc(e) => write!(f, "kernel rejected the call: {e}"),
        }
    }
}

impl std::error::Error for CallError {}

/// Per-call knobs. `response_timeout` must exceed the kernel's own method
/// timeout so the kernel's specific answer wins the race.
#[derive(Debug, Clone, Copy)]
pub struct CallOptions {
    pub response_timeout: Duration,
    /// `None` = fail fast with `Busy` when all slots are taken.
    pub slot_wait: Option<Duration>,
}

pub const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(35);
const _: () = assert!(DEFAULT_RESPONSE_TIMEOUT.as_secs() > crate::rpc::CALL_TIMEOUT.as_secs());

impl Default for CallOptions {
    fn default() -> Self {
        Self {
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            slot_wait: None,
        }
    }
}

/// How long a write (including the mandatory flush) may take before the
/// connection is considered dead.
const WRITE_TIMEOUT: Duration = crate::rpc::WRITE_TIMEOUT;

/// Writer queue depth: enough for every in-flight call's request (bounded by
/// the semaphore at `MAX_IN_FLIGHT_PER_CONNECTION`) plus headroom for
/// `$/cancelRequest` notifications racing in around the same time.
const WRITER_QUEUE_CAPACITY: usize = 2 * crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION + 8;

struct PendingState {
    /// Set exactly once, by whichever of the writer/reader tasks first
    /// decides the connection is dead — see [`declare_dead`]. Checked inside
    /// this same lock at `call()`'s insert step so "the connection is already
    /// closed" and "insert my waiter" can never race each other.
    closed: bool,
    map: HashMap<String, oneshot::Sender<Result<Value, CallError>>>,
}

type Pending = Arc<StdMutex<PendingState>>;

/// Wakes the writer/reader tasks when the connection dies, and makes sure
/// [`FatalHook`] runs exactly once even if both tasks notice at once.
struct DeathSignal {
    fired: AtomicBool,
    notify: Notify,
}

/// Runs exactly once per connection, the first time either task calls it:
/// fails every still-pending call with [`CallError::ConnectionLost`], closes
/// `semaphore` (any `acquire_owned`/slot-waiting call already waiting for a
/// slot wakes immediately with an error instead of riding out its whole
/// timeout), wakes the other task, and finally runs [`FatalHook`].
fn declare_dead(
    death: &DeathSignal,
    pending: &Pending,
    semaphore: &Semaphore,
    on_fatal: &FatalHook,
) {
    if death.fired.swap(true, Ordering::SeqCst) {
        return; // the other side already ran this.
    }
    fail_all_pending(pending, CallError::ConnectionLost);
    semaphore.close();
    death.notify.notify_waiters();
    on_fatal.fire();
}

fn fail_all_pending(pending: &Pending, err: CallError) {
    let drained: Vec<_> = {
        let mut guard = match pending.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.closed = true;
        guard.map.drain().collect()
    };
    for (_, tx) in drained {
        let _ = tx.send(Err(err.clone()));
    }
}

fn dispatch_response(pending: &Pending, resp: &Value) {
    let Some(id) = resp.get("id").and_then(Value::as_str) else {
        tracing::warn!(?resp, "module: response with no string id; dropping");
        return;
    };
    let sender = {
        let mut guard = match pending.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.map.remove(id)
    };
    let Some(sender) = sender else {
        // Late response to an id this connection no longer has a waiter for
        // (typically: the caller cancelled it). Not an error — drop it.
        tracing::debug!(id, "module: dropped a response with no waiting caller");
        return;
    };
    let result = if let Some(err) = resp.get("error") {
        Err(CallError::Rpc(RpcErrorInfo::from_json(err)))
    } else {
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    };
    let _ = sender.send(result);
}

/// Frees the in-flight slot for one call and, if it had not already been
/// answered, tells the kernel to stop working on it.
struct CallGuard {
    id: String,
    pending: Pending,
    write_tx: mpsc::Sender<Vec<u8>>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        // `dispatch_response`/`fail_all_pending` remove the entry BEFORE
        // sending a result, so by the time a normally-completed (or
        // already-failed) call reaches this drop, `remove` here finds
        // nothing and sends no cancel. Only a call abandoned before any
        // answer arrived — future dropped, or timed out — still has an entry
        // here.
        let still_pending = match self.pending.lock() {
            Ok(mut guard) => guard.map.remove(&self.id).is_some(),
            Err(poisoned) => poisoned.into_inner().map.remove(&self.id).is_some(),
        };
        if still_pending {
            let notice = json!({
                "jsonrpc": "2.0",
                "method": crate::rpc::CANCEL_METHOD,
                "params": { "id": self.id },
            });
            let mut bytes = serde_json::to_vec(&notice).unwrap_or_default();
            bytes.push(b'\n');
            // Best-effort, non-blocking: `Drop` cannot `.await`, and a full
            // queue just means this cancel is silently skipped — the permit
            // is freed regardless, right below.
            let _ = self.write_tx.try_send(bytes);
        }
        // `_permit` drops here, releasing the semaphore slot.
    }
}

/// The multiplexed callback connection (single writer task, reader task
/// dispatching by id, 64 in-flight bound, cancel-on-drop). Never exposes the
/// underlying stream. No `notify`: nothing module→kernel uses one (L6);
/// `$/cancelRequest` is sent internally on drop.
///
/// No reconnect: the kernel serves exactly one callback connection per
/// generation (`endpoint::CallbackListener::accept_one`, D1). Once this
/// decides the connection is dead it runs [`FatalHook`] exactly once and
/// stays dead; every future `call` fails fast with [`CallError::NotSent`].
pub struct Connection {
    offer: Offer,
    write_tx: mpsc::Sender<Vec<u8>>,
    pending: Pending,
    next_id: AtomicU64,
    semaphore: Arc<Semaphore>,
    death: Arc<DeathSignal>,
    writer_task: JoinHandle<()>,
    reader_task: JoinHandle<()>,
}

impl Connection {
    /// Dial `env.callback_sock`, send `initialize`, parse the reply as
    /// [`InitializeReply`], require `protocol_version` within
    /// `hello.protocol_min..=hello.protocol_max` and no bytes after the
    /// reply, then spawn the mux tasks.
    ///
    /// # Errors
    /// See [`ConnectError`].
    pub async fn connect_from_env(
        env: &ModuleEnv,
        hello: &Hello<'_>,
        on_fatal: FatalHook,
    ) -> Result<Self, ConnectError> {
        let stream = UnixStream::connect(&env.callback_sock)
            .await
            .map_err(ConnectError::Io)?;
        let (offer, stream) = handshake(stream, env, hello).await?;
        Ok(Self::spawn(stream, offer, on_fatal))
    }

    fn spawn(stream: UnixStream, offer: Offer, on_fatal: FatalHook) -> Self {
        Self::spawn_with_write_timeout(stream, offer, on_fatal, WRITE_TIMEOUT)
    }

    fn spawn_with_write_timeout(
        stream: UnixStream,
        offer: Offer,
        on_fatal: FatalHook,
        write_timeout: Duration,
    ) -> Self {
        let (read_half, write_half) = stream.into_split();
        let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(WRITER_QUEUE_CAPACITY);
        let pending: Pending = Arc::new(StdMutex::new(PendingState {
            closed: false,
            map: HashMap::new(),
        }));
        let death = Arc::new(DeathSignal {
            fired: AtomicBool::new(false),
            notify: Notify::new(),
        });
        let semaphore = Arc::new(Semaphore::new(crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION));

        let writer_death = Arc::clone(&death);
        let writer_pending = Arc::clone(&pending);
        let writer_semaphore = Arc::clone(&semaphore);
        let writer_on_fatal = on_fatal.clone();
        let writer_task = tokio::spawn(async move {
            let mut write_half = write_half;
            loop {
                // Register interest in `notify` BEFORE checking `fired` —
                // tokio's documented safe pattern for closing the gap
                // between "we checked and it was false" and "we started
                // waiting".
                let notified = writer_death.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if writer_death.fired.load(Ordering::SeqCst) {
                    break;
                }
                tokio::select! {
                    biased;
                    () = notified.as_mut() => break,
                    maybe_bytes = write_rx.recv() => {
                        let Some(bytes) = maybe_bytes else { break };
                        let outcome = tokio::time::timeout(write_timeout, async {
                            write_half.write_all(&bytes).await?;
                            write_half.flush().await
                        })
                        .await;
                        match outcome {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => {
                                tracing::warn!(error = %e, "module: transport write failed; connection considered dead");
                                declare_dead(&writer_death, &writer_pending, &writer_semaphore, &writer_on_fatal);
                                break;
                            }
                            Err(_elapsed) => {
                                tracing::warn!(?write_timeout, "module: transport write timed out; connection considered dead");
                                declare_dead(&writer_death, &writer_pending, &writer_semaphore, &writer_on_fatal);
                                break;
                            }
                        }
                    }
                }
            }
            let _ = write_half.shutdown().await;
        });

        let reader_death = Arc::clone(&death);
        let reader_pending = Arc::clone(&pending);
        let reader_semaphore = Arc::clone(&semaphore);
        let reader_on_fatal = on_fatal;
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            loop {
                let notified = reader_death.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if reader_death.fired.load(Ordering::SeqCst) {
                    break;
                }
                tokio::select! {
                    biased;
                    () = notified.as_mut() => break,
                    frame_result = crate::rpc::read_frame_async(&mut reader) => {
                        match frame_result {
                            Ok(line) => match serde_json::from_slice::<Value>(&line) {
                                Ok(resp) => dispatch_response(&reader_pending, &resp),
                                Err(e) => {
                                    tracing::warn!(error = %e, "module: received a non-JSON frame; connection considered dead");
                                    declare_dead(&reader_death, &reader_pending, &reader_semaphore, &reader_on_fatal);
                                    break;
                                }
                            },
                            Err(e) => {
                                tracing::debug!(error = %e, "module: read ended; connection considered dead");
                                declare_dead(&reader_death, &reader_pending, &reader_semaphore, &reader_on_fatal);
                                break;
                            }
                        }
                    }
                }
            }
        });

        Self {
            offer,
            write_tx,
            pending,
            next_id: AtomicU64::new(1),
            semaphore,
            death,
            writer_task,
            reader_task,
        }
    }

    /// What this connection is authorised to call (§2.4's "no reconnect, no
    /// mutable `Offer` after construction").
    #[must_use]
    pub fn offer(&self) -> &Offer {
        &self.offer
    }

    #[must_use]
    pub fn is_alive(&self) -> bool {
        !self.death.fired.load(Ordering::SeqCst)
    }

    /// Issue one call and await its response.
    ///
    /// Dropping the returned future before it resolves sends
    /// `$/cancelRequest` for its id (the plan's "cancel").
    ///
    /// # Errors
    /// See [`CallError`].
    pub async fn call(
        &self,
        method: &'static str,
        params: Value,
        opts: CallOptions,
    ) -> Result<Value, CallError> {
        let (id, encoded) = self.encode_request(method, params)?;
        let permit = match opts.slot_wait {
            None => match Arc::clone(&self.semaphore).try_acquire_owned() {
                Ok(permit) => permit,
                // `declare_dead` closes the semaphore, so a dead connection
                // answers `NotSent` (accurate: definitely not going
                // anywhere) rather than `Busy` (which would suggest
                // retrying makes sense).
                Err(tokio::sync::TryAcquireError::Closed) => return Err(CallError::NotSent),
                Err(tokio::sync::TryAcquireError::NoPermits) => return Err(CallError::Busy),
            },
            Some(wait) => {
                match tokio::time::timeout(wait, Arc::clone(&self.semaphore).acquire_owned()).await
                {
                    Ok(Ok(permit)) => permit,
                    Ok(Err(_closed)) => return Err(CallError::NotSent),
                    Err(_elapsed) => return Err(CallError::Busy),
                }
            }
        };
        self.send_and_await(id, encoded, permit, opts.response_timeout)
            .await
    }

    /// Serializes the envelope and applies the size check — before a
    /// semaphore permit is taken, regardless of `opts`.
    fn encode_request(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<(String, Vec<u8>), CallError> {
        let id = format!("c-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut encoded = serde_json::to_vec(&req).unwrap_or_default();
        if encoded.len() > crate::frame::MAX_FRAME_BYTES {
            return Err(CallError::FrameTooLarge);
        }
        encoded.push(b'\n');
        Ok((id, encoded))
    }

    /// Registers the waiter, hands the encoded frame to the writer, and
    /// awaits the response (or `response_timeout`).
    async fn send_and_await(
        &self,
        id: String,
        encoded: Vec<u8>,
        permit: tokio::sync::OwnedSemaphorePermit,
        response_timeout: Duration,
    ) -> Result<Value, CallError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = match self.pending.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            if guard.closed {
                return Err(CallError::NotSent);
            }
            match guard.map.entry(id.clone()) {
                Entry::Occupied(_) => return Err(CallError::IdCollision),
                Entry::Vacant(v) => {
                    v.insert(tx);
                }
            }
        }

        // Holds the permit and, on drop before a normal completion, sends
        // `$/cancelRequest` and frees the pending-map slot.
        let _guard = CallGuard {
            id: id.clone(),
            pending: Arc::clone(&self.pending),
            write_tx: self.write_tx.clone(),
            _permit: permit,
        };

        if let Err(e) = self.write_tx.send(encoded).await {
            // Do NOT short-circuit here with `NotSent`: a `send` only fails
            // once the writer task has ended, which only happens through
            // `declare_dead` — and `declare_dead` already drained `pending`,
            // including this call's entry, BEFORE the writer dropped its
            // receiver. So `rx` below already has the right answer queued.
            tracing::debug!(error = %e, id, "module: write-queue send failed (connection already dying)");
        }

        match tokio::time::timeout(response_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_recv_error)) => Err(CallError::ConnectionLost),
            Err(_elapsed) => Err(CallError::Timeout),
        }
    }

    #[cfg(test)]
    fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.writer_task.abort();
        self.reader_task.abort();
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("offer", &self.offer)
            .field("is_alive", &self.is_alive())
            .finish_non_exhaustive()
    }
}

/// Dial, run `initialize`, and hand back the granted `Offer` plus the raw
/// stream (past the handshake, ready for [`Connection::spawn`]).
async fn handshake(
    stream: UnixStream,
    env: &ModuleEnv,
    hello: &Hello<'_>,
) -> Result<(Offer, UnixStream), ConnectError> {
    let mut reader = BufReader::new(stream);

    let id = "1".to_owned();
    let params = InitializeParams {
        protocol_versions: VersionRange::new(hello.protocol_min, hello.protocol_max),
        module: hello.module.to_owned(),
        manifest_digest: crate::manifest::manifest_digest(hello.manifest_bytes),
        auth_token: env.handshake_token.clone(),
        capabilities: hello.capabilities.iter().map(|s| (*s).to_owned()).collect(),
    };
    let req = InitializeRequest {
        jsonrpc: "2.0".to_owned(),
        method: INITIALIZE_METHOD.to_owned(),
        id: id.clone(),
        params,
    };
    let mut encoded =
        serde_json::to_vec(&req).map_err(|e| ConnectError::Protocol(e.to_string()))?;
    encoded.push(b'\n');
    reader
        .get_mut()
        .write_all(&encoded)
        .await
        .map_err(ConnectError::Io)?;
    reader.get_mut().flush().await.map_err(ConnectError::Io)?;

    let line = crate::rpc::read_frame_async(&mut reader)
        .await
        .map_err(|e| ConnectError::Protocol(e.to_string()))?;
    let resp: Value =
        serde_json::from_slice(&line).map_err(|e| ConnectError::Protocol(e.to_string()))?;
    let got_id = resp.get("id").and_then(Value::as_str).unwrap_or_default();
    if got_id != id {
        return Err(ConnectError::Protocol(format!(
            "handshake response id mismatch: sent {id}, got {got_id}"
        )));
    }
    if let Some(err) = resp.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let kind = err
            .get("data")
            .and_then(|d| d.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return Err(ConnectError::Refused {
            code,
            kind,
            message,
        });
    }
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    let reply: InitializeReply =
        serde_json::from_value(result).map_err(|e| ConnectError::Protocol(e.to_string()))?;
    if reply.protocol_version < hello.protocol_min || reply.protocol_version > hello.protocol_max {
        return Err(ConnectError::Protocol(format!(
            "kernel chose protocol version {} outside of the declared range [{}, {}]",
            reply.protocol_version, hello.protocol_min, hello.protocol_max
        )));
    }

    // Nothing should have arrived on the socket beyond this one handshake
    // response. If the `BufReader` nonetheless has unconsumed bytes
    // buffered, handing back only `reader.into_inner()` would silently drop
    // them, so that case is treated as a protocol violation rather than
    // risking lost bytes on the connection `Connection::spawn` is about to
    // take over (J-S12).
    if !reader.buffer().is_empty() {
        return Err(ConnectError::Protocol(
            "kernel sent unexpected extra bytes immediately after the initialize response"
                .to_owned(),
        ));
    }
    Ok((reply.offer, reader.into_inner()))
}

#[derive(Debug)]
pub enum ListenError {
    Inherit(agent24_os_fd::InheritError),
    Io(std::io::Error),
}

/// The kernel-bound listener, taken over and ready for tokio. A named struct
/// (not a type alias, J-S1b) so the SDK can hold it in `Module` without ever
/// writing a socket type in its own source (J-S1).
#[derive(Debug)]
pub struct InheritedListener(tokio::net::UnixListener);

impl InheritedListener {
    /// Hand the listener to a server (`axum::serve(l.into_tokio(), app)`).
    #[must_use]
    pub fn into_tokio(self) -> tokio::net::UnixListener {
        self.0
    }
}

/// Delegates the fd takeover to `agent24_os_fd::take_inherited_listener`
/// (reads `A24_LISTEN_FD` itself, validates, sets `FD_CLOEXEC`, once per
/// process) and wraps it for tokio. The SDK calls this from
/// `ModuleBuilder::connect()` (production path only, v3 M-3).
///
/// Once per PROCESS, and `cargo test` runs every test of one test binary in
/// one process: at most one test per test binary may reach this (directly,
/// or via a `connect()` without `with_env`). Everything else uses
/// `testing::fake_kernel` / `FakeEndpoint` + `with_env`.
///
/// Must be called from inside a tokio runtime: `UnixListener::from_std`
/// registers the fd with tokio's reactor, which panics ("there is no reactor
/// running") outside one. Production callers already run inside
/// `#[tokio::main]`/`ModuleBuilder::connect()`'s runtime; a test that calls
/// this directly needs `#[tokio::test]` (or an equivalent runtime guard) for
/// the same reason.
///
/// # Errors
/// See [`ListenError`].
pub fn take_listener() -> Result<InheritedListener, ListenError> {
    let std = agent24_os_fd::take_inherited_listener().map_err(ListenError::Inherit)?;
    tokio::net::UnixListener::from_std(std)
        .map(InheritedListener)
        .map_err(ListenError::Io)
}

/// In-memory fake kernel for module-side unit tests (`test-util`). Function
/// names and signatures are Sin90 `clients/test_support.rs`'s (`135ddb7`),
/// so Sin90's test modules compile unchanged against a one-line shim (H4).
#[cfg(feature = "test-util")]
pub mod testing {
    // This is fixture code for other crates' tests, never a production
    // path (the whole module is gated behind `test-util`); panicking on an
    // impossible-in-a-test failure (a fake socket pair, a `Value` that
    // always serializes) is the right behaviour for a test helper, so the
    // workspace's `unwrap_used`/`expect_used` deny does not apply here.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    use super::{Connection, FatalHook, ModuleEnv, Offer};

    /// The fake kernel's end of the socket pair — a persistent reader (so
    /// multiple `read_request` calls in one test never drop buffered bytes)
    /// plus a plain write half.
    pub struct FakePeer {
        reader: BufReader<OwnedReadHalf>,
        writer: OwnedWriteHalf,
    }

    #[must_use]
    pub fn noop_hook() -> FatalHook {
        FatalHook::new(|| {})
    }

    /// A hook that counts instead of exiting — the only kind a test may hand
    /// `ModuleBuilder::with_env` (M3).
    #[must_use]
    pub fn recording_hook() -> (FatalHook, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let m = Arc::clone(&n);
        (
            FatalHook::new(move || {
                m.fetch_add(1, Ordering::SeqCst);
            }),
            n,
        )
    }

    /// Builds a [`Connection`] with the given `Offer` injected directly (no
    /// real handshake) over one half of an in-memory socket pair, and hands
    /// back the other half as a [`FakePeer`] a test drives by hand.
    pub async fn fake_kernel(offer: Vec<String>) -> (Arc<Connection>, FakePeer) {
        let (a, b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        a.set_nonblocking(true).expect("nonblocking");
        b.set_nonblocking(true).expect("nonblocking");
        let stream = UnixStream::from_std(a).expect("tokio adopt");
        let peer = UnixStream::from_std(b).expect("tokio adopt");
        let conn = Connection::spawn(stream, Offer { provides: offer }, noop_hook());
        let (read_half, writer) = peer.into_split();
        let peer = FakePeer {
            reader: BufReader::new(read_half),
            writer,
        };
        (Arc::new(conn), peer)
    }

    /// Reads one JSON-RPC request line off `peer` — whichever the client
    /// under test just sent.
    pub async fn read_request(peer: &mut FakePeer) -> Value {
        let mut buf = Vec::new();
        peer.reader
            .read_until(b'\n', &mut buf)
            .await
            .expect("read from fake peer");
        serde_json::from_slice(&buf).expect("fake peer request is valid json")
    }

    /// Answers `req` with a successful `result`.
    pub async fn respond(peer: &mut FakePeer, req: &Value, result: Value) {
        let resp = json!({"jsonrpc": "2.0", "id": req["id"], "result": result});
        write_line(peer, &resp).await;
    }

    /// Answers `req` with an application error — `kind` empty means "no
    /// `data.kind` at all" (e.g. a bare `-32602`), matching how a real
    /// `-32602 invalid params` response has no `data` object at all.
    pub async fn respond_error(
        peer: &mut FakePeer,
        req: &Value,
        code: i64,
        kind: &str,
        message: &str,
    ) {
        let error = if kind.is_empty() {
            json!({"code": code, "message": message})
        } else {
            json!({"code": code, "message": message, "data": {"kind": kind}})
        };
        let resp = json!({"jsonrpc": "2.0", "id": req["id"], "error": error});
        write_line(peer, &resp).await;
    }

    /// Like [`respond_error`], but `data` is an arbitrary caller-built
    /// object (which must itself include `"kind"`) instead of just
    /// `{"kind": kind}`.
    pub async fn respond_error_with_data(
        peer: &mut FakePeer,
        req: &Value,
        code: i64,
        message: &str,
        data: Value,
    ) {
        let error = json!({"code": code, "message": message, "data": data});
        let resp = json!({"jsonrpc": "2.0", "id": req["id"], "error": error});
        write_line(peer, &resp).await;
    }

    async fn write_line(peer: &mut FakePeer, value: &Value) {
        let mut bytes = serde_json::to_vec(value).expect("value always serializes");
        bytes.push(b'\n');
        peer.writer
            .write_all(&bytes)
            .await
            .expect("write to fake peer");
    }

    /// A real callback endpoint under `dir` for handshake tests: the
    /// returned env points at it (never the process env).
    pub struct FakeEndpoint {
        listener: std::os::unix::net::UnixListener,
    }

    impl FakeEndpoint {
        /// # Panics
        /// `dir` cannot be bound (e.g. the path is too long for
        /// `sockaddr_un`, or a stale socket file could not be removed).
        #[must_use]
        pub fn bind(dir: &Path) -> (ModuleEnv, Self) {
            let sock_path = dir.join("cb.sock");
            let _ = std::fs::remove_file(&sock_path);
            let listener =
                std::os::unix::net::UnixListener::bind(&sock_path).expect("bind fake endpoint");
            listener.set_nonblocking(true).expect("nonblocking");
            let env = ModuleEnv::from_vars(|k| {
                Some(match k {
                    "A24_CALLBACK_SOCK" => sock_path.clone().into_os_string(),
                    "A24_HANDSHAKE_TOKEN" => "fake-token".into(),
                    _ => dir.as_os_str().to_owned(),
                })
            })
            .expect("fake endpoint env is always complete");
            (env, Self { listener })
        }

        /// Accept one connection, return the `initialize` params it sent,
        /// answer with `result` (the `InitializeResult`-shaped value), and
        /// hand back the peer for further raw interaction.
        ///
        /// # Panics
        /// The connection is not accepted, or the module's first frame is
        /// not readable as JSON.
        pub async fn accept_initialize(self, result: Value) -> (Value, FakePeer) {
            let std_stream = loop {
                match self.listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                    Err(e) => panic!("fake endpoint accept failed: {e}"),
                }
            };
            std_stream.set_nonblocking(true).expect("nonblocking");
            let stream = UnixStream::from_std(std_stream).expect("tokio adopt");
            let (read_half, mut writer) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut buf = Vec::new();
            reader
                .read_until(b'\n', &mut buf)
                .await
                .expect("read initialize request");
            let req: Value =
                serde_json::from_slice(&buf).expect("initialize request is valid json");
            let resp = json!({"jsonrpc": "2.0", "id": req["id"], "result": result});
            let mut bytes = serde_json::to_vec(&resp).expect("value always serializes");
            bytes.push(b'\n');
            writer
                .write_all(&bytes)
                .await
                .expect("write handshake reply");
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            (params, FakePeer { reader, writer })
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::AsyncBufReadExt;
    use tokio::net::UnixListener;
    use tokio::net::unix::OwnedReadHalf;

    /// A drop guard around a manually-created temp dir. `tempfile::TempDir`
    /// (already a dev-dependency, used elsewhere in this crate) was tried
    /// first, but `Builder::tempdir` appends its own random suffix on top of
    /// our prefix, and these particular temp dirs hold a `cb.sock` AF_UNIX
    /// path — that extra suffix was enough to blow macOS's ~104-byte
    /// `sun_path` limit ("path must be shorter than SUN_LEN"). Building the
    /// exact, length-budgeted name ourselves and only borrowing `tempfile`'s
    /// idea (a guard that removes the dir on drop) keeps both properties.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// B1 (external review of #515): plain `SystemTime::now()` nanoseconds
    /// collided under parallel `cargo test` on macOS (clock resolution),
    /// producing `AddrInUse`/`EEXIST` for the socket path built on top of
    /// this dir. A process-local counter makes each call unique regardless
    /// of clock resolution; the returned guard additionally cleans the
    /// directory up on drop instead of leaking it into the temp dir on every
    /// test run.
    fn tempdir() -> TempDir {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "a24proto-mod-{}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    async fn connected_pair() -> (UnixStream, UnixStream) {
        let dir = tempdir();
        let sock_path = dir.path().join("cb.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let client = UnixStream::connect(&sock_path).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    fn counting_hook() -> (FatalHook, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let hook_count = Arc::clone(&count);
        (
            FatalHook::new(move || {
                hook_count.fetch_add(1, Ordering::SeqCst);
            }),
            count,
        )
    }

    fn no_offer(conn: UnixStream, hook: FatalHook) -> Connection {
        Connection::spawn(conn, Offer::none(), hook)
    }

    async fn read_json_line(reader: &mut BufReader<OwnedReadHalf>) -> Value {
        let mut buf = Vec::new();
        reader.read_until(b'\n', &mut buf).await.unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    async fn write_json_line(writer: &mut tokio::net::unix::OwnedWriteHalf, value: &Value) {
        use tokio::io::AsyncWriteExt;
        let mut bytes = serde_json::to_vec(value).unwrap();
        bytes.push(b'\n');
        writer.write_all(&bytes).await.unwrap();
    }

    async fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if cond() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("condition did not become true within {timeout:?}");
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[test]
    fn os_fd_and_proto_agree_on_the_listen_fd_env_var_name() {
        assert_eq!(agent24_os_fd::ENV_LISTEN_FD, launch::ENV_LISTEN_FD);
    }

    // -- J-S19: lenient handshake reply parsing --------------------------

    #[test]
    fn initialize_reply_is_lenient_where_the_kernel_type_is_strict() {
        let with_extra = json!({
            "protocol_version": 1,
            "offer": {"provides": ["_a24/events/"], "x": "unexpected"},
            "future": "field",
        });
        assert!(
            serde_json::from_value::<crate::initialize::InitializeResult>(with_extra.clone())
                .is_err(),
            "the kernel's own type must still reject unknown fields"
        );
        let reply: InitializeReply = serde_json::from_value(with_extra).unwrap();
        assert_eq!(reply.protocol_version, 1);
        assert_eq!(reply.offer.provides, vec!["_a24/events/".to_string()]);

        let strict = crate::initialize::InitializeResult {
            protocol_version: 1,
            offer: Offer {
                provides: vec!["_a24/memory/private/".to_string()],
            },
        };
        let roundtrip: InitializeReply =
            serde_json::from_value(serde_json::to_value(&strict).unwrap()).unwrap();
        assert_eq!(roundtrip.protocol_version, strict.protocol_version);
        assert_eq!(roundtrip.offer, strict.offer);
    }

    // -- J-S12/handshake: real dial against a fake endpoint ---------------
    // (`testing` is `test-util`-gated; so are the two tests that use it.)

    #[cfg(feature = "test-util")]
    #[tokio::test]
    async fn connect_from_env_sends_manifest_derived_hello_and_parses_the_reply() {
        let dir = tempdir();
        let (env, endpoint) = testing::FakeEndpoint::bind(dir.path());
        let hello = Hello {
            module: "minimal",
            manifest_bytes: b"name: minimal\n",
            capabilities: &["events"],
            protocol_min: 1,
            protocol_max: 1,
        };
        let accept = endpoint.accept_initialize(json!({
            "protocol_version": 1,
            "offer": {"provides": ["_a24/events/"]},
        }));
        let connect = Connection::connect_from_env(&env, &hello, testing::noop_hook());
        let ((params, _peer), conn_result) = tokio::join!(accept, connect);
        let conn = conn_result.expect("handshake must succeed");
        assert_eq!(params["module"], "minimal");
        assert_eq!(
            params["manifest_digest"],
            crate::manifest::manifest_digest(hello.manifest_bytes)
        );
        assert_eq!(params["capabilities"], json!(["events"]));
        assert!(conn.offer().provides("_a24/events/emit"));
    }

    #[cfg(feature = "test-util")]
    #[tokio::test]
    async fn connect_from_env_rejects_residual_bytes_after_the_handshake() {
        let dir = tempdir();
        let sock_path = dir.path().join("cb.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let env = ModuleEnv::from_vars(|k| {
            Some(match k {
                "A24_CALLBACK_SOCK" => sock_path.clone().into_os_string(),
                "A24_HANDSHAKE_TOKEN" => "tok".into(),
                _ => dir.path().as_os_str().to_owned(),
            })
        })
        .unwrap();
        let hello = Hello {
            module: "m",
            manifest_bytes: b"x",
            capabilities: &[],
            protocol_min: 1,
            protocol_max: 1,
        };
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut writer) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut buf = Vec::new();
            reader.read_until(b'\n', &mut buf).await.unwrap();
            let req: Value = serde_json::from_slice(&buf).unwrap();
            let resp = json!({"jsonrpc":"2.0","id":req["id"],"result":{"protocol_version":1,"offer":{"provides":[]}}});
            let mut bytes = serde_json::to_vec(&resp).unwrap();
            bytes.push(b'\n');
            bytes.extend_from_slice(b"garbage-after-handshake");
            writer.write_all(&bytes).await.unwrap();
        });
        let result = Connection::connect_from_env(&env, &hello, testing::noop_hook()).await;
        assert!(
            matches!(result, Err(ConnectError::Protocol(_))),
            "got {result:?}"
        );
        server.await.unwrap();
    }

    // -- mux tests, migrated from Sin90 `adapter_agent24::transport` -----
    // (`135ddb7`, `src/adapter_agent24/transport.rs`), adapted to
    // `Connection::spawn`/`Connection::call`.

    #[tokio::test]
    async fn concurrent_calls_out_of_order_responses_each_get_own_result() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, mut server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let c1 = Arc::clone(&conn);
        let call_a = tokio::spawn(async move {
            c1.call("method.a", json!({"who": "a"}), CallOptions::default())
                .await
        });
        let req_a = read_json_line(&mut server_read).await;

        let c2 = Arc::clone(&conn);
        let call_b = tokio::spawn(async move {
            c2.call("method.b", json!({"who": "b"}), CallOptions::default())
                .await
        });
        let req_b = read_json_line(&mut server_read).await;

        assert_eq!(req_a["method"], "method.a");
        assert_eq!(req_b["method"], "method.b");
        let id_a = req_a["id"].as_str().unwrap().to_string();
        let id_b = req_b["id"].as_str().unwrap().to_string();
        assert_ne!(id_a, id_b, "each call must get a unique id");

        // Answer B FIRST, then A — deliberately out of request order.
        write_json_line(
            &mut server_write,
            &json!({"jsonrpc": "2.0", "id": id_b, "result": {"who": "b"}}),
        )
        .await;
        write_json_line(
            &mut server_write,
            &json!({"jsonrpc": "2.0", "id": id_a, "result": {"who": "a"}}),
        )
        .await;

        let result_a = call_a.await.unwrap().unwrap();
        let result_b = call_b.await.unwrap().unwrap();
        assert_eq!(result_a, json!({"who": "a"}));
        assert_eq!(result_b, json!({"who": "b"}));
    }

    #[tokio::test]
    async fn the_65th_in_flight_call_is_rejected_busy() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, _server_write) = server.into_split();
        tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                if reader.read_until(b'\n', &mut buf).await.unwrap_or(0) == 0 {
                    break;
                }
            }
        });

        let mut handles = Vec::new();
        for i in 0..crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION {
            let conn = Arc::clone(&conn);
            handles.push(tokio::spawn(async move {
                conn.call(
                    Box::leak(format!("method.{i}").into_boxed_str()),
                    json!({}),
                    CallOptions::default(),
                )
                .await
            }));
        }
        wait_until(|| conn.available_permits() == 0, Duration::from_secs(1)).await;

        let busy = conn
            .call("method.65th", json!({}), CallOptions::default())
            .await;
        assert!(
            matches!(busy, Err(CallError::Busy)),
            "the 65th concurrent call must be rejected Busy immediately, got {busy:?}"
        );

        for h in handles {
            h.abort();
        }
    }

    #[tokio::test]
    async fn cancel_on_drop_releases_slot_and_writes_exact_cancel_frame() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, _server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let c = Arc::clone(&conn);
        let handle = tokio::spawn(async move {
            c.call("routine.upsert", json!({"a": 1}), CallOptions::default())
                .await
        });

        let request_line = read_json_line(&mut server_read).await;
        assert_eq!(request_line["method"], "routine.upsert");
        assert_eq!(request_line["id"], "c-1");
        assert_eq!(
            conn.available_permits(),
            crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION - 1,
            "the call holds its slot until answered or cancelled"
        );

        handle.abort();
        let _ = handle.await;

        let cancel_frame = read_frame_bytes(&mut server_read).await;
        let expected: Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":"c-1"}}"#,
        )
        .unwrap();
        let actual: Value = serde_json::from_slice(&cancel_frame).unwrap();
        assert_eq!(
            actual, expected,
            "cancel frame must match the SPEC wire shape exactly"
        );
        assert!(
            actual.get("id").is_none(),
            "a notification must not carry a top-level id"
        );

        wait_until(
            || conn.available_permits() == crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION,
            Duration::from_secs(1),
        )
        .await;
    }

    #[tokio::test]
    async fn cancel_is_not_sent_when_the_call_completes_normally() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, mut server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let c = Arc::clone(&conn);
        let handle = tokio::spawn(async move {
            c.call("routine.upsert", json!({}), CallOptions::default())
                .await
        });
        let request_line = read_json_line(&mut server_read).await;
        let id = request_line["id"].as_str().unwrap().to_string();

        write_json_line(
            &mut server_write,
            &json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}}),
        )
        .await;
        let result = handle.await.unwrap().unwrap();
        assert_eq!(result, json!({"ok": true}));

        // Positive control: a normally-completed call must NOT also write a
        // cancel frame — prove it by sending one more real call and checking
        // the kernel sees THAT request next.
        let c2 = Arc::clone(&conn);
        let handle2 = tokio::spawn(async move {
            c2.call("routine.other", json!({}), CallOptions::default())
                .await
        });
        let next_line = read_json_line(&mut server_read).await;
        assert_eq!(next_line["method"], "routine.other");
        let id2 = next_line["id"].as_str().unwrap().to_string();
        write_json_line(
            &mut server_write,
            &json!({"jsonrpc": "2.0", "id": id2, "result": {}}),
        )
        .await;
        handle2.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn late_response_after_cancel_is_dropped_and_connection_keeps_working() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, mut server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let c = Arc::clone(&conn);
        let handle = tokio::spawn(async move {
            c.call("slow.method", json!({}), CallOptions::default())
                .await
        });
        let request_line = read_json_line(&mut server_read).await;
        let id = request_line["id"].as_str().unwrap().to_string();

        handle.abort();
        let _ = handle.await;
        let _cancel_frame = read_json_line(&mut server_read).await;

        // The kernel answers anyway, after the caller already gave up.
        write_json_line(
            &mut server_write,
            &json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}}),
        )
        .await;

        // Positive control: the connection is still genuinely usable.
        let c2 = Arc::clone(&conn);
        let handle2 = tokio::spawn(async move {
            c2.call("after.stale", json!({}), CallOptions::default())
                .await
        });
        let next = read_json_line(&mut server_read).await;
        assert_eq!(next["method"], "after.stale");
        let id2 = next["id"].as_str().unwrap().to_string();
        write_json_line(
            &mut server_write,
            &json!({"jsonrpc": "2.0", "id": id2, "result": {"still": "alive"}}),
        )
        .await;
        let result = handle2.await.unwrap().unwrap();
        assert_eq!(result, json!({"still": "alive"}));
    }

    #[tokio::test]
    async fn disconnect_in_flight_call_gets_connection_lost_and_never_retries() {
        let dir = tempdir();
        let sock_path = dir.path().join("cb.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let client = UnixStream::connect(&sock_path).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();

        let (hook, count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let c = Arc::clone(&conn);
        let handle = tokio::spawn(async move {
            c.call("routine.upsert", json!({}), CallOptions::default())
                .await
        });
        let request_line = read_json_line(&mut server_read).await;
        assert_eq!(request_line["method"], "routine.upsert");

        let second_frame_on_same_connection =
            tokio::time::timeout(Duration::from_millis(100), read_json_line(&mut server_read))
                .await;
        assert!(
            second_frame_on_same_connection.is_err(),
            "the kernel must see exactly one frame for this call, not a retry"
        );

        drop(server_write);
        drop(server_read);

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(CallError::ConnectionLost)),
            "an in-flight call must get ConnectionLost on disconnect, got {result:?}"
        );
        wait_until(|| !conn.is_alive(), Duration::from_secs(1)).await;
        wait_until(|| count.load(Ordering::SeqCst) == 1, Duration::from_secs(1)).await;

        let second_connection =
            tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
        assert!(
            second_connection.is_err(),
            "Connection must never attempt to reconnect"
        );
    }

    #[tokio::test]
    async fn call_after_close_fails_fast_as_not_sent() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, server_write) = server.into_split();

        drop(server_write);
        drop(server_read);
        wait_until(|| !conn.is_alive(), Duration::from_secs(1)).await;

        let started = tokio::time::Instant::now();
        let result = conn
            .call("routine.upsert", json!({}), CallOptions::default())
            .await;
        assert!(
            matches!(result, Err(CallError::NotSent)),
            "a call on an already-dead connection must fail fast as NotSent, got {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "NotSent must be fast"
        );
    }

    #[tokio::test]
    async fn frame_too_large_is_rejected_before_reaching_the_writer() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = no_offer(client, hook);
        let (server_read, _server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let huge = "x".repeat(crate::frame::MAX_FRAME_BYTES + 1);
        let result = conn
            .call(
                "routine.upsert",
                json!({"huge": huge}),
                CallOptions::default(),
            )
            .await;
        assert!(
            matches!(result, Err(CallError::FrameTooLarge)),
            "got {result:?}"
        );

        let nothing =
            tokio::time::timeout(Duration::from_millis(100), read_json_line(&mut server_read))
                .await;
        assert!(
            nothing.is_err(),
            "an oversized call must never reach the writer/wire"
        );
    }

    #[tokio::test]
    async fn frame_of_exactly_max_bytes_is_accepted() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = no_offer(client, hook);
        let (server_read, mut server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let method = "routine.upsert";
        let id = "c-1";
        let base = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": {"pad": ""}
        }))
        .unwrap();
        let pad_len = crate::frame::MAX_FRAME_BYTES - base.len();
        let pad = "x".repeat(pad_len);
        let envelope_len = base.len() + pad_len;
        assert_eq!(envelope_len, crate::frame::MAX_FRAME_BYTES);

        let call = conn.call(method, json!({"pad": pad}), CallOptions::default());
        let joined = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                async {
                    let line = read_json_line(&mut server_read).await;
                    write_json_line(
                        &mut server_write,
                        &json!({"jsonrpc": "2.0", "id": line["id"], "result": {}}),
                    )
                    .await;
                    line
                },
                call,
            )
        })
        .await;
        let (request_line, call_result) =
            joined.expect("exactly-MAX call must reach the wire and round-trip");
        assert_eq!(request_line["method"], method);
        assert_eq!(call_result.unwrap(), json!({}));
    }

    #[tokio::test]
    async fn response_timeout_sends_cancel_and_returns_timeout_error() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = no_offer(client, hook);
        let (server_read, _server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let opts = CallOptions {
            response_timeout: Duration::from_millis(50),
            slot_wait: None,
        };
        let request_line_fut = read_json_line(&mut server_read);
        let call_fut = conn.call("slow.method", json!({}), opts);
        let (request_line, result) = tokio::join!(request_line_fut, call_fut);
        assert_eq!(request_line["method"], "slow.method");
        assert!(matches!(result, Err(CallError::Timeout)), "got {result:?}");

        let cancel_frame = read_json_line(&mut server_read).await;
        assert_eq!(cancel_frame["method"], crate::rpc::CANCEL_METHOD);
        assert!(cancel_frame.get("id").is_none());
    }

    #[tokio::test]
    async fn write_timeout_triggers_on_fatal_once_and_in_flight_calls_get_connection_lost() {
        let (client, server) = connected_pair().await;
        let (hook, count) = counting_hook();
        let conn = Arc::new(Connection::spawn_with_write_timeout(
            client,
            Offer::none(),
            hook,
            Duration::from_millis(50),
        ));
        // Deliberately never read from `server` — that is the point.
        let _server = server;

        let big = "x".repeat(200_000);
        let mut handles = Vec::new();
        for i in 0..20 {
            let conn = Arc::clone(&conn);
            let payload = big.clone();
            handles.push(tokio::spawn(async move {
                conn.call(
                    Box::leak(format!("flood.{i}").into_boxed_str()),
                    json!({"data": payload}),
                    CallOptions::default(),
                )
                .await
            }));
        }

        let mut saw_connection_lost = false;
        for h in handles {
            match h.await.unwrap() {
                Err(CallError::ConnectionLost) => saw_connection_lost = true,
                Err(CallError::NotSent) => {}
                other => panic!(
                    "expected ConnectionLost/NotSent once the write times out, got {other:?}"
                ),
            }
        }
        assert!(
            saw_connection_lost,
            "at least one call must have genuinely been in flight"
        );
        wait_until(|| count.load(Ordering::SeqCst) == 1, Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn declare_dead_fires_on_fatal_exactly_once_under_concurrent_callers() {
        let (hook, count) = counting_hook();
        let death = Arc::new(DeathSignal {
            fired: AtomicBool::new(false),
            notify: Notify::new(),
        });
        let pending: Pending = Arc::new(StdMutex::new(PendingState {
            closed: false,
            map: HashMap::new(),
        }));
        let semaphore = Arc::new(Semaphore::new(crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION));

        let mut handles = Vec::new();
        for _ in 0..100 {
            let death = Arc::clone(&death);
            let pending = Arc::clone(&pending);
            let semaphore = Arc::clone(&semaphore);
            let hook = hook.clone();
            handles.push(tokio::spawn(async move {
                declare_dead(&death, &pending, &semaphore, &hook);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "on_fatal must fire exactly once"
        );
        assert!(
            semaphore.is_closed(),
            "declare_dead must close the semaphore"
        );
    }

    #[tokio::test]
    async fn on_fatal_is_triggered_by_a_non_json_frame_from_the_kernel() {
        let (client, server) = connected_pair().await;
        let (hook, count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, mut server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let c = Arc::clone(&conn);
        let handle = tokio::spawn(async move {
            c.call("routine.upsert", json!({}), CallOptions::default())
                .await
        });
        let _request_line = read_json_line(&mut server_read).await;

        server_write.write_all(b"not json at all\n").await.unwrap();

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(CallError::ConnectionLost)),
            "got {result:?}"
        );
        wait_until(|| count.load(Ordering::SeqCst) == 1, Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn on_fatal_is_triggered_by_an_oversized_incoming_frame() {
        let (client, server) = connected_pair().await;
        let (hook, count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, mut server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let c = Arc::clone(&conn);
        let handle = tokio::spawn(async move {
            c.call("routine.upsert", json!({}), CallOptions::default())
                .await
        });
        let _request_line = read_json_line(&mut server_read).await;

        let mut huge = vec![b'x'; crate::frame::MAX_FRAME_BYTES + 1];
        huge.push(b'\n');
        server_write.write_all(&huge).await.unwrap();

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(CallError::ConnectionLost)),
            "got {result:?}"
        );
        wait_until(|| count.load(Ordering::SeqCst) == 1, Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn call_with_slot_wait_succeeds_once_a_slot_frees_up() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, mut server_write) = server.into_split();
        let mut server_read = BufReader::new(server_read);

        let mut handles = Vec::new();
        let mut ids = Vec::new();
        for i in 0..crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION {
            let c = Arc::clone(&conn);
            handles.push(tokio::spawn(async move {
                c.call(
                    Box::leak(format!("method.{i}").into_boxed_str()),
                    json!({}),
                    CallOptions::default(),
                )
                .await
            }));
        }
        for _ in 0..crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION {
            ids.push(
                read_json_line(&mut server_read).await["id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        wait_until(|| conn.available_permits() == 0, Duration::from_secs(1)).await;

        let c = Arc::clone(&conn);
        let opts = CallOptions {
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            slot_wait: Some(Duration::from_secs(2)),
        };
        let waiter = tokio::spawn(async move { c.call("event.emit", json!({}), opts).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !waiter.is_finished(),
            "the waiter must still be blocked on a slot"
        );

        write_json_line(
            &mut server_write,
            &json!({"jsonrpc": "2.0", "id": ids[0], "result": {}}),
        )
        .await;

        let waiter_request = read_json_line(&mut server_read).await;
        assert_eq!(waiter_request["method"], "event.emit");
        write_json_line(
            &mut server_write,
            &json!({"jsonrpc": "2.0", "id": waiter_request["id"], "result": {"ok": true}}),
        )
        .await;
        let result = waiter.await.unwrap();
        assert_eq!(result.unwrap(), json!({"ok": true}));

        for h in handles {
            h.abort();
        }
    }

    #[tokio::test]
    async fn call_with_slot_wait_times_out_to_busy_when_no_slot_frees() {
        let (client, server) = connected_pair().await;
        let (hook, _count) = counting_hook();
        let conn = Arc::new(no_offer(client, hook));
        let (server_read, _server_write) = server.into_split();
        tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                if reader.read_until(b'\n', &mut buf).await.unwrap_or(0) == 0 {
                    break;
                }
            }
        });

        let mut handles = Vec::new();
        for i in 0..crate::rpc::MAX_IN_FLIGHT_PER_CONNECTION {
            let c = Arc::clone(&conn);
            handles.push(tokio::spawn(async move {
                c.call(
                    Box::leak(format!("method.{i}").into_boxed_str()),
                    json!({}),
                    CallOptions::default(),
                )
                .await
            }));
        }
        wait_until(|| conn.available_permits() == 0, Duration::from_secs(1)).await;

        let started = tokio::time::Instant::now();
        let opts = CallOptions {
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            slot_wait: Some(Duration::from_millis(50)),
        };
        let result = conn.call("event.emit", json!({}), opts).await;
        assert!(matches!(result, Err(CallError::Busy)), "got {result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "must give up around the requested wait"
        );

        for h in handles {
            h.abort();
        }
    }

    async fn read_frame_bytes(reader: &mut BufReader<OwnedReadHalf>) -> Vec<u8> {
        let mut buf = Vec::new();
        reader.read_until(b'\n', &mut buf).await.unwrap();
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        buf
    }
}
