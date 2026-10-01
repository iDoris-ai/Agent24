#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{collections::HashMap, sync::Arc};

use agent24_memory::{
    KvStore, MemoryError,
    event::{EventQuery, EventStore, Origin, Trust},
    session::{CanonicalSession, CompactionPolicy, Summarizer},
    session_log::SessionLog,
};
use agent24_models::Msg;
use tokio::sync::Mutex;
#[cfg(test)]
use tokio::sync::Notify;

const RECENT_HARD_CEILING_FACTOR: usize = 4;
const SCAN_PAGE: i64 = 500;

/// Append-only session memory. Callers inject the personal partition key with
/// `with_owner`; the daemon catalogue wiring belongs to M1-T05. Legacy blobs
/// are read only for first import and never updated: downgrades are unsupported.
pub struct SessionMemory {
    kv: KvStore,
    log: SessionLog,
    owner: String,
    summarizer: Arc<dyn Summarizer>,
    policy: CompactionPolicy,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    #[cfg(test)]
    commit_probe: Option<Arc<CommitAcknowledgementProbe>>,
}

/// Test-only pause between a successful append and its acknowledgement to remember.
#[cfg(test)]
pub(crate) struct CommitAcknowledgementProbe {
    pub(crate) armed: AtomicBool,
    pub(crate) committed: Notify,
    pub(crate) deadline_elapsed: Notify,
    pub(crate) release: Notify,
}

impl SessionMemory {
    /// Creates memory without an owner. Reads and writes fail until `with_owner` is applied.
    pub fn new(kv: KvStore, summarizer: Arc<dyn Summarizer>) -> Self {
        let log = kv.session_log();
        Self {
            kv,
            log,
            owner: String::new(),
            summarizer,
            policy: CompactionPolicy::default(),
            locks: Mutex::new(HashMap::new()),
            #[cfg(test)]
            commit_probe: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_commit_probe(mut self, probe: Arc<CommitAcknowledgementProbe>) -> Self {
        self.commit_probe = Some(probe);
        self
    }

    #[must_use]
    pub fn with_owner(mut self, owner: String) -> Self {
        self.owner = owner;
        self
    }

    #[must_use]
    pub fn with_policy(mut self, policy: CompactionPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub(crate) async fn session_lock(&self, sid: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        if locks.len() > 1024 {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        Arc::clone(locks.entry(sid.to_owned()).or_default())
    }

    fn check_owner(&self) -> agent24_memory::Result<()> {
        if self.owner.trim().is_empty() {
            return Err(MemoryError::Io(
                "personal session owner is not configured".into(),
            ));
        }
        Ok(())
    }

    async fn ensure_imported(&self, sid: &str) -> agent24_memory::Result<()> {
        let events = self
            .kv
            .events()
            .scan(&EventQuery::owner(&self.owner).session(sid).limit(1))
            .await?;
        if !events.is_empty() {
            return Ok(());
        }
        // Recover only what the legacy blob still contains at first import;
        // already-compacted history cannot be reconstructed, and downgrade is unsupported.
        if let Some(legacy) = CanonicalSession::load(&self.kv, sid).await? {
            if legacy.session_id != sid {
                return Err(MemoryError::Io(format!(
                    "legacy session id {} does not match requested session {sid}",
                    legacy.session_id
                )));
            }
            self.log.import_legacy(&self.owner, &legacy).await?;
        }
        Ok(())
    }

    pub(crate) async fn context(&self, sid: &str) -> agent24_memory::Result<Vec<Msg>> {
        self.check_owner()?;
        let lock = self.session_lock(sid).await;
        let _guard = lock.lock().await;
        self.ensure_imported(sid).await?;
        let view = self.log.load_view(&self.owner, sid).await?;
        let cap = self
            .policy
            .max_recent
            .max(1)
            .saturating_mul(RECENT_HARD_CEILING_FACTOR);
        let mut start = view.tail.len().saturating_sub(cap);
        while start > 0 && start < view.tail.len() && view.tail[start].1.role == "tool" {
            start += 1;
        }
        if start > 0 {
            tracing::error!(
                "session {sid} context tail exceeded hard ceiling {cap}; omitting {start} oldest view messages; event log remains intact"
            );
        }
        let mut messages = Vec::with_capacity(view.tail.len().saturating_sub(start) + 1);
        if let Some(summary) = view.summary {
            messages.push(Msg::system(format!(
                "Summary of earlier conversation:\n{summary}"
            )));
        }
        messages.extend(view.tail.into_iter().skip(start).map(|(_, msg)| msg));
        Ok(messages)
    }

    pub(crate) async fn remember(
        &self,
        sid: &str,
        prompt: &str,
        answer: &str,
    ) -> agent24_memory::Result<()> {
        self.check_owner()?;
        let deadline = tokio::time::Instant::now() + super::MEMORY_WRITE_BUDGET;
        // This budget bounds lock acquisition and the work needed to prepare a
        // turn. Once append_turn starts, its outcome must be observed: dropping
        // that future after COMMIT was sent cannot establish that it rolled back.
        let (guard, turn_no) = match tokio::time::timeout_at(deadline, async {
            let guard = self.session_lock(sid).await.lock_owned().await;
            self.ensure_imported(sid).await?;
            let turn_no = self.count_user_messages(sid).await? as u64;
            Ok::<_, MemoryError>((guard, turn_no))
        })
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                return Err(MemoryError::Io(
                    "session memory preparation timed out".into(),
                ));
            }
        };
        let append = async {
            self.log
                .append_turn(
                    &self.owner,
                    sid,
                    turn_no,
                    &Msg::user(prompt.to_owned()),
                    Origin {
                        source: "agent_loop".into(),
                        trust: Trust::UserSaid,
                    },
                    &Msg::assistant(Some(answer.to_owned()), vec![]),
                    Origin {
                        source: "agent_loop".into(),
                        trust: Trust::Model,
                    },
                )
                .await?;
            #[cfg(test)]
            if let Some(probe) = &self.commit_probe
                && probe.armed.swap(false, Ordering::AcqRel)
            {
                probe.committed.notify_one();
                probe.release.notified().await;
            }
            Ok::<_, MemoryError>(())
        };
        tokio::pin!(append);
        match tokio::time::timeout_at(deadline, &mut append).await {
            Ok(result) => result?,
            Err(_) => {
                tracing::warn!(
                    "session {sid} memory write budget expired; waiting for append transaction outcome"
                );
                #[cfg(test)]
                if let Some(probe) = &self.commit_probe {
                    probe.deadline_elapsed.notify_one();
                }
                append.await?;
            }
        }
        // append_turn is the durable boundary. Any compaction error or timeout
        // is best-effort and leaves every source message in the event log.
        // If confirmation consumed the budget, do not start database work only
        // to cancel its connection acquisition immediately.
        if tokio::time::Instant::now() >= deadline {
            return Ok(());
        }
        match tokio::time::timeout_at(deadline, self.compact(sid)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!("session {sid} compaction failed; originals retained: {err}")
            }
            Err(_) => tracing::error!("session {sid} compaction timed out; originals retained"),
        }
        drop(guard);
        Ok(())
    }

    async fn count_user_messages(&self, sid: &str) -> agent24_memory::Result<usize> {
        let mut after = 0;
        let mut count = 0;
        loop {
            let page = self
                .kv
                .events()
                .scan(
                    &EventQuery::owner(&self.owner)
                        .session(sid)
                        .after(after)
                        .limit(SCAN_PAGE),
                )
                .await?;
            if page.is_empty() {
                break;
            }
            for row in &page {
                after = row.seq;
                if row.event.kind == "message" {
                    let message: Msg = serde_json::from_value(row.event.body.clone())?;
                    if message.role == "user" {
                        count += 1;
                    }
                }
            }
            if page.len() < SCAN_PAGE as usize {
                break;
            }
        }
        Ok(count)
    }

    async fn compact(&self, sid: &str) -> agent24_memory::Result<()> {
        let view = self.log.load_view(&self.owner, sid).await?;
        let policy = normalized_policy(self.policy);
        if view.tail.len() <= policy.max_recent {
            return Ok(());
        }
        let mut fold = view.tail.len().saturating_sub(policy.keep_recent);
        while fold < view.tail.len() && view.tail[fold].1.role == "tool" {
            fold += 1;
        }
        if fold == 0 {
            return Ok(());
        }
        let messages: Vec<Msg> = view.tail[..fold]
            .iter()
            .map(|(_, msg)| msg.clone())
            .collect();
        match self
            .summarizer
            .summarize(view.summary.as_deref(), &messages)
            .await
        {
            Ok(summary) => {
                let summary = cap_summary(summary, policy.max_summary_chars);
                let covered_through_seq = view.tail[fold - 1].0;
                self.log
                    .append_summary(&self.owner, sid, &summary, covered_through_seq)
                    .await?;
            }
            Err(err) => return Err(MemoryError::Summarizer(err)),
        }
        Ok(())
    }
}

fn normalized_policy(policy: CompactionPolicy) -> CompactionPolicy {
    let max_recent = policy.max_recent.max(1);
    CompactionPolicy {
        max_recent,
        keep_recent: policy.keep_recent.min(max_recent.saturating_sub(1)),
        max_summary_chars: policy.max_summary_chars.max(1),
    }
}

fn cap_summary(summary: String, max: usize) -> String {
    if summary.chars().count() <= max {
        return summary;
    }
    let kept: String = summary.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}
