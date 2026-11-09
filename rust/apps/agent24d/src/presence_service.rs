//! K-B3 #1715 — the service-mode presence channel.
//!
//! In service mode the desktop is not the daemon's parent, so the HTTP
//! challenge queue of [`crate::presence_desktop`] (ProductHost only) has no
//! poller. Instead the desktop main process opens a second connection on the
//! enrolment socket with `hello{role:"presence"}` (frames in
//! `docs/agent/CORE-2-AGENTEAR-SLICE.md` §"在场连接"). [`ServicePresence`] is
//! the queue between [`PresenceProvider::confirm`] and that connection:
//!
//! - one live presence connection at a time ([`ServicePresence::attach`]);
//! - at most one challenge delivered and unanswered on it, the rest queued;
//! - a challenge that expires while queued is never delivered;
//! - a delivered challenge settled some other way is followed by a cancel;
//! - the connection going away settles every challenge as `Unavailable`;
//! - with no connection, `confirm` answers `Unavailable` at once.
//!
//! The connection side (frames, ping, identity recheck) lives in
//! `enroll_endpoint.rs`; this module holds no socket.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

use crate::capabilities::presence::{PresenceOutcome, PresenceProvider, PresenceRequest};
use crate::capabilities::unix_now;

/// Queue bound: a runaway requester cannot grow it without limit. Beyond it
/// `confirm` answers `Unavailable` rather than evicting anyone.
const MAX_PENDING: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Allow,
    Deny,
}

/// What the connection writes next.
#[derive(Clone, Debug)]
pub enum Event {
    /// Deliver this challenge; `nonce` must come back with the answer.
    Challenge { req: PresenceRequest, nonce: String },
    /// The delivered challenge was settled before it was answered.
    Cancel { challenge_id: String },
    /// A newer connection replaced this one (its peer was gone): close.
    Replaced,
}

/// `attach` refused: a live presence connection is already there.
#[derive(Debug, Eq, PartialEq)]
pub struct Conflict;

struct Entry {
    req: PresenceRequest,
    nonce: String,
    tx: Option<oneshot::Sender<PresenceOutcome>>,
}

struct Conn {
    id: u64,
    /// Is the peer of this connection still alive? Asked only when a second
    /// connection wants the slot, so a dead peer whose task has not yet seen
    /// EOF does not lock the desktop out.
    alive: Box<dyn Fn() -> bool + Send>,
    delivered: Option<String>,
    cancels: VecDeque<String>,
}

#[derive(Default)]
struct Inner {
    next_conn: u64,
    conn: Option<Conn>,
    queue: VecDeque<String>,
    entries: HashMap<String, Entry>,
}

impl Inner {
    /// Settle every challenge as `Unavailable` and forget the connection.
    fn drop_conn(&mut self) {
        self.conn = None;
        self.queue.clear();
        for (_, mut entry) in self.entries.drain() {
            if let Some(tx) = entry.tx.take() {
                let _ = tx.send(PresenceOutcome::Unavailable);
            }
        }
    }
}

#[derive(Default)]
pub struct ServicePresence {
    inner: Mutex<Inner>,
    wake: Notify,
}

impl ServicePresence {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock still guards consistent data: every critical
        // section below leaves `Inner` whole before it can panic.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Take the presence slot. `alive` reports whether this connection's peer
    /// still runs; a later `attach` replaces this one only when it says no.
    pub fn attach(
        self: &Arc<Self>,
        alive: impl Fn() -> bool + Send + 'static,
    ) -> Result<Attachment, Conflict> {
        let mut inner = self.lock();
        if let Some(conn) = &inner.conn {
            if (conn.alive)() {
                return Err(Conflict);
            }
            // Its peer is gone: everything it held is unavailable, and its
            // task, still waiting on the socket, gets `Replaced`.
            inner.drop_conn();
            self.wake.notify_waiters();
        }
        inner.next_conn += 1;
        let id = inner.next_conn;
        inner.conn = Some(Conn {
            id,
            alive: Box::new(alive),
            delivered: None,
            cancels: VecDeque::new(),
        });
        Ok(Attachment {
            presence: self.clone(),
            id,
        })
    }

    #[cfg(test)]
    fn queued(&self) -> usize {
        self.lock().entries.len()
    }
}

/// The connection's handle on the slot. Dropping it (the connection ended)
/// settles every challenge as `Unavailable`.
pub struct Attachment {
    presence: Arc<ServicePresence>,
    id: u64,
}

impl Attachment {
    /// The next thing to write. Cancel-safe: state changes only in the poll
    /// that returns.
    pub async fn next_event(&self) -> Event {
        loop {
            let notified = self.presence.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(event) = self.poll_event() {
                return event;
            }
            notified.await;
        }
    }

    fn poll_event(&self) -> Option<Event> {
        let mut inner = self.presence.lock();
        let Inner {
            conn,
            queue,
            entries,
            ..
        } = &mut *inner;
        let Some(conn) = conn.as_mut().filter(|c| c.id == self.id) else {
            return Some(Event::Replaced);
        };
        if let Some(challenge_id) = conn.cancels.pop_front() {
            return Some(Event::Cancel { challenge_id });
        }
        if conn.delivered.is_some() {
            return None;
        }
        let now = unix_now();
        while let Some(id) = queue.pop_front() {
            let Some(entry) = entries.get(&id) else {
                continue;
            };
            if entry.req.expires_at <= now {
                // Expired while queued: never delivered. `confirm` settles it.
                continue;
            }
            conn.delivered = Some(id);
            return Some(Event::Challenge {
                req: entry.req.clone(),
                nonce: entry.nonce.clone(),
            });
        }
        None
    }

    /// The desktop's answer. True only for the first answer to the challenge
    /// this connection has open, with that challenge's nonce.
    pub fn answer(&self, challenge_id: &str, nonce: &str, decision: Decision) -> bool {
        let mut inner = self.presence.lock();
        let Inner { conn, entries, .. } = &mut *inner;
        let Some(conn) = conn.as_mut().filter(|c| c.id == self.id) else {
            return false;
        };
        if conn.delivered.as_deref() != Some(challenge_id) {
            return false;
        }
        let Some(entry) = entries.get(challenge_id) else {
            return false;
        };
        if entry.nonce.len() != nonce.len()
            || !constant_time_eq(entry.nonce.as_bytes(), nonce.as_bytes())
        {
            return false;
        }
        conn.delivered = None;
        if let Some(mut entry) = entries.remove(challenge_id)
            && let Some(tx) = entry.tx.take()
        {
            let _ = tx.send(match decision {
                Decision::Allow => PresenceOutcome::Allowed,
                Decision::Deny => PresenceOutcome::Denied,
            });
        }
        drop(inner);
        self.presence.wake.notify_waiters();
        true
    }
}

/// Constant-time comparison of equal-length byte strings.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let mut inner = self.presence.lock();
        if inner.conn.as_ref().is_some_and(|c| c.id == self.id) {
            inner.drop_conn();
        }
        drop(inner);
        self.presence.wake.notify_waiters();
    }
}

/// Retires the entry when `confirm` ends any way other than an answer
/// (expired, cancelled, dropped by the elevate connection going away).
struct Pending<'a> {
    presence: &'a ServicePresence,
    id: &'a str,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        let mut inner = self.presence.lock();
        if inner.entries.remove(self.id).is_none() {
            return;
        }
        inner.queue.retain(|queued| queued != self.id);
        if let Some(conn) = inner.conn.as_mut()
            && conn.delivered.as_deref() == Some(self.id)
        {
            conn.delivered = None;
            conn.cancels.push_back(self.id.to_owned());
        }
        drop(inner);
        self.presence.wake.notify_waiters();
    }
}

fn random_nonce() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[async_trait]
impl PresenceProvider for ServicePresence {
    async fn confirm(&self, req: PresenceRequest, cancel: CancellationToken) -> PresenceOutcome {
        let id = req.challenge_id.clone();
        let expires_at = req.expires_at;
        let (tx, rx) = oneshot::channel();
        {
            let mut inner = self.lock();
            // No desktop connected: unavailable at once, never queued for a
            // desktop that may connect later (#1715 §7).
            if inner.conn.is_none()
                || inner.entries.len() >= MAX_PENDING
                || inner.entries.contains_key(&id)
            {
                return PresenceOutcome::Unavailable;
            }
            inner.entries.insert(
                id.clone(),
                Entry {
                    req,
                    nonce: random_nonce(),
                    tx: Some(tx),
                },
            );
            inner.queue.push_back(id.clone());
        }
        let _pending = Pending {
            presence: self,
            id: &id,
        };
        self.wake.notify_waiters();
        let remaining = Duration::from_secs(expires_at.saturating_sub(unix_now()));
        tokio::select! {
            outcome = rx => outcome.unwrap_or(PresenceOutcome::Unavailable),
            () = cancel.cancelled() => PresenceOutcome::Unavailable,
            () = tokio::time::sleep(remaining) => PresenceOutcome::TimedOut,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::capabilities::Operation;
    use crate::capabilities::presence::RequesterIdentity;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn request(id: &str, ttl: u64) -> PresenceRequest {
        PresenceRequest {
            challenge_id: id.into(),
            operation: Operation::KnowledgeHost,
            method: "PUT".into(),
            path: "/api/v1/host/knowledge/bindings/b1".into(),
            summary: format!("summary {id}"),
            requester: RequesterIdentity {
                identifier: "ai.auraai.agent24".into(),
                pid: 42,
            },
            expires_at: unix_now() + ttl,
        }
    }

    async fn challenge(attachment: &Attachment) -> (PresenceRequest, String) {
        match tokio::time::timeout(Duration::from_secs(2), attachment.next_event())
            .await
            .unwrap()
        {
            Event::Challenge { req, nonce } => (req, nonce),
            other => panic!("expected a challenge, got {other:?}"),
        }
    }

    async fn nothing_for_a_moment(attachment: &Attachment) {
        assert!(
            tokio::time::timeout(Duration::from_millis(100), attachment.next_event())
                .await
                .is_err(),
            "no event expected"
        );
    }

    fn spawn_confirm(
        presence: &Arc<ServicePresence>,
        id: &str,
        ttl: u64,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<PresenceOutcome> {
        let presence = presence.clone();
        let req = request(id, ttl);
        tokio::spawn(async move { presence.confirm(req, cancel).await })
    }

    #[tokio::test]
    async fn without_a_connection_confirm_is_unavailable_at_once() {
        let presence = Arc::new(ServicePresence::new());
        let outcome = tokio::time::timeout(
            Duration::from_millis(200),
            presence.confirm(request("c1", 60), CancellationToken::new()),
        )
        .await
        .unwrap();
        assert_eq!(outcome, PresenceOutcome::Unavailable);
        assert_eq!(presence.queued(), 0);
    }

    #[tokio::test]
    async fn an_answer_settles_the_challenge_and_only_the_first_counts() {
        for (decision, outcome) in [
            (Decision::Allow, PresenceOutcome::Allowed),
            (Decision::Deny, PresenceOutcome::Denied),
        ] {
            let presence = Arc::new(ServicePresence::new());
            let attachment = presence.attach(|| true).unwrap();
            let waiter = spawn_confirm(&presence, "c1", 60, CancellationToken::new());
            let (req, nonce) = challenge(&attachment).await;
            assert_eq!(req.challenge_id, "c1");
            assert_eq!(req.summary, "summary c1");
            assert_eq!(nonce.len(), 32);
            assert!(attachment.answer("c1", &nonce, decision));
            assert_eq!(waiter.await.unwrap(), outcome);
            assert!(!attachment.answer("c1", &nonce, Decision::Allow), "late");
        }
    }

    #[tokio::test]
    async fn a_wrong_nonce_or_unknown_id_is_not_accepted_and_the_challenge_stays_open() {
        let presence = Arc::new(ServicePresence::new());
        let attachment = presence.attach(|| true).unwrap();
        let waiter = spawn_confirm(&presence, "c1", 60, CancellationToken::new());
        let (_, nonce) = challenge(&attachment).await;
        let mut wrong = nonce.clone().into_bytes();
        wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
        let wrong = String::from_utf8(wrong).unwrap();
        assert!(!attachment.answer("c1", &wrong, Decision::Allow));
        assert!(!attachment.answer("c1", "", Decision::Allow));
        assert!(!attachment.answer("c2", &nonce, Decision::Allow));
        assert!(!waiter.is_finished());
        assert!(attachment.answer("c1", &nonce, Decision::Deny));
        assert_eq!(waiter.await.unwrap(), PresenceOutcome::Denied);
    }

    #[tokio::test]
    async fn one_challenge_at_a_time_the_next_after_the_answer() {
        let presence = Arc::new(ServicePresence::new());
        let attachment = presence.attach(|| true).unwrap();
        let first = spawn_confirm(&presence, "c1", 60, CancellationToken::new());
        let (_, nonce1) = challenge(&attachment).await;
        let second = spawn_confirm(&presence, "c2", 60, CancellationToken::new());
        nothing_for_a_moment(&attachment).await;
        // The queued one cannot be answered before it is delivered.
        assert!(!attachment.answer("c2", &nonce1, Decision::Allow));
        assert!(attachment.answer("c1", &nonce1, Decision::Allow));
        let (req, nonce2) = challenge(&attachment).await;
        assert_eq!(req.challenge_id, "c2");
        assert!(attachment.answer("c2", &nonce2, Decision::Deny));
        assert_eq!(first.await.unwrap(), PresenceOutcome::Allowed);
        assert_eq!(second.await.unwrap(), PresenceOutcome::Denied);
    }

    #[tokio::test]
    async fn a_delivered_challenge_settled_elsewhere_is_cancelled_and_then_refused() {
        let presence = Arc::new(ServicePresence::new());
        let attachment = presence.attach(|| true).unwrap();
        let cancel = CancellationToken::new();
        let waiter = spawn_confirm(&presence, "c1", 60, cancel.clone());
        let (_, nonce) = challenge(&attachment).await;
        cancel.cancel(); // the elevate connection went away
        assert_eq!(waiter.await.unwrap(), PresenceOutcome::Unavailable);
        assert!(matches!(
            attachment.next_event().await,
            Event::Cancel { challenge_id } if challenge_id == "c1"
        ));
        assert!(!attachment.answer("c1", &nonce, Decision::Allow));
        assert_eq!(presence.queued(), 0);
    }

    #[tokio::test]
    async fn a_delivered_challenge_that_expires_times_out_and_is_cancelled() {
        let presence = Arc::new(ServicePresence::new());
        let attachment = presence.attach(|| true).unwrap();
        // Two seconds: whole-second deadlines, so at least one remains to
        // deliver it before it lapses.
        let waiter = spawn_confirm(&presence, "c1", 2, CancellationToken::new());
        let (_, nonce) = challenge(&attachment).await;
        assert_eq!(waiter.await.unwrap(), PresenceOutcome::TimedOut);
        assert!(matches!(
            attachment.next_event().await,
            Event::Cancel { challenge_id } if challenge_id == "c1"
        ));
        assert!(!attachment.answer("c1", &nonce, Decision::Allow));
    }

    #[tokio::test]
    async fn a_queued_challenge_that_expires_is_never_delivered() {
        let presence = Arc::new(ServicePresence::new());
        let attachment = presence.attach(|| true).unwrap();
        let first = spawn_confirm(&presence, "c1", 60, CancellationToken::new());
        let (_, nonce1) = challenge(&attachment).await;
        let short = spawn_confirm(&presence, "short", 1, CancellationToken::new());
        assert_eq!(short.await.unwrap(), PresenceOutcome::TimedOut);
        let third = spawn_confirm(&presence, "c3", 60, CancellationToken::new());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(attachment.answer("c1", &nonce1, Decision::Allow));
        // Not `short`, and no cancel for it either: it was never shown.
        let (req, nonce3) = challenge(&attachment).await;
        assert_eq!(req.challenge_id, "c3");
        assert!(attachment.answer("c3", &nonce3, Decision::Allow));
        assert_eq!(first.await.unwrap(), PresenceOutcome::Allowed);
        assert_eq!(third.await.unwrap(), PresenceOutcome::Allowed);
        nothing_for_a_moment(&attachment).await;
    }

    #[tokio::test]
    async fn the_connection_going_away_settles_delivered_and_queued_as_unavailable() {
        let presence = Arc::new(ServicePresence::new());
        let attachment = presence.attach(|| true).unwrap();
        let first = spawn_confirm(&presence, "c1", 60, CancellationToken::new());
        let _ = challenge(&attachment).await;
        let second = spawn_confirm(&presence, "c2", 60, CancellationToken::new());
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(attachment);
        assert_eq!(first.await.unwrap(), PresenceOutcome::Unavailable);
        assert_eq!(second.await.unwrap(), PresenceOutcome::Unavailable);
        assert_eq!(presence.queued(), 0);
        // The slot is free again; nothing pending carries over.
        let again = presence.attach(|| true).unwrap();
        nothing_for_a_moment(&again).await;
    }

    #[tokio::test]
    async fn a_second_connection_is_refused_while_the_first_peer_lives() {
        let presence = Arc::new(ServicePresence::new());
        let alive = Arc::new(AtomicBool::new(true));
        let flag = alive.clone();
        let first = presence
            .attach(move || flag.load(Ordering::SeqCst))
            .unwrap();
        assert_eq!(presence.attach(|| true).err(), Some(Conflict));
        let waiter = spawn_confirm(&presence, "c1", 60, CancellationToken::new());
        let (_, nonce) = challenge(&first).await;
        // The first peer dies before its task sees EOF: the newcomer takes
        // over, the old challenge is unavailable, the old task is told.
        alive.store(false, Ordering::SeqCst);
        let second = presence.attach(|| true).unwrap();
        assert_eq!(waiter.await.unwrap(), PresenceOutcome::Unavailable);
        assert!(matches!(first.next_event().await, Event::Replaced));
        assert!(!first.answer("c1", &nonce, Decision::Allow));
        // The old handle going away later does not free the new slot.
        drop(first);
        assert_eq!(presence.attach(|| true).err(), Some(Conflict));
        let waiter = spawn_confirm(&presence, "c2", 60, CancellationToken::new());
        let (req, nonce) = challenge(&second).await;
        assert_eq!(req.challenge_id, "c2");
        assert!(second.answer("c2", &nonce, Decision::Allow));
        assert_eq!(waiter.await.unwrap(), PresenceOutcome::Allowed);
    }

    #[tokio::test]
    async fn the_queue_is_bounded() {
        let presence = Arc::new(ServicePresence::new());
        let _attachment = presence.attach(|| true).unwrap();
        let mut waiters = Vec::new();
        for i in 0..MAX_PENDING {
            waiters.push(spawn_confirm(
                &presence,
                &format!("c{i}"),
                60,
                CancellationToken::new(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(presence.queued(), MAX_PENDING);
        let over = presence
            .confirm(request("over", 60), CancellationToken::new())
            .await;
        assert_eq!(over, PresenceOutcome::Unavailable);
        drop(_attachment);
        for waiter in waiters {
            assert_eq!(waiter.await.unwrap(), PresenceOutcome::Unavailable);
        }
    }
}
