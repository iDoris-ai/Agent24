use agent24_models::Msg;
use agent24_protocol::EventBody;

/// M1-T14: what [`crate::RunManager::chat_memory_prelude`] found out about THIS
/// prompt — independent of the messages it returns to prepend. The caller
/// (`/api/v1/chat`) needs this to build a deterministic `memory_receipt`
/// after the turn is committed, since the model's own reply text must never
/// be the source of that signal (it has been observed claiming success on a
/// turn the server actually skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplicitRememberState {
    /// The prompt is an explicit remember, and personal memory is currently
    /// paused — the write will be skipped.
    Paused,
    /// The prompt is an explicit remember, and personal memory is active —
    /// the write is expected to land (success/failure determined by the
    /// actual write that follows).
    Active,
}

/// fix683 (PR #683 review): the LIVE result of [`crate::RunManager::remember_exchange`]
/// / [`crate::RunManager::chat_remember_turn`] — a tri-state (four-state, counting
/// failure) replacing the old bare `bool`, which could only say
/// "write_gate didn't error", not "paused" vs "committed". `/api/v1/chat`'s
/// `memory_receipt` must be built from THIS, never from the pre-model-call
/// [`ExplicitRememberState`] snapshot — personal memory can be paused or
/// un-paused while the model is still generating, after that snapshot was
/// taken but before this turn's write actually runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryWriteOutcome {
    /// The turn's explicit-remember assertion is durably persisted.
    Saved,
    /// Personal memory was paused at the moment of commit; nothing was
    /// persisted for this turn's explicit remember.
    SkippedPaused,
    /// The session append (the conversation turn) reported an error; NOTHING of
    /// this turn was written. Carries the typed cause, never its text.
    Failed(MemoryFailure),
    /// P1-02c: the conversation turn WAS saved, but the explicit-remember
    /// assertion was not. Distinct from [`Self::Failed`] so a receipt never
    /// says "nothing saved" about a turn that is in the history, nor "saved"
    /// about an assertion that is not.
    Partial(MemoryFailure),
    /// There was nothing to write: no configured memory, no session, the
    /// prompt was not an explicit remember, or the origin was untrusted.
    NotApplicable,
}

/// P1-02c: the typed cause of a failed memory write — a closed code and, for a
/// quota code, its dimension. Carries no owner key and no storage text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryFailure {
    pub code: agent24_memory::MemoryErrorCode,
}

impl MemoryFailure {
    pub fn from_error(err: &agent24_memory::MemoryError) -> Self {
        Self { code: err.code() }
    }

    /// The wire form of the code. Exhaustive on purpose: a new
    /// `MemoryErrorCode` is a compile error here until the protocol knows it.
    pub fn wire_code(self) -> agent24_protocol::MemoryFailureCode {
        use agent24_memory::MemoryErrorCode as M;
        use agent24_protocol::MemoryFailureCode as W;
        match self.code {
            M::QuotaRows => W::QuotaRows,
            M::QuotaBytes => W::QuotaBytes,
            M::DiskLow => W::DiskLow,
            M::DiskFull => W::DiskFull,
            M::ReservedKind => W::ReservedKind,
            M::RetractIdTooLong => W::RetractIdTooLong,
            M::Internal => W::Internal,
        }
    }

    pub(crate) fn wire_code_str(self) -> &'static str {
        self.code.as_str()
    }

    pub(crate) fn event(
        self,
        session_id: &str,
        kind: Option<&str>,
        partial: bool,
    ) -> agent24_protocol::MemoryWriteFailedPayload {
        agent24_protocol::MemoryWriteFailedPayload {
            session_id: session_id.to_owned(),
            // Sanitized transitional `reason`: the code string, nothing else.
            reason: Some(self.code.as_str().to_owned()),
            code: Some(self.wire_code()),
            dimension: self.code.dimension().map(str::to_owned),
            owner_kind: Some(agent24_protocol::MemoryOwnerKind::Personal),
            kind: kind.map(str::to_owned),
            partial,
        }
    }
}

/// What loading a session's prior context came to (see
/// [`crate::RunManager::session_context`]).
pub(crate) enum SessionContext {
    Ready(Vec<Msg>),
    Cancelled,
    ImportIncomplete(MemoryFailure),
}

#[cfg(test)]
impl SessionContext {
    /// Test convenience: the loaded messages, panicking on any other outcome.
    pub(crate) fn unwrap(self) -> Vec<Msg> {
        match self {
            Self::Ready(context) => context,
            Self::Cancelled => panic!("session context load was cancelled"),
            Self::ImportIncomplete(f) => panic!("session import incomplete: {}", f.code),
        }
    }
}

impl MemoryWriteOutcome {
    /// P1-02c: `false` iff a memory write this run attempted failed (wholly or
    /// partly). Paused and not-applicable are not failures.
    pub fn persisted(self) -> bool {
        !matches!(self, Self::Failed(_) | Self::Partial(_))
    }
}

/// Where lifecycle events go (the daemon adapts this onto its WS hub).
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, body: EventBody);
}

/// Budget for preparing and compacting the post-completion memory write.
/// Ordering demands it happen before `run.completed`; append transaction
/// confirmation can exceed this budget because its outcome must be observed.
pub(crate) const MEMORY_WRITE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a parked approval remains resumable (H3). A restored approval older
/// than this is too stale to safely continue — the world it was queued against
/// has likely moved on — so `assess_restore` aborts it. Generous: an overnight
/// scheduled run must still be answerable the next morning.
pub const RESUME_TTL: std::time::Duration = std::time::Duration::from_secs(72 * 3600);
