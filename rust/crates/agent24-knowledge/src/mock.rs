//! Scripted [`KnowledgeTransport`]: one FIFO reply queue per operation.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use crate::transport::{KnowledgeOperation, KnowledgeTransport, TransportError, TransportResponse};

/// One received request: the operation, the body bytes and the deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// Operation the caller named.
    pub op: KnowledgeOperation,
    /// Body bytes the caller passed.
    pub body: Vec<u8>,
    /// Deadline the caller passed.
    pub deadline: Duration,
}

#[derive(Default)]
struct State {
    queues: HashMap<KnowledgeOperation, VecDeque<Result<TransportResponse, TransportError>>>,
    requests: Vec<RecordedRequest>,
}

/// In-memory transport: each operation has its own FIFO queue of scripted replies.
#[derive(Default)]
pub struct MockTransport {
    inner: Mutex<State>,
}

impl MockTransport {
    /// Empty mock: every operation's queue starts empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `reply` to the end of `op`'s queue.
    pub fn push(&self, op: KnowledgeOperation, reply: Result<TransportResponse, TransportError>) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.queues.entry(op).or_default().push_back(reply);
    }

    /// Every request received so far, in call order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.requests.clone()
    }
}

#[async_trait]
impl KnowledgeTransport for MockTransport {
    async fn send(
        &self,
        op: KnowledgeOperation,
        body: Vec<u8>,
        deadline: Duration,
    ) -> Result<TransportResponse, TransportError> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.requests.push(RecordedRequest {
            op,
            body: body.clone(),
            deadline,
        });
        if !op.has_body() && !body.is_empty() {
            return Err(TransportError::Other("mock: body on a request without one"));
        }
        match inner.queues.get_mut(&op).and_then(VecDeque::pop_front) {
            Some(reply) => reply,
            None => Err(TransportError::Other("mock: no scripted reply")),
        }
    }
}
