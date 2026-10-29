//! 群聊的 CLI 参数与 REST 请求。授权和群状态均由 daemon 校验。
use clap::{Args, Subcommand};
use reqwest::Method;
use serde_json::{Value, json};

#[derive(Args)]
pub struct Identity {
    /// 使用的身份昵称，省略则使用默认身份
    #[arg(long)]
    identity: Option<String>,
}

#[derive(Subcommand)]
pub enum GroupsAction {
    /// 列出当前身份加入的群
    List {
        #[command(flatten)]
        identity: Identity,
    },
    /// 创建群并邀请成员（npub）
    Create {
        name: String,
        #[arg(required = true, num_args = 1..)]
        invitees: Vec<String>,
        #[command(flatten)]
        identity: Identity,
    },
    /// 列出群成员
    Members {
        group_id: String,
        #[command(flatten)]
        identity: Identity,
    },
    /// 列出群邀请（仅创建者可读）
    Invites {
        group_id: String,
        #[command(flatten)]
        identity: Identity,
    },
    /// 读取本地群消息，支持游标翻页
    Messages {
        group_id: String,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: Option<u32>,
        #[arg(long, conflicts_with = "after_id")]
        before_id: Option<String>,
        #[arg(long)]
        after_id: Option<String>,
        #[command(flatten)]
        identity: Identity,
    },
    /// 发送群消息；部分失败后应使用 retry，避免重复发送
    Send {
        group_id: String,
        body: String,
        #[arg(long)]
        logical_id: Option<String>,
        #[arg(long)]
        max_retries: Option<u32>,
        #[command(flatten)]
        identity: Identity,
    },
    /// 查询每位收件人的投递状态（relay 接受不代表已读）
    Status {
        group_id: String,
        logical_id: String,
        #[command(flatten)]
        identity: Identity,
    },
    /// 重试失败收件人，复用原始事件
    Retry {
        group_id: String,
        logical_id: String,
        #[arg(long)]
        recipient: Option<String>,
        #[command(flatten)]
        identity: Identity,
    },
    /// 接受指定邀请；pending 表示仍在等待其他成员
    Accept {
        invite_id: String,
        #[command(flatten)]
        identity: Identity,
    },
}

pub fn request(action: GroupsAction) -> (Method, String, Option<Value>) {
    let (method, mut path, mut body, identity) = match action {
        GroupsAction::List { identity } => (
            Method::GET,
            "/api/v1/comm/groupchat".to_owned(),
            None,
            identity,
        ),
        GroupsAction::Create {
            name,
            invitees,
            identity,
        } => (
            Method::POST,
            "/api/v1/comm/groupchat".to_owned(),
            Some(json!({"name":name,"invitees":invitees})),
            identity,
        ),
        GroupsAction::Members { group_id, identity } => (
            Method::GET,
            format!(
                "/api/v1/comm/groupchat/{}/members",
                super::qs_encode(&group_id)
            ),
            None,
            identity,
        ),
        GroupsAction::Invites { group_id, identity } => (
            Method::GET,
            format!(
                "/api/v1/comm/groupchat/{}/invites",
                super::qs_encode(&group_id)
            ),
            None,
            identity,
        ),
        GroupsAction::Messages {
            group_id,
            limit,
            before_id,
            after_id,
            identity,
        } => (
            Method::GET,
            {
                let mut path = format!(
                    "/api/v1/comm/groupchat/{}/messages",
                    super::qs_encode(&group_id)
                );
                for (key, value) in [
                    ("limit", limit.map(|v| v.to_string())),
                    ("before_id", before_id),
                    ("after_id", after_id),
                ] {
                    if let Some(value) = value {
                        append_query(&mut path, key, &value);
                    }
                }
                path
            },
            None,
            identity,
        ),
        GroupsAction::Send {
            group_id,
            body,
            logical_id,
            max_retries,
            identity,
        } => {
            let mut payload = json!({"body":body});
            if let Some(id) = logical_id {
                payload["logical_id"] = json!(id);
            }
            if let Some(n) = max_retries {
                payload["max_retries"] = json!(n);
            }
            (
                Method::POST,
                format!(
                    "/api/v1/comm/groupchat/{}/messages",
                    super::qs_encode(&group_id)
                ),
                Some(payload),
                identity,
            )
        }
        GroupsAction::Status {
            group_id,
            logical_id,
            identity,
        } => (
            Method::GET,
            format!(
                "/api/v1/comm/groupchat/{}/messages/{}",
                super::qs_encode(&group_id),
                super::qs_encode(&logical_id)
            ),
            None,
            identity,
        ),
        GroupsAction::Retry {
            group_id,
            logical_id,
            recipient,
            identity,
        } => {
            let mut body = json!({});
            if let Some(recipient) = recipient {
                body["recipient"] = json!(recipient);
            }
            (
                Method::POST,
                format!(
                    "/api/v1/comm/groupchat/{}/messages/{}/retry",
                    super::qs_encode(&group_id),
                    super::qs_encode(&logical_id)
                ),
                Some(body),
                identity,
            )
        }
        GroupsAction::Accept {
            invite_id,
            identity,
        } => (
            Method::POST,
            format!(
                "/api/v1/comm/groupchat/invites/{}/accept",
                super::qs_encode(&invite_id)
            ),
            Some(json!({})),
            identity,
        ),
    };
    if let Some(identity) = identity.identity {
        if let Some(body) = &mut body {
            body["identity"] = json!(identity);
        } else {
            append_query(&mut path, "identity", &identity);
        }
    }
    (method, path, body)
}

pub fn append_query(path: &mut String, key: &str, value: &str) {
    path.push(if path.contains('?') { '&' } else { '?' });
    path.push_str(key);
    path.push('=');
    path.push_str(&super::qs_encode(value));
}
