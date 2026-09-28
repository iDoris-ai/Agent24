#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_sidecar_host_protocol::{
    Event, PROTOCOL_VERSION, Reply, Request, RequestSequence, decode_event, decode_reply,
    encode_request,
};
use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Write},
    process::{Child, ChildStdout, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(20);
const READY_TOKEN: &str = "tttttttttttttttttttttttttttttttt";

fn spawn_host() -> Child {
    Command::new(env!("CARGO_BIN_EXE_agent24-sidecar-host"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sidecar host")
}

fn wait_bounded(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().expect("poll host exit") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("sidecar host did not exit before deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn output_reader(
    stdout: ChildStdout,
) -> (
    mpsc::Receiver<Result<Vec<u8>, io::ErrorKind>>,
    thread::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::channel();
    let join = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut frame = Vec::new();
            match reader.read_until(b'\n', &mut frame) {
                Ok(0) => return,
                Ok(_) if tx.send(Ok(frame)).is_err() => return,
                Ok(_) => {}
                Err(error) => {
                    let _ = tx.send(Err(error.kind()));
                    return;
                }
            }
        }
    });
    (rx, join)
}

fn recv_frame(rx: &mpsc::Receiver<Result<Vec<u8>, io::ErrorKind>>, deadline: Instant) -> Vec<u8> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    rx.recv_timeout(remaining)
        .expect("host output frame before deadline")
        .map_err(io::Error::from)
        .expect("read host output")
}

fn send(stdin: &mut impl Write, sequence: &mut RequestSequence, request: &Request) {
    let frame = encode_request(request, sequence).expect("encode request");
    stdin.write_all(&frame).expect("write host request");
    stdin.flush().expect("flush host request");
}

fn reply_id(reply: &Reply) -> u64 {
    match reply {
        Reply::Owned { request_id, .. }
        | Reply::Result { request_id, .. }
        | Reply::Empty { request_id, .. }
        | Reply::Error { request_id, .. } => *request_id,
    }
}

fn recv_reply(
    rx: &mpsc::Receiver<Result<Vec<u8>, io::ErrorKind>>,
    request_id: u64,
    deadline: Instant,
) -> Reply {
    loop {
        let frame = recv_frame(rx, deadline);
        if let Ok(reply) = decode_reply(&frame) {
            assert_eq!(reply_id(&reply), request_id, "reply request id");
            return reply;
        }
        let event = decode_event(&frame).expect("output is reply or event");
        assert!(matches!(event, Event::Exit { .. }), "unexpected event");
    }
}

fn ready_request(request_id: u64) -> Request {
    let ready = format!(
        "{{\"type\":\"ready\",\"protocol\":1,\"port\":4312,\"token\":\"{READY_TOKEN}\",\"version\":\"binary-smoke\"}}"
    );
    #[cfg(unix)]
    let (executable, cwd, argv, env) = (
        "/bin/sh".to_owned(),
        "/".to_owned(),
        vec![
            "-c".to_owned(),
            format!(
                "printf '%s\\n' '{ready}'; trap '' TERM; /bin/cat >/dev/null; exec /bin/sleep 30"
            ),
        ],
        BTreeMap::new(),
    );
    #[cfg(windows)]
    let (executable, cwd, argv, env) = {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot");
        let executable = std::path::PathBuf::from(&root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let script = format!(
            "[Console]::Out.WriteLine('{ready}'); [Console]::Out.Flush(); $null = [Console]::In.ReadToEnd(); [System.Threading.Thread]::Sleep(30000)"
        );
        let env = ["SystemRoot", "WINDIR", "PATH", "TEMP", "TMP", "USERPROFILE"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
            .collect();
        (
            executable.to_string_lossy().into_owned(),
            std::env::temp_dir().to_string_lossy().into_owned(),
            vec![
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                script,
            ],
            env,
        )
    };
    Request::Launch {
        version: PROTOCOL_VERSION,
        request_id,
        executable,
        cwd,
        argv,
        env,
    }
}

#[test]
fn immediate_parent_eof_exits_successfully_without_output() {
    let mut child = spawn_host();
    drop(child.stdin.take());
    let status = wait_bounded(&mut child);
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("host stdout")
        .read_to_end(&mut stdout)
        .expect("read host stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("host stderr")
        .read_to_string(&mut stderr)
        .expect("read host stderr");
    assert!(status.success(), "host status: {status}; stderr={stderr}");
    assert!(stdout.is_empty(), "EOF must not fabricate protocol output");
    assert!(stderr.is_empty(), "successful EOF must be silent");
}

#[test]
fn invalid_input_is_redacted_and_never_replied() {
    const SECRET: &str = "binary-smoke-secret-never-echo";
    let mut child = spawn_host();
    {
        let mut stdin = child.stdin.take().expect("host stdin");
        writeln!(stdin, "{{\"type\":\"launch\",\"secret\":\"{SECRET}\"}}")
            .expect("write invalid frame");
    }
    let status = wait_bounded(&mut child);
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("host stdout")
        .read_to_end(&mut stdout)
        .expect("read host stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("host stderr")
        .read_to_string(&mut stderr)
        .expect("read host stderr");
    assert!(!status.success(), "invalid input unexpectedly succeeded");
    assert!(
        stdout.is_empty(),
        "invalid input must not fabricate a reply"
    );
    assert!(
        stderr.contains("control_failed"),
        "static failure stage missing"
    );
    assert!(!stderr.contains(SECRET), "stderr leaked invalid input");
}

#[test]
fn owned_precedes_ready_and_force_stop_reaches_confirmed_empty() {
    let mut child = spawn_host();
    let mut stdin = child.stdin.take().expect("host stdin");
    let stdout = child.stdout.take().expect("host stdout");
    let (rx, reader) = output_reader(stdout);
    let deadline = Instant::now() + WAIT;
    let mut sequence = RequestSequence::new();

    send(&mut stdin, &mut sequence, &ready_request(1));
    let owned = recv_frame(&rx, deadline);
    assert_eq!(
        decode_reply(&owned).expect("Owned reply"),
        Reply::Owned {
            version: PROTOCOL_VERSION,
            request_id: 1,
        }
    );
    let ready = recv_frame(&rx, deadline);
    assert!(matches!(
        decode_event(&ready),
        Ok(Event::Ready {
            protocol: PROTOCOL_VERSION,
            port: 4312,
            ref token,
            ref version,
        }) if token == READY_TOKEN && version == "binary-smoke"
    ));

    send(
        &mut stdin,
        &mut sequence,
        &Request::Signal {
            version: PROTOCOL_VERSION,
            request_id: 2,
            force: true,
        },
    );
    assert_eq!(
        recv_reply(&rx, 2, deadline),
        Reply::Result {
            version: PROTOCOL_VERSION,
            request_id: 2,
        }
    );

    let mut request_id = 3;
    loop {
        send(
            &mut stdin,
            &mut sequence,
            &Request::IsEmpty {
                version: PROTOCOL_VERSION,
                request_id,
            },
        );
        match recv_reply(&rx, request_id, deadline) {
            Reply::Empty { empty: true, .. } => break,
            Reply::Empty { empty: false, .. } if Instant::now() < deadline => {
                request_id += 1;
                thread::sleep(Duration::from_millis(10));
            }
            reply => panic!("unexpected IsEmpty reply: {reply:?}"),
        }
    }

    drop(stdin);
    let status = wait_bounded(&mut child);
    reader.join().expect("stdout reader");
    for frame in rx.try_iter() {
        let frame = frame.map_err(io::Error::from).expect("read trailing frame");
        assert!(
            decode_reply(&frame).is_ok() || decode_event(&frame).is_ok(),
            "trailing output is not protocol"
        );
    }
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("host stderr")
        .read_to_string(&mut stderr)
        .expect("read host stderr");
    assert!(status.success(), "host status: {status}; stderr={stderr}");
    assert!(stderr.is_empty(), "successful lifecycle must be silent");
}
