#![allow(clippy::unwrap_used)]
use super::*;
use std::os::unix::fs::PermissionsExt;
use tokio::net::TcpListener;
const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
#[test]
fn frozen_hmac_vectors() {
    let register = Register {
        instance_id: "3f0c6a52-6a0e-4c0e-9a6e-0d2f1c7a9b11".into(),
        pid: 41234,
        version: "0.6.0".into(),
        contract_version: "1.3.0".into(),
        data_port: 51234,
        admin_port: 51235,
        started_at_ms: 1763625330123,
        phase: "ready".into(),
        resume: false,
        proof: String::new(),
    };
    let key = decode_hex::<32>(KEY).unwrap();
    let transcript = register.transcript("0123456789abcdef", "00112233445566778899aabbccddeeff");
    assert_eq!(transcript.len(), 137);
    assert_eq!(
        proof(&key, &transcript).unwrap(),
        "386409d010127b68de12aab15ca786c9abac147add07e228191e56dbb05d7884"
    );
    let ack = "idoris-ack/v1\n0123456789abcdef\n00112233445566778899aabbccddeeff\na1b2c3d4e5f60718293a4b5c6d7e8f90";
    assert_eq!(ack.len(), 96);
    assert_eq!(
        proof(&key, ack).unwrap(),
        "099e173f6990315a62de5c9426ecbf0ed395a58e47728c5453e3e5a483cc0967"
    );
}
struct Fixture {
    _directory: tempfile::TempDir,
    server: Server,
    path: PathBuf,
    health: JoinHandle<()>,
    data: u16,
    admin: TcpListener,
    instance: String,
    pid: u32,
}
impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("ir-")
            .tempdir_in("/tmp")
            .unwrap();
        let path = directory.path().join("idoris.json");
        let server =
            Server::start_inner(path.clone(), crate::IdorisLock::embedded(), true).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let data = listener.local_addr().unwrap().port();
        let admin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let instance = uuid::Uuid::new_v4().to_string();
        let response_id = instance.clone();
        let health = tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/health",
                axum::routing::get(move || {
                    let id = response_id.clone();
                    async move { axum::Json(json!({"instance_id":id})) }
                }),
            );
            axum::serve(listener, app).await.unwrap();
        });
        let fixture = Self {
            _directory: directory,
            server,
            path,
            health,
            data,
            admin,
            instance,
            pid: std::process::id(),
        };
        fixture.write_discovery();
        fixture
    }
    fn write_discovery(&self) {
        std::fs::write(&self.path,json!({"schema":1,"service":"idoris","pid":self.pid,"instance_nonce":KEY,"instance_id":self.instance,"data_port":self.data,"admin_port":self.admin.local_addr().unwrap().port(),"started_at_ms":1763625330123u64,"version":"0.6.0","contract_version":"1.3.0"}).to_string()).unwrap();
        std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    async fn connect(&self, path: &Path) -> (Client, Value) {
        let stream = UnixStream::connect(path).await.unwrap();
        let (read, write) = stream.into_split();
        let mut client = Client {
            reader: BufReader::new(read),
            write,
        };
        let challenge = client.line().await;
        assert_eq!(challenge["op"], "challenge");
        let mut frame = json!({"v":1,"op":"register","nonce":challenge["nonce"],"instance_id":self.instance,"pid":self.pid,"version":"0.6.0","contract_version":"1.3.0","data_port":self.data,"admin_port":self.admin.local_addr().unwrap().port(),"started_at_ms":1763625330123u64,"phase":"ready","resume":false,"proof":""});
        let registration: Register = serde_json::from_value(frame.clone()).unwrap();
        frame["proof"] = proof(
            &decode_hex::<32>(KEY).unwrap(),
            &registration.transcript(
                challenge["conn_id"].as_str().unwrap(),
                challenge["nonce"].as_str().unwrap(),
            ),
        )
        .unwrap()
        .into();
        (client, frame)
    }
    async fn registered(&self) -> Client {
        let (mut client, frame) = self.connect(self.server.socket_path()).await;
        client.send(&frame).await;
        let ack = client.line().await;
        assert_eq!(ack["op"], "ack", "{ack}");
        assert_eq!(ack["heartbeat_ms"], 10000);
        client
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.health.abort();
    }
}
struct Client {
    reader: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}
impl Client {
    async fn line(&mut self) -> Value {
        let mut bytes = Vec::new();
        timeout(
            Duration::from_secs(5),
            self.reader.read_until(b'\n', &mut bytes),
        )
        .await
        .unwrap()
        .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn send(&mut self, frame: &Value) {
        send(&mut self.write, frame).await.unwrap();
    }
    async fn heartbeat(&mut self, seq: u64) {
        self.send(&json!({"v":1,"op":"heartbeat","seq":seq,"phase":"ready","ready":true}))
            .await;
        assert_eq!(
            self.line().await,
            json!({"v":1,"op":"heartbeat_ack","seq":seq})
        );
    }
}
#[tokio::test]
async fn register_heartbeat_rotation_and_bye() {
    let fixture = Fixture::new().await;
    let mut client = fixture.registered().await;
    client.heartbeat(1).await;
    client.heartbeat(u64::MAX).await;
    let handle = fixture.server.handle();
    let instance = fixture.instance.clone();
    let token = "a".repeat(64);
    let expected = token.clone();
    let rotate = tokio::spawn(async move {
        handle
            .set_admin_token(&instance, Zeroizing::new(token))
            .await
    });
    assert_eq!(
        client.line().await,
        json!({"v":1,"op":"set_admin_token","token":expected})
    );
    client.send(&json!({"v":1,"op":"admin_token_ack"})).await;
    rotate.await.unwrap().unwrap();
    client
        .send(&json!({"v":1,"op":"bye","reason":"shutdown"}))
        .await;
    let mut changed = fixture.server.handle().subscribe();
    timeout(Duration::from_secs(2), async {
        while changed.borrow_and_update().is_some() {
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let bye = fixture
        .server
        .handle()
        .take_expected_exit(&fixture.instance);
    // This is the supervisor's gate before incrementing crashes or restart policy.
    assert!(super::super::expected_exit(None, bye));
    assert!(!super::super::expected_exit(None, false));
    assert!(!super::super::expected_exit(Some(5), bye));
    assert!(
        !fixture
            .server
            .handle()
            .take_expected_exit(&fixture.instance)
    );
}
#[tokio::test]
async fn invalid_proof_nonce_contract_and_version() {
    let fixture = Fixture::new().await;
    for (field, value, code) in [
        ("proof", json!("0".repeat(64)), "proof_invalid"),
        ("nonce", json!("wrong"), "identity_rejected"),
        ("nonce", Value::Null, "identity_rejected"),
        ("contract_version", json!("1.0.0"), "contract_incompatible"),
        ("contract_version", json!("2.3.0"), "contract_incompatible"),
        ("v", json!(2), "unsupported_version"),
        ("pid", json!(1), "proof_invalid"),
    ] {
        let (mut client, mut frame) = fixture.connect(fixture.server.socket_path()).await;
        frame[field] = value;
        client.send(&frame).await;
        assert_eq!(client.line().await["code"], code, "{field}");
        assert!(fixture.server.handle().session().is_none());
    }
}
#[tokio::test]
async fn healthy_conflict_stale_replacement_and_resume() {
    let fixture = Fixture::new().await;
    let mut first = fixture.registered().await;
    let handle = fixture.server.handle();
    let original = handle
        .0
        .state
        .lock()
        .unwrap()
        .current
        .as_ref()
        .unwrap()
        .id
        .clone();
    let mut resume = fixture.registered().await;
    assert_ne!(
        original,
        handle.0.state.lock().unwrap().current.as_ref().unwrap().id
    );
    let mut eof = String::new();
    assert_eq!(
        timeout(Duration::from_secs(2), first.reader.read_line(&mut eof))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    resume.heartbeat(1).await;
    let second = Fixture::new().await;
    std::fs::copy(&second.path, &fixture.path).unwrap();
    let (mut contender, frame) = second.connect(fixture.server.socket_path()).await;
    contender.send(&frame).await;
    assert_eq!(contender.line().await["code"], "instance_conflict");
    assert_eq!(handle.session().unwrap().instance_id, fixture.instance);
    handle
        .0
        .state
        .lock()
        .unwrap()
        .current
        .as_mut()
        .unwrap()
        .last_frame = Instant::now() - SESSION_TIMEOUT;
    let (mut contender, frame) = second.connect(fixture.server.socket_path()).await;
    contender.send(&frame).await;
    assert_eq!(contender.line().await["op"], "ack");
    assert_eq!(handle.session().unwrap().instance_id, second.instance);
    contender.heartbeat(1).await;
}
#[tokio::test]
async fn bad_frames_and_client_error() {
    let fixture = Fixture::new().await;
    for bytes in [
        b"\n".to_vec(),
        b"{}\r\n".to_vec(),
        b"\xef\xbb\xbf{}\n".to_vec(),
        b"[]\n".to_vec(),
        b"\xff\n".to_vec(),
        vec![b'a'; 16385],
    ] {
        let (mut client, _) = fixture.connect(fixture.server.socket_path()).await;
        client.write.write_all(&bytes).await.unwrap();
        assert_eq!(client.line().await["code"], "bad_frame");
    }
    let mut client = fixture.registered().await;
    client
        .send(&json!({"v":1,"op":"error","code":"internal","message":"failed"}))
        .await;
    let mut line = String::new();
    assert_eq!(
        timeout(Duration::from_secs(2), client.reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(fixture.server.handle().session().is_none());
}
#[tokio::test]
async fn session_times_out_without_frames() {
    let fixture = Fixture::new().await;
    let _client = fixture.registered().await;
    let mut changed = fixture.server.handle().subscribe();
    tokio::time::pause();
    tokio::time::advance(SESSION_TIMEOUT + Duration::from_secs(1)).await;
    changed.changed().await.unwrap();
    assert!(changed.borrow().is_none());
}
#[test]
fn socket_path_limit() {
    assert_eq!(
        Server::start(
            PathBuf::from(format!("/{}/idoris.json", "a".repeat(110))),
            crate::IdorisLock::embedded()
        )
        .err()
        .unwrap()
        .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[tokio::test]
async fn ic3b_register_rejects_peer_pid_mismatch() {
    let mut fixture = Fixture::new().await;
    fixture.pid += 1;
    fixture.write_discovery();
    let (mut client, frame) = fixture.connect(fixture.server.socket_path()).await;
    client.send(&frame).await;
    assert_eq!(client.line().await["code"], "identity_rejected");
    assert!(fixture.server.handle().session().is_none());
}

#[tokio::test]
async fn ic3b_register_rejects_health_instance_mismatch() {
    let mut fixture = Fixture::new().await;
    fixture.instance = uuid::Uuid::new_v4().to_string();
    fixture.write_discovery();
    let (mut client, frame) = fixture.connect(fixture.server.socket_path()).await;
    client.send(&frame).await;
    assert_eq!(client.line().await["code"], "identity_rejected");
    assert!(fixture.server.handle().session().is_none());
}

#[tokio::test]
async fn ic3b_register_rejects_unowned_ports() {
    let fixture = Fixture::new().await;
    *fixture.server.handle.0.port_ownership.lock().unwrap() = Some(false);
    let (mut client, frame) = fixture.connect(fixture.server.socket_path()).await;
    client.send(&frame).await;
    assert_eq!(client.line().await["code"], "identity_rejected");
    assert!(fixture.server.handle().session().is_none());
}

#[tokio::test]
async fn ic3b_reconnect_clears_expected_exit() {
    let fixture = Fixture::new().await;
    let mut client = fixture.registered().await;
    let handle = fixture.server.handle();
    let mut changed = handle.subscribe();
    client
        .send(&json!({"v":1,"op":"bye","reason":"shutdown"}))
        .await;
    timeout(Duration::from_secs(2), async {
        while changed.borrow_and_update().is_some() {
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        handle
            .0
            .state
            .lock()
            .unwrap()
            .expected
            .contains(&fixture.instance)
    );
    let client = fixture.registered().await;
    drop(client);
    let bye = handle.take_expected_exit(&fixture.instance);
    assert!(!super::super::expected_exit(Some(1), bye));
}
