//! `agent24-os-sdk` — ME4-S3 §7 slice ②: the out-of-process module SDK.
//!
//! Design: `docs/design/ME4-S3-os-sdk.md` (v4, frozen). This crate is the
//! prototype scope of §7's slice ② — the SDK crate itself, its five typed
//! clients (Events/Memory/Approval/Scheduler/Model), and the `fired`
//! registration point. It never owns a socket and never parses protocol
//! bytes into JSON itself (§2.2): everything wire-level goes through
//! [`agent24_os_proto::module`], which slice ① (`feat/me4-5.1.2a-proto-
//! module-side`) added. The only ordinary dependency is `agent24-os-proto`
//! (J-S2).
//!
//! # What is NOT in this prototype round
//!
//! Per the user's 2026-09-26 scope decision recorded in the design's §0/§6:
//! the "prototype-round-optional" judgements (the 26-item clippy positive
//! control, the J-S1b/J-S3 proto/os-fd back-door scripts, a dedicated
//! `macos-latest` CI job, and Sin90's migration expected-patch script) are
//! not implemented here — `clippy.toml` below carries a basic list (socket
//! types + `from_raw_fd` + `serde_json::from_slice`) rather than the full
//! 26-entry one. `examples/minimal`, the mount smoke test, `CHANGELOG.md`
//! and the `agent24-os-sdk-v0.1.0` tag (design §7's "c2") are release-adjacent
//! and out of scope for this slice too — see the PR body for the full list
//! of deviations from the frozen design.

pub mod clients;
mod context;
mod error;
mod fired;
mod module;

pub use agent24_os_proto::kernel_call::FiredBody;
pub use agent24_os_proto::module::SPAWN_ENV_VARS;
pub use clients::{
    ApprovalAnswer, ApprovalClient, ApprovalDecision, ApprovalKind, ApprovalSubmit,
    CompleteRequest, CompleteResult, Complexity, DEDUP_KEY_FIELD, DeleteOutcome, DeleteResult,
    EventSink, EventSinkConfig, EventsClient, JsonSchemaFormat, LastFire, LastFires, ListResult,
    MODEL_RESPONSE_TIMEOUT, MemoryClient, ModelClient, ModelMessage, ModelRole,
    RECALL_PRECHECK_MAX_PAGES, RECALL_PRECHECK_PAGE_SIZE, RecallPage, Recollection, RememberOnce,
    Remembered, ScheduleSpec, ScheduleState, SchedulerClient, ServedTier, UpsertOutcome,
    UpsertRequest, UpsertResult, Usage,
};
pub use context::{ApprovalToken, RequestContext, RequestId};
pub use error::{ClientError, UnavailableCause};
pub use fired::{FIRED_PATH, FiredDelivery, FiredRejection, with_fired};
pub use module::{Module, ModuleBuilder, PROTOCOL_MAX, PROTOCOL_MIN, SdkError};

/// proto's in-memory fake kernel, forwarded (L8): a downstream module's
/// dev-dependencies only ever need to name this crate, not
/// `agent24-os-proto` directly (J-S2 is about ordinary dependencies; a test
/// fixture reached only through `test-util` is not one).
#[cfg(feature = "test-util")]
pub use agent24_os_proto::module::testing;
