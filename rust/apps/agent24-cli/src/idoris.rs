//! Daemon-only iDoris client. Capability enrollment joins here after CORE-2.

use super::{Endpoint, bearer, client, endpoint_from_state, finish, health_ok, spawn_daemon};
use clap::{Args, Subcommand};
use reqwest::Method;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};
use std::time::Duration;
use tokio::signal::unix::{Signal, SignalKind, signal};

#[derive(Subcommand)]
pub enum IdorisAction {
    /// Show the daemon's managed iDoris status
    Status(Output),
    /// Download, verify and install the locked release
    Install {
        /// Accept the locked release and installation risks without prompting
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Start configured iDoris (run connect first on a new installation)
    Start(Output),
    /// Stop managed iDoris
    Stop(Output),
    /// List oMLX endpoints, identity, authentication and model IDs
    Candidates(Output),
    /// Configure oMLX, issue credentials, start and verify normal chat
    Connect(Connect),
}
#[derive(Args)]
pub struct Output {
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
pub struct Connect {
    #[arg(long)]
    endpoint: String,
    #[arg(long)]
    daily: String,
    #[arg(long)]
    deep: Option<String>,
    #[arg(long)]
    core_reserved_bytes: Option<u64>,
    #[arg(long)]
    save_omlx_key: bool,
    #[arg(long)]
    replace: bool,
    /// Accept credential sharing and the selected models without prompting
    #[arg(long)]
    yes: bool,
    #[command(flatten)]
    output: Output,
}

#[derive(Deserialize, Serialize)]
struct Failure {
    code: String,
    message: String,
}
impl Failure {
    fn local(code: &str) -> Self {
        Self {
            code: code.to_owned(),
            message: english(code).to_owned(),
        }
    }
}
fn english(code: &str) -> &'static str {
    match code {
        "consent_required" => "Consent required. Use --yes in a non-interactive terminal.",
        "not_installed" => "iDoris is not installed. Run agent24 idoris install first.",
        "not_configured" => "iDoris is not configured. Run agent24 idoris connect first.",
        "instance_conflict" => "Another iDoris instance owns the database.",
        "keychain_locked" => "Unlock the keychain and allow Agent24 access.",
        "key_unavailable" => "A required credential is unavailable in the keychain.",
        "contract_mismatch" => "iDoris does not match the expected readiness contract.",
        "start_failed" => "iDoris failed to start.",
        "stop_timeout" => "Stopping timed out; forced cleanup was requested.",
        "busy" | "operation_in_progress" => "Another lifecycle operation is in progress.",
        "cancelled" => "The operation was cancelled.",
        "not_found" => "The operation was not found.",
        "not_cancellable" => "Cleanup cannot be cancelled. Waiting for completion.",
        "shutting_down" => "The daemon is shutting down.",
        "invalid_request" => "The daemon rejected the request format.",
        "persistence_failed" => "The daemon could not save the lifecycle record.",
        "omlx_unreachable" => "oMLX is unreachable.",
        "omlx_auth_required" => "oMLX requires authentication.",
        "omlx_auth_rejected" => "oMLX rejected the credential.",
        "unknown_service" => "The endpoint is not a confirmed oMLX service.",
        "model_not_found" => "A selected model is not available in oMLX.",
        "setup_failed" => "iDoris setup failed.",
        "verify_failed" => "Normal chat did not return verified local iDoris evidence.",
        "idoris_headroom_confirmation_required" => {
            "iDoris needs reserved memory headroom. Send agent24 chat \"Hi\" --confirm-headroom to confirm this request."
        }
        "replace_required" => "A connection already exists. Use --replace to replace it.",
        "host_authority_unavailable" => {
            "Capability enrollment requires the CORE-2 client. Use a legacy or ProductHost daemon."
        }
        "daemon_unavailable" => "Could not connect to or start the daemon.",
        "transport_error" => "The daemon request failed. Check daemon status.",
        "invalid_response" => "The daemon returned an invalid response.",
        "signal_unavailable" => "Could not register the interrupt handler.",
        _ => "The daemon operation failed. Check agent24 idoris status --json.",
    }
}

// Deserialize only public fields: operation receipts also contain private retry metadata.
#[derive(Deserialize, Serialize)]
struct Operation {
    operation_id: String,
    action: String,
    phase: String,
    result: Option<String>,
    error_code: Option<String>,
    cancellable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    setup_code: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct Status {
    state: String,
    version: Option<String>,
    data_port: Option<u16>,
    admin_port: Option<u16>,
    roles: Vec<String>,
    last_error: Option<String>,
    current_operation: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct Candidate {
    endpoint: String,
    identity: String,
    authentication: String,
    models: Vec<String>,
}
#[derive(Deserialize, Serialize)]
struct Candidates {
    candidates: Vec<Candidate>,
    key_from_local_settings: bool,
}

fn consent(yes: bool, description: &str) -> Result<(), Failure> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(Failure::local("consent_required"));
    }
    eprintln!("{description}");
    eprint!("Type yes to continue: ");
    std::io::stderr()
        .flush()
        .map_err(|_| Failure::local("consent_required"))?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|_| Failure::local("consent_required"))?;
    if answer.trim() == "yes" {
        Ok(())
    } else {
        Err(Failure::local("consent_required"))
    }
}

async fn host() -> Result<Endpoint, Failure> {
    use agent24_protocol::state_file;
    // A lifecycle command must leave its host running after the CLI exits.
    // Discovery never supplies authority for capabilities; CORE-2 enrollment
    // will replace this failure once its shared client is available.
    if let Some(state) = state_file::read_live() {
        if state.auth_mode.is_capabilities() {
            return Err(Failure::local("host_authority_unavailable"));
        }
        let base = format!("http://127.0.0.1:{}", state.port);
        if health_ok(&base, &state.token).await {
            return endpoint_from_state(state).map_err(|_| Failure::local("daemon_unavailable"));
        }
    }
    let (state, _child) = spawn_daemon(false).await.map_err(|error| {
        Failure::local(if error.contains(super::HOST_AUTHORITY_UNAVAILABLE) {
            "host_authority_unavailable"
        } else {
            "daemon_unavailable"
        })
    })?;
    endpoint_from_state(state).map_err(|_| Failure::local("host_authority_unavailable"))
}

async fn request(
    ep: &Endpoint,
    method: Method,
    path: &str,
    body: Option<&Value>,
    key: Option<&str>,
) -> Result<reqwest::Response, Failure> {
    // Retrying an ambiguous submission uses the original key and identical body.
    for attempt in 0..2 {
        let mut request = bearer(
            ep,
            client().request(method.clone(), format!("{}{path}", ep.base)),
        )
        .timeout(Duration::from_secs(if path == "/api/v1/chat" {
            180
        } else {
            15
        }));
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(key) = key {
            request = request.header("Idempotency-Key", key);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(_) if key.is_some() && attempt == 0 => continue,
            Err(_) => return Err(Failure::local("transport_error")),
        };
        if response.status().is_success() {
            return Ok(response);
        }
        #[derive(Deserialize)]
        struct Envelope {
            error: Failure,
        }
        let error = response
            .json::<Envelope>()
            .await
            .map_err(|_| Failure::local("invalid_response"))?
            .error;
        return Err(error);
    }
    Err(Failure::local("transport_error"))
}
async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, Failure> {
    response
        .json()
        .await
        .map_err(|_| Failure::local("invalid_response"))
}
async fn poll(
    ep: &Endpoint,
    mut op: Operation,
    json: bool,
    interrupt: &mut Signal,
) -> Result<Operation, Failure> {
    let mut previous = String::new();
    let mut cancelling = false;
    loop {
        if !json && previous != op.phase {
            eprintln!("iDoris {}: {}", op.action, op.phase);
            previous.clone_from(&op.phase);
        }
        match op.result.as_deref() {
            Some("succeeded") => return Ok(op),
            Some("failed" | "cancelled") => {
                return Err(Failure::local(op.error_code.as_deref().unwrap_or(
                    if op.result.as_deref() == Some("cancelled") {
                        "cancelled"
                    } else {
                        "operation_failed"
                    },
                )));
            }
            Some(_) => return Err(Failure::local("invalid_response")),
            None => {}
        }
        let operation_id = super::qs_encode(&op.operation_id);
        tokio::select! {
            _ = interrupt.recv(), if !cancelling => {
                cancelling = true;
                match request(ep, Method::POST, &format!("/api/v1/idoris/operations/{operation_id}/cancel"), None, None).await {
                    Ok(response) => op = decode(response).await?,
                    Err(error) if error.code == "not_cancellable" => {
                        if !json { eprintln!("{}", english(&error.code)); }
                    }
                    Err(error) => return Err(error),
                }
            }
            next = async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                decode(request(ep, Method::GET, &format!("/api/v1/idoris/operations/{operation_id}"), None, None).await?).await
            } => op = next?,
        }
    }
}
async fn verify(ep: &Endpoint) -> Result<Value, Failure> {
    let body =
        json!({"messages":[{"role":"user", "content":"Hi"}], "model":null, "session_id":null});
    let response = request(ep, Method::POST, "/api/v1/chat", Some(&body), None).await?;
    let record = response
        .headers()
        .get("x-idoris-record-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let body: agent24_protocol::ChatResponse = decode(response).await?;
    let (Some(record), Some(model)) = (record, body.model_id.filter(|v| !v.is_empty())) else {
        return Err(Failure::local("verify_failed"));
    };
    if body.tier.as_deref() != Some("local") {
        return Err(Failure::local("verify_failed"));
    }
    // The daemon emits this record header only after its gateway validates
    // iDoris's served-locality attestation. No evidence is inferred from /health.
    Ok(json!({"model_id":model, "record_id":record, "locality":"loopback"}))
}

async fn execute(action: IdorisAction, json_output: bool) -> Result<Value, Failure> {
    let (method, path, body) = match action {
        IdorisAction::Status(_) => (Method::GET, "/api/v1/idoris/status", None),
        IdorisAction::Candidates(_) => (Method::GET, "/api/v1/idoris/omlx/candidates", None),
        IdorisAction::Install { yes, .. } => {
            consent(
                yes,
                "Download and install the daemon's locked iDoris release. This runs downloaded software after integrity and signature checks.",
            )?;
            (
                Method::POST,
                "/api/v1/idoris/install",
                Some(json!({"consent":true})),
            )
        }
        IdorisAction::Start(_) => (Method::POST, "/api/v1/idoris/start", None),
        IdorisAction::Stop(_) => (Method::POST, "/api/v1/idoris/stop", None),
        IdorisAction::Connect(options) => {
            consent(
                options.yes,
                &format!(
                    "Connect iDoris to oMLX at {} using daily={} and deep={}. Share the local oMLX credential with iDoris, issue a data credential, start iDoris and verify normal chat. Save oMLX credential: {}. Replace existing connection: {}. Core reserved bytes: {}.",
                    options.endpoint,
                    options.daily,
                    options.deep.as_deref().unwrap_or("none"),
                    options.save_omlx_key,
                    options.replace,
                    options
                        .core_reserved_bytes
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "daemon allocation".into())
                ),
            )?;
            (
                Method::POST,
                "/api/v1/idoris/connect",
                Some(
                    json!({"endpoint":options.endpoint, "daily_model":options.daily, "deep_model":options.deep, "core_reserved_bytes":options.core_reserved_bytes, "save_omlx_key":options.save_omlx_key, "replace":options.replace,
                "consent":{"share_omlx_key":true, "daily_model":options.daily, "deep_model":options.deep}}),
                ),
            )
        }
    };
    let mutation = method != Method::GET;
    let mut interrupt =
        signal(SignalKind::interrupt()).map_err(|_| Failure::local("signal_unavailable"))?;
    let ep = host().await?;
    let result = async {
        let key = mutation.then(|| uuid::Uuid::new_v4().to_string());
        let response = request(&ep, method, path, body.as_ref(), key.as_deref()).await?;
        if mutation {
            let op = poll(&ep, decode(response).await?, json_output, &mut interrupt).await?;
            let mut value =
                serde_json::to_value(op).map_err(|_| Failure::local("invalid_response"))?;
            if path == "/api/v1/idoris/connect" {
                value["verification"] = tokio::select! {
                    result = verify(&ep) => result?,
                    _ = interrupt.recv() => return Err(Failure::local("cancelled")),
                };
            }
            Ok(value)
        } else if path == "/api/v1/idoris/status" {
            let status: Status = decode(response).await?;
            serde_json::to_value(status).map_err(|_| Failure::local("invalid_response"))
        } else {
            let candidates: Candidates = decode(response).await?;
            serde_json::to_value(candidates).map_err(|_| Failure::local("invalid_response"))
        }
    }
    .await;
    finish(ep).await;
    result
}

pub async fn run(action: IdorisAction) -> Result<(), String> {
    let json_output = match &action {
        IdorisAction::Status(o)
        | IdorisAction::Start(o)
        | IdorisAction::Stop(o)
        | IdorisAction::Candidates(o)
        | IdorisAction::Install { output: o, .. } => o.json,
        IdorisAction::Connect(c) => c.output.json,
    };
    match execute(action, json_output).await {
        Ok(value) => {
            if json_output {
                println!("{value}");
            } else {
                print_human(&value);
            }
            Ok(())
        }
        Err(error) => {
            if json_output {
                println!("{}", json!({"error":error}));
            }
            Err(format!("iDoris {}: {}", error.code, english(&error.code)))
        }
    }
}
fn print_human(value: &Value) {
    if let Some(candidates) = value["candidates"].as_array() {
        if candidates.is_empty() {
            println!("No oMLX candidates found.");
        }
        for c in candidates {
            println!(
                "{}  identity={}  authentication={}  models={}",
                c["endpoint"].as_str().unwrap_or_default(),
                c["identity"].as_str().unwrap_or_default(),
                c["authentication"].as_str().unwrap_or_default(),
                c["models"]
            );
        }
    } else if let Some(state) = value["state"].as_str() {
        println!(
            "iDoris: {state}; version={}; data port={}; admin port={}; roles={}",
            value["version"], value["data_port"], value["admin_port"], value["roles"]
        );
        if let Some(error) = value["last_error"].as_str() {
            println!("Last error: {error} — {}", english(error));
        }
        if let Some(op) = value["current_operation"].as_str() {
            println!("Current operation: {op}");
        }
    } else {
        println!(
            "iDoris {} succeeded.",
            value["action"].as_str().unwrap_or("operation")
        );
        if let Some(v) = value.get("verification") {
            println!(
                "Verified model: {}; record id: {}; locality: loopback",
                v["model_id"].as_str().unwrap_or_default(),
                v["record_id"].as_str().unwrap_or_default()
            );
            println!("Send another message with: agent24 chat \"Hi\"");
        }
    }
}
