use std::sync::Arc;

use agent24_memory::hybrid::{QueryRewriter, normalize_rewrites, rewrite_prompt};
use agent24_memory::session::Summarizer;
use agent24_models::router::{ModelRouter, TaskProfile};
use agent24_models::{CompletionRequest, Msg};
use tokio_util::sync::CancellationToken;

use crate::merge_source_policy;

/// A [`Summarizer`] backed by the model router.
///
/// Session compaction currently folds with `LocalOnly`: source tags are
/// attached to individual runs, while a session can contain messages from
/// multiple runs and this interface does not carry a run ID. Until that
/// provenance can be resolved across the compacted messages, fail closed.
pub struct RouterSummarizer {
    router: Arc<ModelRouter>,
    /// Daemon shutdown token — compaction can call a slow provider, and a stuck
    /// summarizer must not outlive shutdown (review D5b).
    shutdown: CancellationToken,
}

/// Per-message budget in the summarization transcript. Elision is marked
/// so the summarizer knows content was cut. Full originals
/// remain in the session event log.
const SUMMARY_MSG_MAX_CHARS: usize = 8000;
/// Whole-transcript budget, so a huge fold can't build an unbounded prompt.
const SUMMARY_TRANSCRIPT_MAX_CHARS: usize = 32_000;

impl RouterSummarizer {
    pub fn new(router: Arc<ModelRouter>, shutdown: CancellationToken) -> Self {
        Self { router, shutdown }
    }
}

#[async_trait::async_trait]
impl Summarizer for RouterSummarizer {
    async fn summarize(
        &self,
        prior: Option<&str>,
        messages: &[Msg],
    ) -> std::result::Result<String, String> {
        let mut transcript = String::new();
        for m in messages {
            let content = m.content.as_deref().unwrap_or("");
            if content.is_empty() {
                continue;
            }
            // Mark elision explicitly; originals remain in the event log.
            let (body, elided) = if content.chars().count() > SUMMARY_MSG_MAX_CHARS {
                let kept: String = content.chars().take(SUMMARY_MSG_MAX_CHARS).collect();
                (kept, true)
            } else {
                (content.to_owned(), false)
            };
            transcript.push_str(&format!("{}: {body}", m.role));
            if elided {
                transcript.push_str(" …[truncated for summarization]");
            }
            transcript.push('\n');
            if transcript.chars().count() >= SUMMARY_TRANSCRIPT_MAX_CHARS {
                transcript.push_str("…[earlier messages omitted]\n");
                break;
            }
        }
        let prompt = match prior {
            Some(prior) => format!(
                "Update this running summary of a conversation so it still captures \
                 everything needed to continue. Reply with the updated summary only.\n\n\
                 EXISTING SUMMARY:\n{prior}\n\nNEW MESSAGES:\n{transcript}"
            ),
            None => format!(
                "Summarize this conversation so it can be continued later, keeping \
                 decisions, facts and open threads. Reply with the summary only.\n\n\
                 {transcript}"
            ),
        };
        let req = CompletionRequest {
            messages: vec![Msg::user(prompt)],
            model: None,
            tools: vec![],
            response_format: None,
            max_tokens: None,
            disable_thinking: false,
            idoris_headroom_confirmed: false,
            temperature: None,
            progress: None,
        };
        let profile =
            merge_source_policy(TaskProfile::default(), agent24_store::SourceMode::LocalOnly);
        let (_provider, res) = self
            .router
            .complete(profile, &req, &self.shutdown)
            .await
            .map_err(|e| e.to_string())?;
        let summary = res.message.content.unwrap_or_default().trim().to_owned();
        if summary.is_empty() {
            return Err("summarizer returned an empty summary".to_owned());
        }
        Ok(summary)
    }
}

/// VR-6: rewrite a near-miss question into short retrieval phrases.
///
/// Same local-only router path as [`RouterSummarizer`]. The retriever applies
/// the 2s timeout and falls back to keyword search when this errors.
pub struct RouterRewriter {
    router: Arc<ModelRouter>,
    shutdown: CancellationToken,
    model: String,
}

impl RouterRewriter {
    #[must_use]
    pub fn new(
        router: Arc<ModelRouter>,
        shutdown: CancellationToken,
        model: impl Into<String>,
    ) -> Self {
        Self {
            router,
            shutdown,
            model: model.into(),
        }
    }
}

#[async_trait::async_trait]
impl QueryRewriter for RouterRewriter {
    async fn rewrite(&self, query: &str) -> Result<Vec<String>, String> {
        let req = CompletionRequest {
            messages: vec![Msg::user(rewrite_prompt(query))],
            model: Some(self.model.clone()),
            tools: vec![],
            response_format: None,
            max_tokens: std::num::NonZeroU32::new(64),
            disable_thinking: true,
            idoris_headroom_confirmed: false,
            temperature: Some(0.0),
            progress: None,
        };
        let profile =
            merge_source_policy(TaskProfile::default(), agent24_store::SourceMode::LocalOnly);
        let (_provider, res) = self
            .router
            .complete(profile, &req, &self.shutdown)
            .await
            .map_err(|e| e.to_string())?;
        Ok(normalize_rewrites(
            res.message.content.unwrap_or_default().as_str(),
        ))
    }
}
