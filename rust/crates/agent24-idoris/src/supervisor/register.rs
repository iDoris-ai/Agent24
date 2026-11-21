//! Server side of the frozen registration protocol v1 (§2.10).
use super::{Ports, SupervisorError, discovery, mint_token};
use agent24_os_proc::AuditToken;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::HashSet,
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        UnixListener, UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Debug)]
pub(super) struct SessionInfo {
    pub instance_id: String,
    pub pid: u32,
    pub ports: Ports,
}
struct Session {
    info: SessionInfo,
    id: String,
    last_frame: Instant,
    cancel: CancellationToken,
    commands: mpsc::Sender<Rotation>,
}
struct State {
    current: Option<Session>,
    expected: HashSet<String>,
}
struct Shared {
    state: Mutex<State>,
    changed: watch::Sender<Option<SessionInfo>>,
    admission: tokio::sync::Mutex<()>,
    lock: &'static crate::IdorisLock,
}
#[derive(Clone)]
pub(super) struct Handle(Arc<Shared>);
impl Handle {
    pub fn session(&self) -> Option<SessionInfo> {
        self.0.changed.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Option<SessionInfo>> {
        self.0.changed.subscribe()
    }
    pub fn take_expected_exit(&self, instance_id: &str) -> bool {
        self.0
            .state
            .lock()
            .is_ok_and(|mut s| s.expected.remove(instance_id))
    }
    pub async fn set_admin_token(
        &self,
        instance_id: &str,
        token: Zeroizing<String>,
    ) -> std::result::Result<(), SupervisorError> {
        let commands = self
            .0
            .state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .current
                    .as_ref()
                    .filter(|s| s.info.instance_id == instance_id)
                    .map(|s| s.commands.clone())
            })
            .ok_or(SupervisorError::Stopped)?;
        if decode_hex::<32>(&token).is_none() {
            return Err(SupervisorError::Configuration);
        }
        let (done, receive) = oneshot::channel();
        timeout(FRAME_TIMEOUT, async {
            commands
                .send(Rotation { token, done })
                .await
                .map_err(|_| SupervisorError::Stopped)?;
            receive.await.map_err(|_| SupervisorError::Stopped)
        })
        .await
        .map_err(|_| SupervisorError::StartupTimeout)?
    }
}
struct Rotation {
    token: Zeroizing<String>,
    done: oneshot::Sender<()>,
}

pub(super) struct Server {
    path: PathBuf,
    handle: Handle,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
impl Server {
    pub fn start(discovery_path: PathBuf, lock: &'static crate::IdorisLock) -> io::Result<Self> {
        Self::start_inner(discovery_path, lock, false)
    }
    fn start_inner(
        discovery_path: PathBuf,
        lock: &'static crate::IdorisLock,
        test_unsigned: bool,
    ) -> io::Result<Self> {
        let path = discovery_path
            .parent()
            .ok_or_else(|| io::Error::other("missing run directory"))?
            .join("idoris-register.sock");
        if path.as_os_str().as_encoded_bytes().len() > 103 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "registration socket path exceeds 103 bytes",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("missing run directory"))?;
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Tokio creates listener and accepted descriptors with CLOEXEC.
        let listener = UnixListener::bind(&path)?;
        let (changed, _) = watch::channel(None);
        let handle = Handle(Arc::new(Shared {
            state: Mutex::new(State {
                current: None,
                expected: HashSet::new(),
            }),
            changed,
            admission: tokio::sync::Mutex::new(()),
            lock,
        }));
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_handle = handle.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            let shared = task_handle.clone();
                            let path = discovery_path.clone();
                            let cancel = task_cancel.clone();
                            connections.spawn(async move {
                                tokio::select! {
                                    _ = cancel.cancelled() => {},
                                    _ = answer(stream, path, shared, test_unsigned) => {},
                                }
                            });
                        }
                        Err(_) => break,
                    },
                    _ = connections.join_next(), if !connections.is_empty() => {},
                }
            }
        });
        Ok(Self {
            path,
            handle,
            cancel,
            task,
        })
    }
    pub fn socket_path(&self) -> &Path {
        &self.path
    }
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
        if let Ok(mut state) = self.handle.0.state.lock() {
            if let Some(session) = state.current.take() {
                session.cancel.cancel();
            }
            self.handle.0.changed.send_replace(None);
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

struct Peer {
    token: Option<AuditToken>,
    pid: i32,
}
impl Peer {
    fn new(stream: &UnixStream) -> Result<Self> {
        let cred = stream.peer_cred().map_err(|_| "identity_rejected")?;
        if cred.uid() != nix::unistd::geteuid().as_raw() {
            return Err("identity_rejected");
        }
        let token = agent24_os_proc::peer_audit_token(stream).ok();
        #[cfg(target_os = "macos")]
        let pid = token.ok_or("identity_rejected")?.pid();
        #[cfg(not(target_os = "macos"))]
        let pid = cred.pid().ok_or("identity_rejected")?;
        if pid <= 0 || token.is_some_and(|t| t.euid() != cred.uid()) {
            return Err("identity_rejected");
        }
        Ok(Self { token, pid })
    }
    fn unchanged(&self, stream: &UnixStream) -> bool {
        if let Some(token) = self.token {
            agent24_os_proc::peer_audit_token(stream).is_ok_and(|now| now == token)
        } else {
            #[cfg(target_os = "macos")]
            {
                false
            }
            #[cfg(not(target_os = "macos"))]
            {
                stream.peer_cred().is_ok_and(|c| {
                    c.uid() == nix::unistd::geteuid().as_raw() && c.pid() == Some(self.pid)
                })
            }
        }
    }
    fn verify(&self, test_unsigned: bool) -> Result<()> {
        let dev = cfg!(debug_assertions)
            && std::env::var("AGENT24_DEV_ALLOW_UNSIGNED_IDORIS").is_ok_and(|v| v == "1");
        if dev || (cfg!(test) && test_unsigned) {
            return Ok(());
        }
        agent24_os_proc::verify_code_identity(
            self.token.ok_or("identity_rejected")?,
            "HYAP444FN9",
            &["ai.idoris.cli"],
        )
        .map(|_| ())
        .map_err(|_| "identity_rejected")
    }
}

struct Reader {
    read: BufReader<OwnedReadHalf>,
    bytes: Vec<u8>,
}
impl Reader {
    async fn frame(&mut self, peer: &Peer, deadline: Instant) -> Result<Option<Value>> {
        let result = timeout_at(deadline, self.read_frame())
            .await
            .map_err(|_| "timeout")
            .and_then(|v| v);
        if !matches!(result, Ok(None)) && !peer.unchanged(self.read.get_ref().as_ref()) {
            return Err("identity_rejected");
        }
        result
    }
    async fn read_frame(&mut self) -> Result<Option<Value>> {
        loop {
            let available = self.read.fill_buf().await.map_err(|_| "bad_frame")?;
            if available.is_empty() {
                return if self.bytes.is_empty() {
                    Ok(None)
                } else {
                    Err("bad_frame")
                };
            }
            let count = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |n| n + 1);
            if self.bytes.len() + count > 16384 {
                return Err("bad_frame");
            }
            let complete = available[count - 1] == b'\n';
            self.bytes.extend_from_slice(&available[..count]);
            self.read.consume(count);
            if complete {
                break;
            }
        }
        if self.bytes.contains(&b'\r') || self.bytes.windows(3).any(|b| b == b"\xef\xbb\xbf") {
            return Err("bad_frame");
        }
        let value: Value = serde_json::from_slice(&self.bytes).map_err(|_| "bad_frame")?;
        self.bytes.clear();
        if !value.is_object() {
            return Err("bad_frame");
        }
        Ok(Some(value))
    }
}
async fn send(write: &mut OwnedWriteHalf, value: &impl serde::Serialize) -> Result<()> {
    let mut bytes = Zeroizing::new(serde_json::to_vec(value).map_err(|_| "internal")?);
    bytes.push(b'\n');
    timeout(FRAME_TIMEOUT, write.write_all(&bytes))
        .await
        .map_err(|_| "timeout")?
        .map_err(|_| "internal")
}
async fn error(write: &mut OwnedWriteHalf, code: &str) {
    let _ = send(
        write,
        &json!({"v":1,"op":"error","code":code,"message":"Registration rejected"}),
    )
    .await;
}
#[derive(Deserialize)]
struct Register {
    instance_id: String,
    pid: u32,
    version: String,
    contract_version: String,
    data_port: u16,
    admin_port: u16,
    started_at_ms: u64,
    phase: String,
    resume: bool,
    proof: String,
}
impl Register {
    fn transcript(&self, conn: &str, nonce: &str) -> String {
        format!(
            "idoris-register/v1\n{conn}\n{nonce}\n{}\n{}\n{}\n{}\n{}",
            self.instance_id, self.pid, self.data_port, self.admin_port, self.started_at_ms
        )
    }
}
fn decode_hex<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() != N * 2
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0; N];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}
fn mac(key: &[u8], transcript: &str) -> Result<Hmac<Sha256>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| "internal")?;
    mac.update(transcript.as_bytes());
    Ok(mac)
}
fn proof(key: &[u8], transcript: &str) -> Result<String> {
    Ok(mac(key, transcript)?
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
async fn healthy(info: &SessionInfo) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    let check = async {
        let mut response = client
            .get(format!("http://127.0.0.1:{}/health", info.ports.data))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len() + chunk.len() > 16384 {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        (value["instance_id"].as_str() == Some(info.instance_id.as_str())).then_some(())
    };
    timeout(Duration::from_secs(2), check)
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn answer(stream: UnixStream, path: PathBuf, handle: Handle, test_unsigned: bool) {
    let peer = Peer::new(&stream);
    let (read, mut write) = stream.into_split();
    let peer = match peer {
        Ok(peer) => peer,
        Err(code) => {
            error(&mut write, code).await;
            return;
        }
    };
    let mut reader = Reader {
        read: BufReader::new(read),
        bytes: Vec::new(),
    };
    let result = handshake(
        &mut reader,
        &mut write,
        &peer,
        &path,
        &handle,
        test_unsigned,
    )
    .await;
    match result {
        Ok(Some((id, commands, cancel))) => {
            let result = session(
                &mut reader,
                &mut write,
                &peer,
                &handle,
                &id,
                commands,
                cancel,
            )
            .await;
            if let Err(code) = result {
                error(&mut write, code).await;
            }
            if let Ok(mut state) = handle.0.state.lock()
                && state.current.as_ref().is_some_and(|s| s.id == id)
            {
                state.current = None;
                handle.0.changed.send_replace(None);
            }
        }
        Ok(None) => {}
        Err(code) => error(&mut write, code).await,
    }
}
type Accepted = (String, mpsc::Receiver<Rotation>, CancellationToken);
async fn handshake(
    reader: &mut Reader,
    write: &mut OwnedWriteHalf,
    peer: &Peer,
    path: &Path,
    handle: &Handle,
    test_unsigned: bool,
) -> Result<Option<Accepted>> {
    let random = mint_token().map_err(|_| "internal")?;
    let conn = &random[..16];
    let nonce = &random[16..48];
    send(
        write,
        &json!({"v":1,"op":"challenge","conn_id":conn,"nonce":nonce}),
    )
    .await?;
    let Some(value) = reader.frame(peer, Instant::now() + FRAME_TIMEOUT).await? else {
        return Ok(None);
    };
    if value["op"] == "error" {
        return Ok(None);
    }
    timeout(FRAME_TIMEOUT, async {
        peer.verify(test_unsigned)?;
        if value["nonce"].as_str() != Some(nonce) {
            return Err("identity_rejected");
        }
        if value["v"].as_u64().ok_or("bad_frame")? != 1 {
            return Err("unsupported_version");
        }
        let contract = value["contract_version"]
            .as_str()
            .and_then(|v| semver::Version::parse(v).ok())
            .ok_or("contract_incompatible")?;
        // Same compatibility rule as /health: the lock's major and minimum minor.
        if handle.0.lock.check_contract(&contract.to_string()).is_err() {
            return Err("contract_incompatible");
        }
        if value["op"] != "register" {
            return Err("bad_frame");
        }
        let registration: Register = serde_json::from_value(value).map_err(|_| "bad_frame")?;
        if !uuid::Uuid::parse_str(&registration.instance_id)
            .is_ok_and(|id| id.get_version_num() == 4)
            || !matches!(
                registration.phase.as_str(),
                "starting" | "ready" | "degraded"
            )
        {
            return Err("bad_frame");
        }
        let _ = registration.resume;
        let record = discovery::read_any(path).map_err(|_| "proof_invalid")?;
        if registration.instance_id != record.instance_id
            || registration.pid != record.pid
            || registration.data_port != record.data_port
            || registration.admin_port != record.admin_port
            || registration.started_at_ms != record.started_at_ms
            || registration.version != record.version
            || registration.contract_version != record.contract_version
        {
            return Err("proof_invalid");
        }
        let key = Zeroizing::new(decode_hex::<32>(&record.instance_nonce).ok_or("proof_invalid")?);
        let provided = decode_hex::<32>(&registration.proof).ok_or("proof_invalid")?;
        mac(key.as_ref(), &registration.transcript(conn, nonce))?
            .verify_slice(&provided)
            .map_err(|_| "proof_invalid")?;
        if u32::try_from(peer.pid).ok() != Some(registration.pid) {
            return Err("identity_rejected");
        }
        match agent24_os_proc::owns_listening_ports(
            peer.pid,
            &[registration.data_port, registration.admin_port],
        ) {
            Ok(true) => {}
            Err(error)
                if error.kind() == io::ErrorKind::Unsupported
                    && (cfg!(test) || !cfg!(target_os = "macos")) => {}
            _ => return Err("identity_rejected"),
        }
        let info = SessionInfo {
            instance_id: registration.instance_id,
            pid: registration.pid,
            ports: Ports {
                data: registration.data_port,
                admin: registration.admin_port,
            },
        };
        if !healthy(&info).await {
            return Err("identity_rejected");
        }
        let _admission = handle.0.admission.lock().await;
        let prior = handle
            .0
            .state
            .lock()
            .map_err(|_| "internal")?
            .current
            .as_ref()
            .map(|s| (s.info.clone(), s.last_frame, s.id.clone()));
        if let Some((old, last_frame, _)) = &prior
            && old.instance_id != info.instance_id
            && last_frame.elapsed() < SESSION_TIMEOUT
            && healthy(old).await
        {
            return Err("instance_conflict");
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let ack_proof = proof(
            key.as_ref(),
            &format!("idoris-ack/v1\n{conn}\n{nonce}\n{id}"),
        )?;
        send(
            write,
            &json!({"v":1,"op":"ack","session_id":id,"proof":ack_proof,"heartbeat_ms":10000}),
        )
        .await?;
        let (commands, receive) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let mut state = handle.0.state.lock().map_err(|_| "internal")?;
        if let Some(old) = state.current.take() {
            old.cancel.cancel();
        }
        state.current = Some(Session {
            info: info.clone(),
            id: id.clone(),
            last_frame: Instant::now(),
            cancel: cancel.clone(),
            commands,
        });
        handle.0.changed.send_replace(Some(info));
        Ok(Some((id, receive, cancel)))
    })
    .await
    .map_err(|_| "timeout")?
}
async fn session(
    reader: &mut Reader,
    write: &mut OwnedWriteHalf,
    peer: &Peer,
    handle: &Handle,
    id: &str,
    mut commands: mpsc::Receiver<Rotation>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut deadline = Instant::now() + SESSION_TIMEOUT;
    let mut pending: Option<(oneshot::Sender<()>, Instant)> = None;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep_until(pending.as_ref().map_or(deadline, |p| p.1)), if pending.is_some() => return Err("timeout"),
            command = commands.recv(), if pending.is_none() => {
                let Some(rotation) = command else { return Ok(()); };
                #[derive(serde::Serialize)]
                struct SetToken<'a> { v: u8, op: &'a str, token: &'a str }
                send(write, &SetToken { v: 1, op: "set_admin_token", token: &rotation.token }).await?;
                pending = Some((rotation.done, Instant::now() + FRAME_TIMEOUT));
            }
            frame = reader.frame(peer, deadline) => {
                let Some(frame) = frame? else { return Ok(()); };
                deadline = Instant::now() + SESSION_TIMEOUT;
                if frame["v"].as_u64().ok_or("bad_frame")? != 1 { return Err("unsupported_version"); }
                if let Ok(mut state) = handle.0.state.lock()
                    && let Some(current) = state.current.as_mut().filter(|s| s.id == id) { current.last_frame = Instant::now(); }
                match frame["op"].as_str().ok_or("bad_frame")? {
                    "heartbeat" => {
                        let seq = frame["seq"].as_u64().ok_or("bad_frame")?;
                        let phase = frame["phase"].as_str().ok_or("bad_frame")?;
                        if !matches!(phase, "starting" | "ready" | "degraded") || !frame["ready"].is_boolean() { return Err("bad_frame"); }
                        send(write, &json!({"v":1,"op":"heartbeat_ack","seq":seq})).await?;
                    }
                    "admin_token_ack" => { if let Some((done, _)) = pending.take() { let _ = done.send(()); } }
                    "bye" => {
                        if frame["reason"] != "shutdown" { return Err("bad_frame"); }
                        if let Ok(mut state) = handle.0.state.lock()
                            && let Some(current) = state.current.as_ref().filter(|s| s.id == id) {
                            let instance = current.info.instance_id.clone();
                            state.expected.insert(instance);
                        }
                        return Ok(());
                    }
                    "error" => return Ok(()),
                    _ => {},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
