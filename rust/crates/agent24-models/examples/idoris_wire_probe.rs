//! One non-streaming call through the real Agent24 router for IDORIS-06.
use agent24_models::{
    CompletionRequest, Msg, ResponseFormat, ToolSpec,
    router::{Complexity, ModelRouter, Privacy, TaskProfile},
};
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let complexity = match args.next().as_deref() {
        Some("simple" | "daily") => Complexity::Simple,
        Some("complex" | "deep") => Complexity::Complex,
        _ => return Err("expected simple|complex and a prompt".into()),
    };
    let prompt = args.next().ok_or("missing prompt")?;
    if args.next().is_some() || std::env::var_os("IDORIS_URL").is_none() {
        return Err("expected exactly two arguments and explicit IDORIS_URL".into());
    }
    let request = CompletionRequest {
        messages: vec![Msg::user(prompt)],
        model: None,
        tools: vec![ToolSpec {
            name: "probe_tool".into(),
            description: "wire acceptance".into(),
            parameters: serde_json::json!({"type":"object"}),
        }],
        response_format: Some(ResponseFormat::JsonSchema {
            name: "probe".into(),
            schema: serde_json::json!({"type":"object"}),
            strict: true,
        }),
        max_tokens: std::num::NonZeroU32::new(512),
        disable_thinking: true,
    };
    let served = ModelRouter::from_env()
        .complete_served(
            TaskProfile {
                privacy: Privacy::LocalOnly,
                complexity,
            },
            &request,
            &CancellationToken::new(),
        )
        .await?;
    let evidence = served.idoris.ok_or("missing iDoris evidence")?;
    println!(
        "{}",
        serde_json::json!({
            "provider": served.provider, "record_id": evidence.record_id,
            "locality": evidence.served_locality, "model": served.response.model_id,
            "content": served.response.message.content,
        })
    );
    Ok(())
}
