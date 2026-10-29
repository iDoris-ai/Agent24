#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn chat_headroom_flag_and_confirmation_hint() {
    for confirmed in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let count = socket.read(&mut buf).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buf[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        let body: serde_json::Value =
                            serde_json::from_slice(&request[end + 4..]).unwrap();
                        assert_eq!(
                            body["idoris_headroom_confirmed"].as_bool().unwrap_or(false),
                            confirmed
                        );
                        assert_eq!(body["messages"][0]["content"], "probe");
                        break;
                    }
                }
            }
            let body = r#"{"error":{"code":"idoris_headroom_confirmation_required","message":"confirmation required"}}"#;
            write!(socket, "HTTP/1.1 409 Conflict\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let home = tempfile::tempdir().unwrap();
        let daemon = home.path().join("fake-agent24d");
        let ready = serde_json::json!({"type":"ready", "port":port, "token":"fixture", "version":"fixture"});
        std::fs::write(&daemon, format!("#!/bin/sh\nprintf '%s\\n' '{ready}'\n")).unwrap();
        std::fs::set_permissions(&daemon, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_agent24"));
        command
            .env("HOME", home.path())
            .env("AGENT24D_BIN", daemon)
            .args(["chat", "probe"]);
        if confirmed {
            command.arg("--confirm-headroom");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Resend with --confirm-headroom"),
            "{stderr}"
        );
        assert!(
            stderr.contains("idoris_headroom_confirmation_required"),
            "{stderr}"
        );
        server.join().unwrap();
    }
}
