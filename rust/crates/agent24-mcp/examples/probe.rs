//! Manual end-to-end probe against a real MCP server.
//! Run: cargo run -p agent24-mcp --example probe -- <cmd> [args...]
use agent24_mcp::{McpServerSpec, connect_and_build_tools};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let mut a = std::env::args().skip(1);
    let Some(cmd) = a.next() else {
        eprintln!("usage: probe <command> [args...]");
        return;
    };
    let args: Vec<String> = a.collect();
    let spec = McpServerSpec::new("probe", cmd, args);
    let cancel = CancellationToken::new();
    match connect_and_build_tools(&spec, cancel).await {
        Ok((server, tools)) => {
            println!("connected to {} — {} tool(s):", server.name(), tools.len());
            for t in &tools {
                let i = t.info();
                println!("  {} (approval={})", i.name, i.requires_approval);
            }
            // Prove the round-trip, not just discovery: actually call one.
            if let Some(t) = tools.iter().find(|t| t.info().name.ends_with("_echo")) {
                let ctx = agent24_tools::ToolContext {
                    run_id: "probe".into(),
                    session_id: None,
                    tool_call_id: "tc".into(),
                };
                let mut input = serde_json::Map::new();
                input.insert("message".into(), serde_json::json!("hello from agent24"));
                match t.call(&ctx, &input, &CancellationToken::new()).await {
                    Ok(out) => println!("\ncall {} -> {out:?}", t.info().name),
                    Err(e) => println!("\ncall FAILED: {e}"),
                }
            }
        }
        Err(e) => println!("FAILED: {e}"),
    }
}
