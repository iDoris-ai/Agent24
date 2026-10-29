//! 模块工具卡片与决定。沿用 CLI 的 host authority，提升级校验留在 daemon。
use serde_json::json;

#[derive(clap::Subcommand)]
pub enum Action {
    /// 读取 daemon 生成的工具授权卡片
    Card {
        module: String,
        tool: String,
        #[arg(long)]
        json: bool,
    },
    /// 提交卡片上的决定；必须回传所查看卡片的绑定值
    Decide {
        module: String,
        tool: String,
        #[arg(value_enum)]
        decision: Decision,
        #[arg(long)]
        fingerprint: String,
        #[arg(long)]
        module_version: String,
        #[arg(long)]
        artifact: String,
        #[arg(long)]
        expires_at: String,
        #[arg(long = "resource")]
        resources: Vec<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Granted,
    Denied,
}

pub async fn run(action: Action) -> Result<(), String> {
    let ep = super::connect().await?;
    let result = execute(&ep, action).await;
    super::finish(ep).await;
    println!("{}", result?);
    Ok(())
}

pub async fn execute(ep: &super::Endpoint, action: Action) -> Result<String, String> {
    let (request, raw) = match action {
        Action::Card { module, tool, json } => (
            super::client().get(format!(
                "{}/api/v1/module-consents/modules/{}/tools/{}/card",
                ep.base,
                super::qs_encode(&module),
                super::qs_encode(&tool)
            )),
            json,
        ),
        Action::Decide {
            module,
            tool,
            decision,
            fingerprint,
            module_version,
            artifact,
            expires_at,
            resources,
            json,
        } => (
            super::client()
                .post(format!(
                    "{}/api/v1/module-consents/modules/{}/tools/{}",
                    ep.base,
                    super::qs_encode(&module),
                    super::qs_encode(&tool)
                ))
                .json(&json!({
                    "decision": decision,
                    "fingerprint": fingerprint,
                    "module_version": module_version,
                    "artifact": artifact,
                    "expires_at": expires_at,
                    "resources": resources,
                })),
            json,
        ),
    };
    let response = super::bearer(ep, request)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("daemon returned {status}: {text}"));
    }
    let body: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("daemon returned {status} but the body is not JSON: {e}"))?;
    if raw {
        Ok(text)
    } else {
        serde_json::to_string_pretty(&body).map_err(|e| e.to_string())
    }
}
