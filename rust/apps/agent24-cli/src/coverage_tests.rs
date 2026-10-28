#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use crate::*;
    use serde_json::{Value, json};

    // 请求读到完整 body 后才回应，避免只验证 headers 而漏掉实际写入参数。
    fn stub(status: &str, response: &str) -> (Endpoint, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = status.to_owned();
        let response = response.to_owned();
        let hit = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let len = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .map(|s| s.trim().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (
            Endpoint {
                base: format!("http://127.0.0.1:{port}"),
                token: "test-host".into(),
                child: None,
            },
            hit,
        )
    }

    async fn comm_case(args: &[&str], line: &str, body: Option<Value>) {
        let mut argv = vec!["agent24", "comm"];
        argv.extend_from_slice(args);
        argv.push("--json");
        let Command::Comm { action, json } = Cli::try_parse_from(argv).unwrap().command else {
            panic!()
        };
        assert!(json);
        let (method, path, payload) = comm_request(action).unwrap();
        let response = r#"{"ok":true,"data":{"state":"pending"}}"#;
        let (ep, hit) = stub("200 OK", response);
        let out = execute_comm(
            &ep,
            method,
            &path,
            &mut CommRequestBody(payload),
            Duration::from_secs(5),
            json,
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            serde_json::from_str::<Value>(response).unwrap()
        );
        let raw = hit.join().unwrap();
        assert_eq!(raw.lines().next().unwrap(), line);
        assert!(
            raw.to_ascii_lowercase()
                .contains("authorization: bearer test-host")
        );
        let text = raw.split_once("\r\n\r\n").unwrap().1;
        assert_eq!(
            if text.is_empty() {
                None
            } else {
                Some(serde_json::from_str::<Value>(text).unwrap())
            },
            body
        );
    }

    macro_rules! comm_case {
        ($name:ident, $args:expr, $line:literal, $body:expr) => {
            #[tokio::test]
            async fn $name() {
                comm_case($args, $line, $body).await;
            }
        };
    }
    comm_case!(
        history,
        &["history", "--as", "a b", "--limit", "12"],
        "GET /api/v1/comm/history?as=a%20b&limit=12 HTTP/1.1",
        None
    );
    comm_case!(
        outbox,
        &["outbox", "list", "--failed-only"],
        "GET /api/v1/comm/outbox?failed_only=true HTTP/1.1",
        None
    );
    comm_case!(
        groups_list,
        &["groups", "list", "--identity", "a b"],
        "GET /api/v1/comm/groupchat?identity=a%20b HTTP/1.1",
        None
    );
    comm_case!(
        groups_create,
        &[
            "groups",
            "create",
            "小组",
            "npub1",
            "npub2",
            "--identity",
            "alice"
        ],
        "POST /api/v1/comm/groupchat HTTP/1.1",
        Some(json!({"name":"小组","invitees":["npub1","npub2"],"identity":"alice"}))
    );
    comm_case!(
        groups_members,
        &["groups", "members", "a/b"],
        "GET /api/v1/comm/groupchat/a%2Fb/members HTTP/1.1",
        None
    );
    comm_case!(
        groups_invites,
        &["groups", "invites", "g1"],
        "GET /api/v1/comm/groupchat/g1/invites HTTP/1.1",
        None
    );
    comm_case!(
        groups_messages,
        &[
            "groups",
            "messages",
            "g1",
            "--before-id",
            "a/b",
            "--limit",
            "10"
        ],
        "GET /api/v1/comm/groupchat/g1/messages?limit=10&before_id=a%2Fb HTTP/1.1",
        None
    );
    comm_case!(
        groups_send,
        &[
            "groups",
            "send",
            "g1",
            "你好",
            "--logical-id",
            "0123456789abcdef0123456789abcdef",
            "--max-retries",
            "3"
        ],
        "POST /api/v1/comm/groupchat/g1/messages HTTP/1.1",
        Some(
            json!({"body":"你好","logical_id":"0123456789abcdef0123456789abcdef","max_retries":3})
        )
    );
    comm_case!(
        groups_status,
        &["groups", "status", "g1", "l1"],
        "GET /api/v1/comm/groupchat/g1/messages/l1 HTTP/1.1",
        None
    );
    comm_case!(
        groups_retry,
        &["groups", "retry", "g1", "l1", "--recipient", "npub1"],
        "POST /api/v1/comm/groupchat/g1/messages/l1/retry HTTP/1.1",
        Some(json!({"recipient":"npub1"}))
    );
    comm_case!(
        groups_accept,
        &["groups", "accept", "i1", "--identity", "alice"],
        "POST /api/v1/comm/groupchat/invites/i1/accept HTTP/1.1",
        Some(json!({"identity":"alice"}))
    );

    #[tokio::test]
    async fn group_partial_error_keeps_delivery_report() {
        let (ep, hit) = stub(
            "502 Bad Gateway",
            r#"{"ok":false,"error":"partial","message":"failed","data":{"logical_id":"l1","recipients":[{"state":"failed"}]}}"#,
        );
        let err = execute_comm(
            &ep,
            reqwest::Method::POST,
            "/api/v1/comm/groupchat/g1/messages",
            &mut CommRequestBody(Some(json!({"body":"hi"}))),
            Duration::from_secs(5),
            true,
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("partial") && err.contains("l1") && err.contains("recipients"),
            "{err}"
        );
        hit.join().unwrap();
    }

    #[test]
    fn groups_reject_missing_arguments_and_conflicting_cursors() {
        for args in [
            vec!["groups", "create", "g1"],
            vec!["groups", "send", "g1"],
            vec!["groups", "accept"],
            vec![
                "groups",
                "messages",
                "g1",
                "--before-id",
                "b",
                "--after-id",
                "a",
            ],
        ] {
            let mut argv = vec!["agent24", "comm"];
            argv.extend(args);
            assert!(Cli::try_parse_from(argv).is_err());
        }
    }

    async fn tool_case(args: &[&str], line: &str, body: Option<Value>, status: &str) {
        let mut argv = vec!["agent24", "module-consents"];
        argv.extend_from_slice(args);
        argv.push("--json");
        let Command::ModuleConsents { action } = Cli::try_parse_from(argv).unwrap().command else {
            panic!()
        };
        let (ep, hit) = stub(status, r#"{"status":"granted"}"#);
        let out = module_tools::execute(&ep, action).await;
        if status.starts_with("200") {
            assert_eq!(out.unwrap(), r#"{"status":"granted"}"#);
        } else {
            assert!(out.unwrap_err().contains(status));
        }
        let raw = hit.join().unwrap();
        assert_eq!(raw.lines().next().unwrap(), line);
        assert!(
            raw.to_ascii_lowercase()
                .contains("authorization: bearer test-host")
        );
        let text = raw.split_once("\r\n\r\n").unwrap().1;
        assert_eq!(
            if text.is_empty() {
                None
            } else {
                Some(serde_json::from_str::<Value>(text).unwrap())
            },
            body
        );
    }

    #[tokio::test]
    async fn module_card() {
        tool_case(
            &["card", "a/b", "t?"],
            "GET /api/v1/module-consents/modules/a%2Fb/tools/t%3F/card HTTP/1.1",
            None,
            "200 OK",
        )
        .await;
    }
    #[tokio::test]
    async fn module_decide() {
        tool_case(&["decide", "m1", "t1", "granted", "--fingerprint", "fp", "--module-version", "1", "--artifact", "art", "--expires-at", "2026-11-01T00:00:00Z", "--resource", "file:f1"], "POST /api/v1/module-consents/modules/m1/tools/t1 HTTP/1.1", Some(json!({"decision":"granted","fingerprint":"fp","module_version":"1","artifact":"art","expires_at":"2026-11-01T00:00:00Z","resources":["file:f1"]})), "200 OK").await;
    }
    #[tokio::test]
    async fn module_card_preserves_forbidden() {
        tool_case(
            &["card", "m1", "t1"],
            "GET /api/v1/module-consents/modules/m1/tools/t1/card HTTP/1.1",
            None,
            "403 Forbidden",
        )
        .await;
    }
    #[test]
    fn module_decide_requires_card_binding_and_a_known_decision() {
        assert!(
            Cli::try_parse_from([
                "agent24",
                "module-consents",
                "decide",
                "m1",
                "t1",
                "granted"
            ])
            .is_err()
        );
        let mut args = vec![
            "agent24",
            "module-consents",
            "decide",
            "m1",
            "t1",
            "unknown",
            "--fingerprint",
            "fp",
            "--module-version",
            "1",
            "--artifact",
            "art",
            "--expires-at",
            "2026-11-01T00:00:00Z",
        ];
        assert!(Cli::try_parse_from(&args).is_err());
        args[5] = "denied";
        assert!(Cli::try_parse_from(&args).is_ok());
    }
}
