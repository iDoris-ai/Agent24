//! `agent24 documents` as a process, the way the desktop runs it: what the
//! unit tests cannot see (the environment's proxy, a stdout nobody reads).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

fn agent24(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent24"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home);
    cmd
}

/// A daemon that answers one request with `body`, and sends its request line.
fn one_reply_daemon(body: &'static str) -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap() > 2 {
            line.clear();
        }
        tx.send(first.trim_end().to_owned()).unwrap();
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    (port, rx)
}

/// A proxy that counts the connections made to it and turns each away.
fn counting_proxy() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    thread::spawn(move || {
        for stream in listener.incoming() {
            seen.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut stream) = stream {
                let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n");
            }
        }
    });
    (port, count)
}

#[test]
fn the_bearer_never_goes_through_a_proxy_from_the_environment() {
    let home = tempfile::tempdir().unwrap();
    let (daemon, request_line) = one_reply_daemon(r#"{"os_version":"1"}"#);
    let (proxy, proxied) = counting_proxy();
    let proxy_url = format!("http://127.0.0.1:{proxy}");
    let mut child = agent24(home.path())
        .envs(
            ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"].map(|k| (k, proxy_url.as_str())),
        )
        .args(["documents", "request", "--host-stdin", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!(
        r#"{{"base":"http://127.0.0.1:{daemon}","token":"t0k","request":{{"op":"capabilities"}}}}"#
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""ok":true"#), "{stdout}");
    assert_eq!(
        request_line.recv().unwrap(),
        "GET /api/v1/documents/capabilities HTTP/1.1"
    );
    assert_eq!(proxied.load(Ordering::SeqCst), 0);
}

#[test]
fn a_closed_stdout_is_no_panic_and_the_exit_code_still_says_how_it_went() {
    let home = tempfile::tempdir().unwrap();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = agent24(home.path())
        .args(["documents", "request", r#"{"op":"proxy"}"#, "--json"])
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("invalid_request"), "{stderr}");
}
