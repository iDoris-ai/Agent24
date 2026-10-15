//! One extraction, call by call (§3.2): every window with every batch of
//! fields, through the kernel's model router, which runs only local models
//! for this module. What each call answers is grounded and merged per field.
//!
//! Calls go one at a time. An answer cut off at the token limit is asked
//! again in two halves of its batch; one not in the schema's shape is asked
//! once more. A window the model rejects (most likely too long for its
//! context) is asked again in two halves. A busy or rate-limited kernel is
//! waited out, up to two minutes a call; a timeout is retried once; no local
//! model to answer is asked again after the router's longest cooldown, up to
//! three minutes of such waiting a call. Every
//! call must name the model that answered, and it must be the same model
//! throughout: an extraction never mixes two.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use agent24_os_sdk::{
    ClientError, CompleteRequest, CompleteResult, MODEL_NO_PROVIDER_WAIT, ModelClient, ServedTier,
    UnavailableCause,
};

use super::Field;
use super::ground::Source;
use super::merge::{Merged, Said, merge, said};
use super::prompt::{AnswerValue, Unanswered, answer_format, messages, parse};
use super::window::{BATCH, BUDGET, Window, batches, windows};

/// What the model may write in one answer, at most (the kernel's limit).
const MAX_TOKENS: u32 = 4096;
/// A model id's bytes, at most, as migration 0006 keeps it (and no NUL).
const MAX_MODEL_ID: usize = 256;
/// How a busy kernel is waited out: first pause, doubling to at most 30 s,
/// for at most two minutes a call. And how long to wait after no local model
/// answered: the router's longest cooldown each time (ME4-S2a §2.6), for at
/// most three of them a call.
#[derive(Debug, Clone, Copy)]
struct Pace {
    first: Duration,
    longest: Duration,
    max_wait: Duration,
    no_provider: Duration,
    no_provider_max: Duration,
}

const PACE: Pace = Pace {
    first: Duration::from_secs(1),
    longest: Duration::from_secs(30),
    max_wait: Duration::from_secs(120),
    no_provider: MODEL_NO_PROVIDER_WAIT,
    no_provider_max: MODEL_NO_PROVIDER_WAIT.saturating_mul(3),
};

pub type Call = Pin<Box<dyn Future<Output = Result<CompleteResult, ClientError>> + Send>>;
type Pieces<'b> =
    Pin<Box<dyn Future<Output = Result<Vec<(Window, Vec<AnswerValue>)>, Failure>> + Send + 'b>>;

/// The job an extraction runs for: whether to go on, asked before every
/// call and every wait, and where it has got to.
pub trait Job: Send + Sync {
    fn go_on(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
    fn progress(&self, done: usize, total: usize) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// The model, as the extraction sees it: the kernel's, or a stand-in.
pub trait Model: Send + Sync {
    fn complete(&self, req: CompleteRequest) -> Call;
}

/// The kernel's model router, through this module's grant.
pub struct Kernel(pub Arc<ModelClient>);

impl Model for Kernel {
    fn complete(&self, req: CompleteRequest) -> Call {
        let client = self.0.clone();
        // A background call: the request that started the job has ended.
        Box::pin(async move { client.complete(&req, None).await })
    }
}

/// Why an extraction did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// No usable model, or it would not answer: `engine_unavailable`.
    Engine(String),
    /// Its answers were not answers, asked again: `parse_failed`.
    Unanswered(String),
    /// The kernel is stopping: the job is interrupted, to run again.
    Interrupted,
    /// The job is being cancelled.
    Cancelled,
}

/// A finished extraction: the model that made it, and each field's value
/// in the order the fields were asked.
#[derive(Debug, Clone, PartialEq)]
pub struct Extracted {
    pub model_id: String,
    pub values: Vec<Merged>,
}

/// What a call came to, short of an answer.
enum Refused {
    /// The model rejected the request: most likely too long for it.
    TooLong,
    Failed(Failure),
}

struct Run<'a> {
    pace: Pace,
    job: &'a dyn Job,
    model: &'a dyn Model,
    src: &'a Source<'a>,
    model_id: Option<String>,
}

impl Run<'_> {
    /// One call, waiting out a busy kernel and retrying a timeout once.
    async fn call(&mut self, window: &Window, batch: &[Field]) -> Result<String, Refused> {
        let mut waited = Duration::ZERO;
        let mut pause = self.pace.first;
        let mut timeouts = 0;
        let mut no_provider = Duration::ZERO;
        loop {
            if !self.job.go_on().await {
                return Err(Refused::Failed(Failure::Cancelled));
            }
            let req = CompleteRequest {
                messages: messages(batch, self.src.layer, window),
                response_format: Some(answer_format(batch)),
                max_tokens: Some(MAX_TOKENS),
                complexity: None,
            };
            let engine = |why: String| Refused::Failed(Failure::Engine(why));
            match self.model.complete(req).await {
                Ok(done) => return self.answered(done).map_err(Refused::Failed),
                Err(ClientError::Busy(_) | ClientError::RateLimited(_))
                    if waited < self.pace.max_wait =>
                {
                    if !self.job.go_on().await {
                        return Err(Refused::Failed(Failure::Cancelled));
                    }
                    // Never past the budget: the last pause is what is left.
                    let wait = pause.min(self.pace.max_wait - waited);
                    tokio::time::sleep(wait).await;
                    waited += wait;
                    pause = (pause * 2).min(self.pace.longest);
                }
                Err(ClientError::Timeout(_)) if timeouts == 0 => timeouts += 1,
                // A stall the kernel caught, a model reloading, the GPU taken
                // for a while: asked again once every cooled provider is back.
                // Another call of this module failing meanwhile cools one
                // again, so this goes on for up to three cooldowns. No
                // provider at all fails the same way, three minutes later.
                Err(ClientError::Unavailable {
                    cause: UnavailableCause::NoProvider,
                    ..
                }) if no_provider < self.pace.no_provider_max => {
                    let wait = self
                        .pace
                        .no_provider
                        .min(self.pace.no_provider_max - no_provider);
                    no_provider += wait;
                    if !self.wait(wait).await {
                        return Err(Refused::Failed(Failure::Cancelled));
                    }
                }
                Err(ClientError::Unavailable {
                    cause: UnavailableCause::RequestRejected,
                    ..
                }) => {
                    return Err(Refused::TooLong);
                }
                Err(
                    ClientError::Cancelled
                    | ClientError::Draining(_)
                    | ClientError::NotReady(_)
                    | ClientError::ConnectionLost,
                ) => return Err(Refused::Failed(Failure::Interrupted)),
                Err(e) => return Err(engine(format!("the local model: {e}"))),
            }
        }
    }

    /// Waits `total`, asking every `first` whether the job goes on: a job
    /// cancelled while it waits ends then, not when the wait is over.
    /// The caller asks once more before its next call, so this does not.
    async fn wait(&self, total: Duration) -> bool {
        debug_assert!(!self.pace.first.is_zero(), "a zero step never ends");
        let mut left = total;
        while !left.is_zero() {
            if !self.job.go_on().await {
                return false;
            }
            let step = left.min(self.pace.first);
            tokio::time::sleep(step).await;
            left -= step;
        }
        true
    }

    /// The answer's text, once its model is known to be the one, local.
    fn answered(&mut self, done: CompleteResult) -> Result<String, Failure> {
        let engine = |why: &str| Err(Failure::Engine(why.into()));
        if done.tier != ServedTier::Local {
            return engine("a model that is not local answered");
        }
        let Some(id) = done.model_id.filter(|id| !id.is_empty()) else {
            return engine("the model did not say which it is");
        };
        // Checked at its first answer, not when the extraction is written:
        // an id that cannot be kept would waste every call after it.
        if id.len() > MAX_MODEL_ID || id.contains('\0') {
            return engine("the model's id cannot be kept");
        }
        match &self.model_id {
            Some(known) if *known != id => engine("the local model changed during the extraction"),
            _ => {
                self.model_id = Some(id);
                Ok(done.text)
            }
        }
    }

    /// The answers for `batch` over `window`, as (window, answers) pieces in
    /// window order: a window rejected as too long is asked in halves, a
    /// batch cut off in halves, a malformed answer once more.
    fn answer<'b>(&'b mut self, window: Window, batch: Vec<Field>) -> Pieces<'b> {
        Box::pin(async move {
            let mut malformed = None;
            for _ in 0..2 {
                let text = match self.call(&window, &batch).await {
                    Ok(text) => text,
                    Err(Refused::Failed(f)) => return Err(f),
                    Err(Refused::TooLong) => {
                        let Some((a, b)) = window.halves() else {
                            return Err(Failure::Engine(
                                "the local model rejects even one block".into(),
                            ));
                        };
                        let mut out = self.answer(a, batch.clone()).await?;
                        out.extend(self.answer(b, batch).await?);
                        return Ok(out);
                    }
                };
                match parse(&batch, &text) {
                    Ok(values) => return Ok(vec![(window, values)]),
                    Err(Unanswered::Truncated) if batch.len() > 1 => {
                        let (a, b) = batch.split_at(batch.len() / 2);
                        let mut out = self.answer(window.clone(), a.to_vec()).await?;
                        out.extend(self.answer(window, b.to_vec()).await?);
                        return Ok(out);
                    }
                    Err(Unanswered::Truncated) => {
                        return Err(Failure::Unanswered("cut off even for one field".into()));
                    }
                    Err(Unanswered::Malformed(why)) => malformed = Some(why),
                }
            }
            Err(Failure::Unanswered(malformed.unwrap_or_default()))
        })
    }
}

/// Extracts `fields` from the layer in `src` for `job`: asked before every
/// call and wait whether to go on, told after each window and batch where
/// it has got to. `partial`: the layer has regions not read. Each field's
/// value stays within `max_bytes`.
///
/// # Errors
/// [`Failure`] when the model cannot be had, will not answer, or the job
/// is stopped.
pub async fn run(
    job: &dyn Job,
    model: &dyn Model,
    src: &Source<'_>,
    fields: &[Field],
    partial: bool,
    max_bytes: usize,
) -> Result<Extracted, Failure> {
    paced(PACE, job, model, src, fields, partial, max_bytes).await
}

async fn paced(
    pace: Pace,
    job: &dyn Job,
    model: &dyn Model,
    src: &Source<'_>,
    fields: &[Field],
    partial: bool,
    max_bytes: usize,
) -> Result<Extracted, Failure> {
    let windows = windows(src.layer, BUDGET);
    let batches = batches(fields, BATCH);
    let total = windows.len() * batches.len();
    let mut said_of: Vec<Vec<Said>> = vec![Vec::new(); fields.len()];
    let mut run = Run {
        pace,
        job,
        model,
        src,
        model_id: None,
    };
    let mut done = 0;
    for window in &windows {
        for batch in &batches {
            for (piece, answers) in run.answer(window.clone(), batch.clone()).await? {
                for a in &answers {
                    if let Some(i) = fields.iter().position(|f| f.key == a.key) {
                        said_of[i].push(said(src, &piece, a));
                    }
                }
            }
            done += 1;
            job.progress(done, total).await;
        }
    }
    let Some(model_id) = run.model_id else {
        return Err(Failure::Engine("no field was asked".into()));
    };
    let values = fields
        .iter()
        .zip(&said_of)
        .map(|(f, s)| merge(&f.key, s, partial, max_bytes).map_err(Failure::Unanswered))
        .collect::<Result<_, _>>()?;
    Ok(Extracted { model_id, values })
}

#[cfg(test)]
mod tests;
