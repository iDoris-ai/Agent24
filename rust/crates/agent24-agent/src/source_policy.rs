use agent24_models::router::TaskProfile;
use agent24_protocol::Run;
use agent24_store::{MessageOrigin, MessageOriginLookup};

pub fn merge_source_policy(
    mut profile: TaskProfile,
    mode: agent24_store::SourceMode,
) -> TaskProfile {
    if mode == agent24_store::SourceMode::LocalOnly {
        profile.privacy = agent24_models::router::Privacy::LocalOnly;
    }
    profile
}

/// Completion→tools round trips per run before the run is failed. A model
/// stuck asking for tools forever must terminate deterministically.
pub const MAX_ITERATIONS: usize = 10;

pub(crate) const SOURCE_UNPROVEN_CODE: &str = "source_unproven";
pub(crate) const SOURCE_UNPROVEN_MESSAGE: &str =
    "无法证明本线程已落盘消息的来源，已停止发送。请新建会话。不要自动复制旧 recall 或待执行工具。";

pub(crate) const SKILL_LABEL_MESSAGE: &str =
    "这次运行钉了技能，但读不到它的来源标签，已停止。缺标签不会按普通来源处理。";
pub(crate) const PROVIDER_ROLE_MESSAGE: &str =
    "模型返回的角色不是 assistant，已停止。不会执行这一轮的工具。";

pub(crate) fn origin_allows_send(lookup: &MessageOriginLookup) -> bool {
    match lookup {
        // A stored HostRecall with a SQL NULL recall_ref is still unproven.
        // Present HostSystem is sent verbatim (R05-5 section 3).
        MessageOriginLookup::Present(stored) => !matches!(
            stored.origin,
            MessageOrigin::HostRecall { recall_ref: None }
        ),
        MessageOriginLookup::Missing | MessageOriginLookup::Unreadable { .. } => false,
    }
}

/// Compact JSON array of every assertion id, sorted lexicographically.
/// One id is `["id"]`. `serde_json::to_string` emits no spaces.
pub(crate) fn canonical_recall_ref(ids: &[String]) -> Result<String, serde_json::Error> {
    let mut sorted = ids.to_vec();
    sorted.sort();
    serde_json::to_string(&sorted)
}

pub(crate) fn unproven_source_failure(run: &Run) -> bool {
    run.error
        .as_ref()
        .is_some_and(|error| error.code == SOURCE_UNPROVEN_CODE)
}

/// Tool calls executed per assistant turn; the rest are answered with a
/// "skipped" tool result so the wire protocol stays balanced.
pub const MAX_TOOL_CALLS_PER_TURN: usize = 16;
