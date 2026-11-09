//! K-B3 #1715: the presence connection on the enrolment socket, end to end
//! over a real Unix socket. macOS only: every frame is checked against the
//! peer's audit token, which exists only there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::capabilities::CapabilityStore;
use crate::capabilities::first_party::{FirstPartyAuthority, body_digest};
use crate::presence_service::ServicePresence;
use async_trait::async_trait;
use std::sync::Mutex;

const GENERATION: &str = "g";
const DESKTOP: &str = "ai.auraai.agent24";

/// Real start times (the authority checks the peer is still the process it
/// enrolled), a fixed signing identity.
struct Verifier(&'static str);
impl IdentityVerifier for Verifier {
    fn start_time(&self, pid: i32) -> io::Result<ProcStart> {
        agent24_os_proc::start_time(pid).map(|s| ProcStart {
            sec: s.sec,
            usec: s.usec,
        })
    }
    fn verify(
        &self,
        _pid: i32,
        audit_token: Option<agent24_os_proc::AuditToken>,
    ) -> Result<VerifiedIdentity, EnrollErrorCode> {
        Ok(VerifiedIdentity {
            kind: IdentityKind::MacosSecCode,
            identifier: self.0.into(),
            team_id: TEAM_ID.into(),
            executable: "/Applications/Agent24.app/Contents/MacOS/Agent24".into(),
            audit_token,
        })
    }
}

/// The real first-party authority, remembering the last token id it issued
/// so the test can store an approval text for it as the HTTP layer would.
struct Recording {
    inner: Arc<FirstPartyAuthority>,
    last: Mutex<Option<String>>,
}
#[async_trait]
impl EnrollAuthority for Recording {
    fn issue(&self, peer: &EnrolledPeer) -> Result<IssuedToken, EnrollErrorCode> {
        let token = self.inner.issue(peer)?;
        *self.last.lock().unwrap() = Some(token.token_id.clone());
        Ok(token)
    }
    fn revoke(&self, peer: &EnrolledPeer, id: &str) {
        self.inner.revoke(peer, id);
    }
    fn renew(&self, peer: &EnrolledPeer, id: &str) -> Result<IssuedToken, EnrollErrorCode> {
        self.inner.renew(peer, id)
    }
    async fn elevate(
        &self,
        peer: &EnrolledPeer,
        id: &str,
        request: ElevateRequest,
        pending: tokio::sync::oneshot::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<ElevationGrant, EnrollErrorCode> {
        self.inner.elevate(peer, id, request, pending, cancel).await
    }
}

struct Daemon {
    _dir: tempfile::TempDir,
    endpoint: EnrollEndpoint,
    stop: CancellationToken,
    authority: Arc<Recording>,
    presence: Arc<ServicePresence>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Signs as the desktop app at `hello` (one verify, then the recheck), and
/// as someone else from then on: the peer exec'd after the handshake.
struct ExecAfterHello(std::sync::atomic::AtomicUsize);
impl IdentityVerifier for ExecAfterHello {
    fn start_time(&self, pid: i32) -> io::Result<ProcStart> {
        Verifier(DESKTOP).start_time(pid)
    }
    fn verify(
        &self,
        pid: i32,
        audit_token: Option<agent24_os_proc::AuditToken>,
    ) -> Result<VerifiedIdentity, EnrollErrorCode> {
        let call = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Verifier(if call < 2 { DESKTOP } else { "other" }).verify(pid, audit_token)
    }
}

/// A service-mode endpoint (`with_presence`) or a host-child one (without).
fn daemon(identifier: &'static str, with_presence: bool) -> Daemon {
    daemon_with(Arc::new(Verifier(identifier)), with_presence)
}

fn daemon_with(verifier: Arc<dyn IdentityVerifier>, with_presence: bool) -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let mut endpoint = bind(dir.path()).unwrap();
    let presence = Arc::new(ServicePresence::new());
    let inner = Arc::new(FirstPartyAuthority::new(CapabilityStore::new(GENERATION)));
    if with_presence {
        inner.set_provider(Some(presence.clone()));
        endpoint.set_presence(presence.clone());
    }
    let authority = Arc::new(Recording {
        inner,
        last: Mutex::new(None),
    });
    let stop = CancellationToken::new();
    endpoint.serve_with_identity(stop.clone(), GENERATION.into(), authority.clone(), verifier);
    Daemon {
        _dir: dir,
        endpoint,
        stop,
        authority,
        presence,
    }
}

type Client = BufReader<tokio::net::UnixStream>;

async fn line(client: &mut Client) -> serde_json::Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_line(&mut line))
        .await
        .expect("a frame within 5s")
        .unwrap();
    assert!(!line.is_empty(), "connection closed");
    serde_json::from_str(&line).unwrap()
}

/// True when the daemon closes the connection (EOF) within `within`.
async fn closed(client: &mut Client, within: Duration) -> bool {
    let mut line = String::new();
    matches!(
        tokio::time::timeout(within, client.read_line(&mut line)).await,
        Ok(Ok(0))
    )
}

async fn write(client: &mut Client, frame: serde_json::Value) {
    let mut bytes = serde_json::to_vec(&frame).unwrap();
    bytes.push(b'\n');
    client.get_mut().write_all(&bytes).await.unwrap();
}

/// Connect and say `hello`, with `role` when given; returns the first reply.
async fn open(daemon: &Daemon, role: Option<&str>) -> (Client, serde_json::Value) {
    let stream = tokio::net::UnixStream::connect(daemon.endpoint.path())
        .await
        .unwrap();
    let mut client = BufReader::new(stream);
    let challenge = line(&mut client).await;
    assert_eq!(challenge["op"], "challenge");
    let mut hello =
        serde_json::json!({"v":1,"op":"hello","generation":GENERATION,"nonce":challenge["nonce"]});
    if let Some(role) = role {
        hello["role"] = role.into();
    }
    write(&mut client, hello).await;
    let reply = line(&mut client).await;
    (client, reply)
}

async fn presence_connection(daemon: &Daemon) -> Client {
    let (client, ready) = open(daemon, Some("presence")).await;
    assert_eq!(ready["op"], "presence_ready", "{ready}");
    client
}

fn attached_body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"manifest": "name: agentear\nversion: '1'\nroute_namespace: /api/v1/agentear\nevent_module: agentear\ndata_dir: ~/.agent24/os/agentear/\nimpl_kind: attached_process\n"})).unwrap()
}

/// Enrol a token connection and ask to elevate a remembered request; returns
/// the connection after `elevation_pending` (or the error frame instead).
async fn elevate(daemon: &Daemon) -> (Client, serde_json::Value) {
    let (mut client, enrolled) = open(daemon, None).await;
    assert_eq!(enrolled["op"], "enrolled", "{enrolled}");
    let token_id = daemon.authority.last.lock().unwrap().clone().unwrap();
    daemon
        .authority
        .inner
        .remember_attached_request(&token_id, &attached_body(), &[]);
    write(
        &mut client,
        serde_json::json!({"v":1,"op":"elevate","method":"POST","path":"/api/v1/attached","body_sha256":body_digest(&attached_body()),"summary":"client words"}),
    )
    .await;
    let reply = line(&mut client).await;
    (client, reply)
}

async fn answer(
    presence: &mut Client,
    challenge: &serde_json::Value,
    decision: &str,
) -> serde_json::Value {
    write(
        presence,
        serde_json::json!({"v":1,"op":"presence_answer","challenge_id":challenge["challenge_id"],"nonce":challenge["nonce"],"decision":decision}),
    )
    .await;
    next_non_ping(presence).await
}

/// Next frame that is not a ping (answering it, so the line stays up).
async fn next_non_ping(client: &mut Client) -> serde_json::Value {
    loop {
        let frame = line(client).await;
        if frame["op"] != "presence_ping" {
            return frame;
        }
        write(
            client,
            serde_json::json!({"v":1,"op":"presence_pong","nonce":frame["nonce"]}),
        )
        .await;
    }
}

#[tokio::test]
async fn a_presence_hello_gets_ready_and_no_token() {
    let daemon = daemon(DESKTOP, true);
    let (_client, ready) = open(&daemon, Some("presence")).await;
    assert_eq!(ready["op"], "presence_ready");
    assert_eq!(ready["identity"]["kind"], "macos_seccode");
    assert_eq!(ready["identity"]["identifier"], DESKTOP);
    for absent in ["token", "http_port", "expires_in", "tier"] {
        assert!(ready.get(absent).is_none(), "{absent} in {ready}");
    }
    assert!(
        daemon.authority.last.lock().unwrap().is_none(),
        "no token issued"
    );
}

#[tokio::test]
async fn only_the_desktop_app_may_hold_the_presence_role() {
    for identifier in ["agent24", "agent24d"] {
        let daemon = daemon(identifier, true);
        let (mut client, reply) = open(&daemon, Some("presence")).await;
        assert_eq!(reply["op"], "error");
        assert_eq!(reply["code"], "identity_rejected", "{identifier}");
        assert!(closed(&mut client, Duration::from_secs(2)).await);
        // The same peer is still welcome as a token connection.
        let (_c, enrolled) = open(&daemon, None).await;
        assert_eq!(enrolled["op"], "enrolled");
    }
}

#[tokio::test]
async fn an_unknown_role_is_a_bad_frame() {
    let daemon = daemon(DESKTOP, true);
    for role in ["", "Presence", "token", "admin"] {
        let (_c, reply) = open(&daemon, Some(role)).await;
        assert_eq!(reply["code"], "bad_frame", "{role:?}");
    }
}

#[tokio::test]
async fn outside_service_mode_the_presence_role_is_refused() {
    let daemon = daemon(DESKTOP, false);
    let (_c, reply) = open(&daemon, Some("presence")).await;
    assert_eq!(reply["code"], "wrong_role");
}

#[tokio::test]
async fn a_second_presence_connection_conflicts_until_the_first_goes() {
    let daemon = daemon(DESKTOP, true);
    let first = presence_connection(&daemon).await;
    let (_c, reply) = open(&daemon, Some("presence")).await;
    assert_eq!(reply["code"], "presence_conflict");
    drop(first);
    // The first one's EOF frees the slot.
    let mut attempts = 0;
    loop {
        let (_c, reply) = open(&daemon, Some("presence")).await;
        if reply["op"] == "presence_ready" {
            break;
        }
        assert_eq!(reply["code"], "presence_conflict");
        attempts += 1;
        assert!(attempts < 20, "slot never freed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn each_role_refuses_the_other_roles_ops() {
    let daemon = daemon(DESKTOP, true);
    for op in [
        serde_json::json!({"v":1,"op":"renew"}),
        serde_json::json!({"v":1,"op":"elevate","method":"POST","path":"/api/v1/attached","body_sha256":"0".repeat(64)}),
        serde_json::json!({"v":1,"op":"hello","generation":GENERATION,"nonce":"n"}),
    ] {
        let mut client = presence_connection(&daemon).await;
        write(&mut client, op.clone()).await;
        let reply = next_non_ping(&mut client).await;
        assert_eq!(reply["code"], "wrong_role", "{op}");
        assert!(closed(&mut client, Duration::from_secs(2)).await);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for op in [
        serde_json::json!({"v":1,"op":"presence_answer","challenge_id":"c","nonce":"n","decision":"allow"}),
        serde_json::json!({"v":1,"op":"presence_pong","nonce":"n"}),
    ] {
        let (mut client, enrolled) = open(&daemon, None).await;
        assert_eq!(enrolled["op"], "enrolled");
        write(&mut client, op.clone()).await;
        assert_eq!(line(&mut client).await["code"], "wrong_role", "{op}");
    }
}

#[tokio::test]
async fn service_mode_elevation_is_confirmed_on_the_presence_connection() {
    for (decision, expected) in [("allow", "elevated"), ("deny", "presence_denied")] {
        let daemon = daemon(DESKTOP, true);
        let mut presence = presence_connection(&daemon).await;
        let (mut token, pending) = elevate(&daemon).await;
        assert_eq!(pending["op"], "elevation_pending", "{pending}");
        let challenge = next_non_ping(&mut presence).await;
        assert_eq!(challenge["op"], "presence_challenge");
        assert_eq!(challenge["challenge_id"], pending["challenge_id"]);
        assert_eq!(challenge["method"], "POST");
        assert_eq!(challenge["path"], "/api/v1/attached");
        assert_eq!(challenge["requester"]["identifier"], DESKTOP);
        assert_eq!(challenge["requester"]["pid"], std::process::id());
        assert_eq!(challenge["nonce"].as_str().unwrap().len(), 32);
        assert!(challenge["expires_at"].as_u64().unwrap() > crate::capabilities::unix_now());
        // The daemon's own words, never the client's.
        let summary = challenge["summary"].as_str().unwrap();
        assert!(summary.contains("register attached module"), "{summary}");
        assert!(!summary.contains("client words"));
        let answered = answer(&mut presence, &challenge, decision).await;
        assert_eq!(answered["op"], "presence_answered");
        assert_eq!(answered["accepted"], true);
        let result = line(&mut token).await;
        if expected == "elevated" {
            assert_eq!(result["op"], "elevated", "{result}");
            assert_eq!(result["challenge_id"], challenge["challenge_id"]);
            assert!(!result["grant"].as_str().unwrap().is_empty());
        } else {
            assert_eq!(result["code"], expected, "{result}");
        }
        // A second answer is late: refused, and the line stays up.
        let late = answer(&mut presence, &challenge, "allow").await;
        assert_eq!(late["accepted"], false);
        let unknown = answer(
            &mut presence,
            &serde_json::json!({"challenge_id":"nope","nonce":"n"}),
            "allow",
        )
        .await;
        assert_eq!(unknown["accepted"], false);
        assert!(!closed(&mut presence, Duration::from_millis(100)).await);
    }
}

#[tokio::test]
async fn a_wrong_nonce_does_not_answer_and_a_bad_answer_closes() {
    let daemon = daemon(DESKTOP, true);
    let mut presence = presence_connection(&daemon).await;
    let (mut token, _) = elevate(&daemon).await;
    let challenge = next_non_ping(&mut presence).await;
    let mut forged = challenge.clone();
    forged["nonce"] = "0".repeat(32).into();
    assert_eq!(
        answer(&mut presence, &forged, "allow").await["accepted"],
        false
    );
    // Still open: the real nonce settles it.
    assert_eq!(
        answer(&mut presence, &challenge, "deny").await["accepted"],
        true
    );
    assert_eq!(line(&mut token).await["code"], "presence_denied");
    // A decision outside allow|deny is a malformed frame.
    let (_t, _) = elevate(&daemon).await;
    let challenge = next_non_ping(&mut presence).await;
    write(
        &mut presence,
        serde_json::json!({"v":1,"op":"presence_answer","challenge_id":challenge["challenge_id"],"nonce":challenge["nonce"],"decision":"yes"}),
    )
    .await;
    assert_eq!(next_non_ping(&mut presence).await["code"], "bad_frame");
}

#[tokio::test]
async fn the_requester_going_away_cancels_the_shown_challenge() {
    let daemon = daemon(DESKTOP, true);
    let mut presence = presence_connection(&daemon).await;
    let (token, _) = elevate(&daemon).await;
    let challenge = next_non_ping(&mut presence).await;
    drop(token);
    let cancel = next_non_ping(&mut presence).await;
    assert_eq!(cancel["op"], "presence_cancel");
    assert_eq!(cancel["challenge_id"], challenge["challenge_id"]);
    assert_eq!(
        answer(&mut presence, &challenge, "allow").await["accepted"],
        false
    );
}

#[tokio::test]
async fn without_a_presence_connection_elevation_is_unavailable_at_once() {
    let daemon = daemon(DESKTOP, true);
    let (mut token, pending) = elevate(&daemon).await;
    assert_eq!(pending["op"], "elevation_pending");
    let result = tokio::time::timeout(Duration::from_secs(2), line(&mut token))
        .await
        .unwrap();
    assert_eq!(result["code"], "presence_unavailable");
}

#[tokio::test]
async fn the_presence_connection_closing_makes_the_open_challenge_unavailable() {
    let daemon = daemon(DESKTOP, true);
    let mut presence = presence_connection(&daemon).await;
    let (mut token, _) = elevate(&daemon).await;
    let _challenge = next_non_ping(&mut presence).await;
    drop(presence);
    assert_eq!(line(&mut token).await["code"], "presence_unavailable");
    assert!(daemon.presence.attach(|| true).is_ok(), "slot freed");
}

#[tokio::test]
async fn a_ping_kept_answered_holds_the_line_and_a_missed_pong_closes_it() {
    let daemon = daemon(DESKTOP, true);
    let mut presence = presence_connection(&daemon).await;
    for _ in 0..3 {
        let ping = line(&mut presence).await;
        assert_eq!(ping["op"], "presence_ping");
        assert_eq!(ping["nonce"].as_str().unwrap().len(), 32);
        write(
            &mut presence,
            serde_json::json!({"v":1,"op":"presence_pong","nonce":ping["nonce"]}),
        )
        .await;
    }
    // A pong with the wrong nonce ends it at once.
    let ping = line(&mut presence).await;
    assert_eq!(ping["op"], "presence_ping");
    write(
        &mut presence,
        serde_json::json!({"v":1,"op":"presence_pong","nonce":"0".repeat(32)}),
    )
    .await;
    assert!(closed(&mut presence, Duration::from_secs(2)).await);
    // No pong at all: closed after the deadline, and the slot is free again.
    let mut presence = presence_connection(&daemon).await;
    assert_eq!(line(&mut presence).await["op"], "presence_ping");
    assert!(closed(&mut presence, PONG_DEADLINE * 3).await);
    assert!(daemon.presence.attach(|| true).is_ok());
}

#[tokio::test]
async fn an_unsolicited_pong_closes_the_line() {
    let daemon = daemon(DESKTOP, true);
    let mut presence = presence_connection(&daemon).await;
    write(
        &mut presence,
        serde_json::json!({"v":1,"op":"presence_pong","nonce":"n"}),
    )
    .await;
    assert!(closed(&mut presence, Duration::from_secs(2)).await);
}

#[tokio::test]
async fn an_answer_from_a_peer_that_changed_since_hello_is_refused() {
    let daemon = daemon_with(
        Arc::new(ExecAfterHello(std::sync::atomic::AtomicUsize::new(0))),
        true,
    );
    let mut presence = presence_connection(&daemon).await;
    // The challenge itself needs a requester; put one in by hand, as the
    // token connection would (its own verifier is the same one here).
    let waiter = {
        let presence = daemon.presence.clone();
        tokio::spawn(async move {
            use crate::capabilities::presence::{
                PresenceProvider, PresenceRequest, RequesterIdentity,
            };
            presence
                .confirm(
                    PresenceRequest {
                        challenge_id: "c1".into(),
                        operation: crate::capabilities::Operation::ModuleAdmin,
                        method: "POST".into(),
                        path: "/api/v1/attached".into(),
                        summary: "s".into(),
                        requester: RequesterIdentity {
                            identifier: "agent24".into(),
                            pid: 1,
                        },
                        expires_at: crate::capabilities::unix_now() + 60,
                    },
                    CancellationToken::new(),
                )
                .await
        })
    };
    let challenge = next_non_ping(&mut presence).await;
    assert_eq!(challenge["challenge_id"], "c1");
    let reply = answer(&mut presence, &challenge, "allow").await;
    assert_eq!(reply["code"], "identity_rejected", "{reply}");
    assert_eq!(
        waiter.await.unwrap(),
        crate::capabilities::presence::PresenceOutcome::Unavailable,
        "never Allowed"
    );
}
