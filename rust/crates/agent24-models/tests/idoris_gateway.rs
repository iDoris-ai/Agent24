#![allow(clippy::unwrap_used, clippy::expect_used)]
use agent24_models::{
    CompletionRequest, Msg, ResponseFormat, ToolSpec,
    router::{Complexity, ModelRouter, Privacy, TaskProfile},
};
use std::{
    process::Command,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

async fn endpoint(content: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    endpoint_with_redirect(content, None).await
}

async fn endpoint_with_redirect(
    content: &'static str,
    redirect: Option<String>,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let captured = captured.clone();
            let redirect = redirect.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 4096];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(bytes).unwrap();
                let health = request.starts_with("GET /health ");
                let attempt = {
                    let mut requests = captured.lock().unwrap();
                    requests.push(request);
                    requests.iter().filter(|r| r.starts_with("POST ")).count()
                };
                let mut body = if health {
                    serde_json::json!({"service":"idoris","version":"0.3.0","contract_version":"1.0.1","instance_id":"00000000-0000-4000-8000-000000000001","dev_no_key_enabled":false})
                } else {
                    serde_json::json!({"model":"fixture-model","choices":[{"message":{"role":"assistant","content":content}}],"usage":{"prompt_tokens":1,"completion_tokens":4,"total_tokens":5}})
                };
                if health {
                    match content {
                        "wrong-service" => body["service"] = serde_json::json!("unrelated"),
                        "wrong-contract" => body["contract_version"] = serde_json::json!("0"),
                        "dev-mode" => body["dev_no_key_enabled"] = serde_json::json!(true),
                        _ => {}
                    }
                } else if content == "missing-model" {
                    body.as_object_mut().unwrap().remove("model");
                }
                let busy =
                    content.starts_with("busy-") && (content != "busy-success" || attempt <= 2);
                if !health && (content == "headroom" || busy) {
                    let reason = if busy {
                        "supervisor_busy"
                    } else {
                        "headroom_confirmation_required"
                    };
                    body = serde_json::json!({"error": {"type":"local_only_unavailable", "reason_code":reason,
                        "rule_id":null, "evidence":null, "remediation":"Ask the user to confirm reserved memory headroom."}});
                }
                let body = body.to_string();
                let record = if content == "missing-record" {
                    ""
                } else {
                    "x-idoris-record-id: 00000000-0000-4000-8000-000000000002\r\n"
                };
                let record = if content == "dup-record-id" {
                    format!("{record}x-idoris-record-id: 00000000-0000-4000-8000-000000000003\r\n")
                } else {
                    record.to_owned()
                };
                let locality = match content {
                    "missing-locality" => "",
                    "remote" => "x-idoris-served-locality: remote\r\n",
                    "lan" => "x-idoris-served-locality: lan\r\n",
                    "dup-locality" => {
                        "x-idoris-served-locality: loopback\r\nx-idoris-served-locality: remote\r\n"
                    }
                    _ => "x-idoris-served-locality: loopback\r\n",
                };
                let cache = if content == "invalid-cache" {
                    "x-idoris-cached: maybe\r\n"
                } else if content == "dup-cached" {
                    "x-idoris-cached: false\r\nx-idoris-cached: false\r\n"
                } else if content == "cached-no-origin" {
                    "x-idoris-cached: true\r\n"
                } else {
                    ""
                };
                let status = if content == "redirect" && !health {
                    "307 Temporary Redirect"
                } else if !health && (content == "headroom" || busy) {
                    "503 Unavailable"
                } else if health {
                    "200 OK"
                } else {
                    match content {
                        "unavailable" => "503 Unavailable",
                        "refused" => "402 Payment Required",
                        _ => "200 OK",
                    }
                };
                let location = redirect
                    .as_ref()
                    .map(|url| format!("location: {url}/v1/chat/completions\r\n"))
                    .unwrap_or_default();
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{record}{locality}{cache}{location}content-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    (url, requests)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_gateway_is_the_only_http_target() {
    if std::env::var_os("A24_IDORIS_PROBE_CHILD").is_some() {
        let req = CompletionRequest {
            messages: vec![Msg::user("probe")],
            model: None,
            tools: vec![ToolSpec {
                name: "probe_tool".into(),
                description: "fixture".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
            response_format: Some(ResponseFormat::JsonSchema {
                name: "probe".into(),
                schema: serde_json::json!({"type":"object"}),
                strict: true,
            }),
            max_tokens: std::num::NonZeroU32::new(512),
            disable_thinking: true,
            idoris_headroom_confirmed: std::env::var("A24_IDORIS_CONFIRMED").as_deref()
                == Ok("true"),
            progress: None,
            temperature: None,
        };
        let cancel = CancellationToken::new();
        let case = std::env::var("A24_IDORIS_CASE").unwrap_or_default();
        if case == "cancelled" {
            cancel.cancel();
        }
        let result = ModelRouter::from_env()
            .complete_served(
                TaskProfile {
                    privacy: Privacy::LocalOnly,
                    complexity: Complexity::Complex,
                },
                &req,
                &cancel,
            )
            .await;
        if case == "headroom" || case.starts_with("busy-") {
            match case.as_str() {
                "headroom" => match result.unwrap_err() {
                    agent24_models::ModelError::IdorisHeadroomConfirmationRequired {
                        role,
                        remediation,
                    } => {
                        assert_eq!(role, "idoris/deep");
                        assert_eq!(
                            remediation.as_deref(),
                            Some("Ask the user to confirm reserved memory headroom.")
                        );
                    }
                    other => panic!("{other:?}"),
                },
                "busy-exhausted" => assert!(matches!(
                    result,
                    Err(agent24_models::ModelError::IdorisSupervisorBusy)
                )),
                "busy-success" => assert!(result.is_ok(), "{result:?}"),
                _ => panic!("unknown case"),
            }
            return;
        }
        if !case.is_empty() && case != "proxy" {
            assert!(result.is_err(), "case={case}");
            if ["unavailable", "disconnected"].contains(&case.as_str()) {
                assert!(matches!(
                    result,
                    Err(agent24_models::ModelError::Unavailable(_))
                ));
            }
            if case == "refused" {
                assert!(matches!(
                    result,
                    Err(agent24_models::ModelError::Rejected { status: 402, .. })
                ));
            }
            if case == "cancelled" {
                assert!(matches!(result, Err(agent24_models::ModelError::Cancelled)));
            }
            return;
        }
        let served = result.unwrap();
        assert_eq!(served.provider, "idoris-local");
        let evidence = served.idoris.as_ref().unwrap();
        assert_eq!(evidence.record_id, "00000000-0000-4000-8000-000000000002");
        assert_eq!(evidence.served_locality, "loopback");
        assert!(!evidence.cached);
        assert_eq!(evidence.origin_record_id, None);
        assert_eq!(served.response.model_id.as_deref(), Some("fixture-model"));
        assert_eq!(
            served.response.message.content.as_deref(),
            Some(if case == "proxy" { "proxy" } else { "gateway" })
        );
        return;
    }
    let (gateway, received) = endpoint("gateway").await;
    let (direct, bypassed) = endpoint("bypass").await;
    let result = tokio::task::spawn_blocking(move || {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "configured_gateway_is_the_only_http_target",
                "--nocapture",
            ])
            .env("A24_IDORIS_PROBE_CHILD", "1")
            .env_remove("A24_IDORIS_CASE")
            .env("IDORIS_URL", gateway)
            .env(
                "IDORIS_API_KEY",
                "idk_1111111111111111111111111111111111111111111111111111111111111111",
            )
            .env("OMLX_URL", &direct)
            .env("OLLAMA_URL", &direct)
            .env_remove("DEFAULT_MODEL")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let requests = received.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("POST /v1/chat/completions "))
            .count(),
        1
    );
    let post = requests
        .iter()
        .find(|r| r.starts_with("POST /v1/chat/completions "));
    assert!(
        post.is_some(),
        "gateway POSTs=0; direct requests={}; child stdout={}; stderr={}",
        bypassed.lock().unwrap().len(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert!(bypassed.lock().unwrap().is_empty());
    let post = post.unwrap();
    let (head, body) = post.split_once("\r\n\r\n").unwrap();
    let lower = head.to_ascii_lowercase();
    assert!(lower.contains("x-idoris-privacy: local_only"));
    assert!(lower.contains("x-idoris-complexity: complex"));
    assert!(lower.contains("x-idoris-request-id:"));
    assert!(lower.contains("authorization: bearer idk_"));
    assert!(
        requests
            .iter()
            .filter(|r| r.starts_with("GET /health "))
            .all(|r| !r.to_ascii_lowercase().contains("authorization:"))
    );
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["model"], "idoris/deep");
    assert_eq!(body["max_tokens"], 512);
    assert_eq!(body["tools"][0]["function"]["name"], "probe_tool");
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_evidence_and_credentials_never_fall_back() {
    for case in [
        "non-loopback",
        "userinfo",
        "path",
        "redirect",
        "wrong-service",
        "wrong-contract",
        "dev-mode",
        "missing-model",
        "invalid-cache",
        "proxy",
        "missing-locality",
        "remote",
        "lan",
        "dup-locality",
        "dup-record-id",
        "dup-cached",
        "missing-record",
        "cached-no-origin",
        "unavailable",
        "refused",
        "disconnected",
        "missing-key",
        "admin-key",
        "cancelled",
    ] {
        let (trap, trapped) = endpoint("forbidden").await;
        let (mut gateway, received) = endpoint_with_redirect(case, Some(trap.clone())).await;
        match case {
            "non-loopback" => gateway = gateway.replace("127.0.0.1", "0.0.0.0"),
            "userinfo" => gateway = gateway.replace("http://", "http://fixture:credential@"),
            "path" => gateway.push_str("/unexpected"),
            _ => {}
        }
        if case == "disconnected" {
            let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            gateway = format!("http://{}", socket.local_addr().unwrap());
            drop(socket);
        }
        let (direct, bypassed) = endpoint("bypass").await;
        let result = tokio::task::spawn_blocking(move || {
            let mut cmd = Command::new(std::env::current_exe().unwrap());
            for name in [
                "HTTP_PROXY",
                "http_proxy",
                "HTTPS_PROXY",
                "https_proxy",
                "ALL_PROXY",
                "all_proxy",
                "NO_PROXY",
                "no_proxy",
            ] {
                cmd.env_remove(name);
            }
            if case == "proxy" {
                cmd.env("HTTP_PROXY", &trap).env("ALL_PROXY", &trap);
            }
            cmd.args([
                "--exact",
                "configured_gateway_is_the_only_http_target",
                "--nocapture",
            ])
            .env("A24_IDORIS_PROBE_CHILD", "1")
            .env("A24_IDORIS_CASE", case)
            .env("IDORIS_URL", gateway)
            .env(
                "IDORIS_API_KEY",
                "idk_1111111111111111111111111111111111111111111111111111111111111111",
            )
            .env("OMLX_URL", &direct)
            .env("OLLAMA_URL", &direct)
            .env_remove("DEFAULT_MODEL");
            if case == "missing-key" {
                cmd.env_remove("IDORIS_API_KEY");
            }
            if case == "admin-key" {
                cmd.env(
                    "IDORIS_API_KEY",
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                );
            }
            cmd.output().unwrap()
        })
        .await
        .unwrap();
        assert!(
            result.status.success(),
            "{case}: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(bypassed.lock().unwrap().is_empty(), "{case}");
        assert!(
            trapped.lock().unwrap().is_empty(),
            "forbidden endpoint reached: {case}"
        );
        let requests = received.lock().unwrap();
        if [
            "missing-key",
            "admin-key",
            "cancelled",
            "disconnected",
            "non-loopback",
            "userinfo",
            "path",
        ]
        .contains(&case)
        {
            assert!(requests.is_empty(), "{case}");
        } else if ["wrong-service", "wrong-contract", "dev-mode"].contains(&case) {
            assert_eq!(requests.len(), 1, "{case}");
            assert!(requests[0].starts_with("GET /health "));
            assert!(
                !requests[0].to_ascii_lowercase().contains("authorization:"),
                "{case}"
            );
        } else {
            assert_eq!(
                requests.iter().filter(|r| r.starts_with("POST ")).count(),
                1,
                "{case}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headroom_gateway_contract() {
    for (case, confirmed, expected_calls) in [
        ("headroom", false, 1),
        ("headroom", true, 1),
        ("", true, 1),
        ("busy-success", false, 3),
        ("busy-success", true, 3),
        ("busy-exhausted", true, 4),
    ] {
        let (gateway, received) = endpoint(if case.is_empty() { "gateway" } else { case }).await;
        let (direct, bypassed) = endpoint("bypass").await;
        let output = tokio::task::spawn_blocking(move || {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "configured_gateway_is_the_only_http_target",
                    "--nocapture",
                ])
                .env("A24_IDORIS_PROBE_CHILD", "1")
                .env("A24_IDORIS_CASE", case)
                .env("A24_IDORIS_CONFIRMED", confirmed.to_string())
                .env("IDORIS_URL", gateway)
                .env("IDORIS_API_KEY", "idk_headroom_fixture")
                .env("OMLX_URL", &direct)
                .env("OLLAMA_URL", &direct)
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "case={case}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let requests = received.lock().unwrap();
        let posts: Vec<_> = requests.iter().filter(|r| r.starts_with("POST ")).collect();
        assert_eq!(posts.len(), expected_calls, "{case}");
        assert!(bypassed.lock().unwrap().is_empty(), "{case}");
        for post in &posts {
            assert_eq!(
                post.to_ascii_lowercase()
                    .contains("x-idoris-headroom-breach: confirmed\r\n"),
                confirmed,
                "{case}"
            );
        }
        let request_ids: Vec<_> = posts
            .iter()
            .map(|post| {
                post.lines()
                    .find(|line| line.starts_with("x-idoris-request-id:"))
                    .unwrap()
            })
            .collect();
        assert!(request_ids.iter().all(|id| id == &request_ids[0]));
    }
}
