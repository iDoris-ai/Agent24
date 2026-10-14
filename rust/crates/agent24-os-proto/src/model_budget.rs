//! How long a module's model call may take (ME4-S2a §2.1, §3): the numbers
//! the kernel and a module's SDK share, so the two cannot drift.
//!
//! A call is bounded by what it does, not by a fixed total: its first chunk
//! must come within [`MODEL_FIRST_CHUNK_TIMEOUT`], and then a chunk within
//! every [`MODEL_STALL_TIMEOUT`]; and the whole call has one deadline, set
//! once, of [`model_call_budget`] for the tokens it may write. The RPC
//! backstop comes after any such deadline, and the SDK waits a little longer
//! still, so a module hears what the kernel decided rather than its own timer.

use std::time::Duration;

/// The most tokens a module may ask a call to write.
pub const MODEL_MAX_TOKENS_CEILING: u32 = 4096;
/// Until the first chunk: loading the model, waiting in the backend, reading the prompt. ⚖️
pub const MODEL_FIRST_CHUNK_TIMEOUT: Duration = Duration::from_secs(300);
/// Between two chunks, once the model writes: longer is a stall. ⚖️
pub const MODEL_STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Each token at the slowest pace allowed, one a second. ⚖️
pub const MODEL_PER_TOKEN_BUDGET: Duration = Duration::from_secs(1);
/// Admission and routing before a call's deadline is set. ⚖️
pub const MODEL_ROUTING_MARGIN: Duration = Duration::from_secs(60);

/// The deadline of one call that may write `max_tokens`: its first chunk,
/// then every token at the slowest pace allowed.
#[must_use]
pub const fn model_call_budget(max_tokens: u32) -> Duration {
    MODEL_FIRST_CHUNK_TIMEOUT.saturating_add(MODEL_PER_TOKEN_BUDGET.saturating_mul(max_tokens))
}

/// The RPC backstop of `_a24/model/complete`: strictly after any call's deadline.
pub const MODEL_CALL_BACKSTOP: Duration =
    model_call_budget(MODEL_MAX_TOKENS_CEILING).saturating_add(MODEL_ROUTING_MARGIN);
/// What a module's SDK waits for a reply: the backstop and 5s, as 125 = 120 + 5 was.
pub const MODEL_RESPONSE_TIMEOUT: Duration =
    MODEL_CALL_BACKSTOP.saturating_add(Duration::from_secs(5));

// J13: the SDK outwaits the kernel, and the kernel outwaits any call's deadline.
const _: () = assert!(MODEL_RESPONSE_TIMEOUT.as_nanos() > MODEL_CALL_BACKSTOP.as_nanos());
const _: () = assert!(
    MODEL_CALL_BACKSTOP.as_nanos() > model_call_budget(MODEL_MAX_TOKENS_CEILING).as_nanos()
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_numbers_follow_from_the_ceiling() {
        assert_eq!(model_call_budget(1024), Duration::from_secs(1324));
        assert_eq!(
            model_call_budget(MODEL_MAX_TOKENS_CEILING),
            Duration::from_secs(4396)
        );
        assert_eq!(MODEL_CALL_BACKSTOP, Duration::from_secs(4456));
        assert_eq!(MODEL_RESPONSE_TIMEOUT, Duration::from_secs(4461));
        assert_eq!(
            model_call_budget(u32::MAX),
            MODEL_FIRST_CHUNK_TIMEOUT + Duration::from_secs(u64::from(u32::MAX))
        );
    }
}
