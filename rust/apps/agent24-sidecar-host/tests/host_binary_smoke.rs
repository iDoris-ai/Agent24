#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_sidecar_host_protocol::{
    Event, PROTOCOL_VERSION, Reply, Request, RequestSequence, decode_event, decode_reply,
    encode_request,
};
#[cfg(unix)]
use std::collections::BTreeMap;
use std::{
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

#[cfg(unix)]
fn script_request(request_id: u64, script: String) -> Request {
    Request::Launch {
        version: PROTOCOL_VERSION,
        request_id,
        executable: "/bin/sh".to_owned(),
        cwd: "/".to_owned(),
        argv: vec!["-c".to_owned(), script],
        env: BTreeMap::new(),
    }
}

#[cfg(windows)]
fn script_request(request_id: u64, script: String) -> Request {
    let (executable, env) = {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot");
        let executable = std::path::PathBuf::from(&root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let env = ["SystemRoot", "WINDIR", "PATH", "TEMP", "TMP", "USERPROFILE"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
            .collect();
        (executable, env)
    };
    Request::Launch {
        version: PROTOCOL_VERSION,
        request_id,
        executable: executable.to_string_lossy().into_owned(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        argv: vec![
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            script,
        ],
        env,
    }
}

fn ready_frame(version: &str) -> String {
    format!(
        "{{\"type\":\"ready\",\"protocol\":1,\"port\":4312,\"token\":\"{READY_TOKEN}\",\"version\":\"{version}\"}}"
    )
}

fn ready_request(request_id: u64) -> Request {
    let ready = ready_frame("binary-smoke");
    #[cfg(unix)]
    let script =
        format!("printf '%s\\n' '{ready}'; trap '' TERM; /bin/cat >/dev/null; exec /bin/sleep 30");
    #[cfg(windows)]
    let script = format!(
        "[Console]::Out.WriteLine('{ready}'); [Console]::Out.Flush(); $null = [Console]::In.ReadToEnd(); [System.Threading.Thread]::Sleep(30000)"
    );
    script_request(request_id, script)
}

fn with_env(mut request: Request, key: &str, value: String) -> Request {
    let Request::Launch { env, .. } = &mut request else {
        unreachable!("fixture requests are always Launch");
    };
    env.insert(key.to_owned(), value);
    request
}

fn read_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("host stderr")
        .read_to_string(&mut stderr)
        .expect("read host stderr");
    stderr
}

fn assert_protocol_only(frames: impl IntoIterator<Item = Vec<u8>>) -> bool {
    let mut saw_exit = false;
    for frame in frames {
        if decode_reply(&frame).is_ok() {
            continue;
        }
        match decode_event(&frame).expect("host output must stay protocol-framed") {
            Event::Exit { .. } => saw_exit = true,
            Event::Ready { .. } => {}
        }
    }
    saw_exit
}

fn unique_marker_dir(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "agent24-sidecar-{label}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&path).expect("create marker directory");
    path
}

fn wait_for_pid(path: &std::path::Path) -> u32 {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse()
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "descendant pid marker deadline");
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    Command::new("/bin/sh")
        .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
        .status()
        .expect("probe descendant")
        .success()
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    let root = std::env::var_os("SystemRoot").expect("SystemRoot");
    let output = Command::new(
        std::path::PathBuf::from(root)
            .join("System32")
            .join("tasklist.exe"),
    )
    .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
    .output()
    .expect("probe descendant");
    output.status.success()
        && String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
}

fn wait_until_gone(pid: u32) {
    let deadline = Instant::now() + WAIT;
    loop {
        if !process_is_alive(pid) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "descendant {pid} survived host cleanup"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_marker(path: &std::path::Path, label: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        if path.exists() {
            return;
        }
        assert!(Instant::now() < deadline, "{label} marker deadline");
        thread::sleep(Duration::from_millis(20));
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

#[test]
fn active_parent_eof_starts_cleanup_before_host_exit_and_leaves_no_target() {
    let markers = unique_marker_dir("binary-parent-eof");
    let marker = markers.join("leader.pid");
    let cleanup = markers.join("cleanup.started");
    let exit_gate = markers.join("target.exit");
    let ready = ready_frame("binary-parent-eof");
    #[cfg(unix)]
    let script = format!(
        "printf '%s' \"$$\" > \"$MARKER\"; trap 'printf %s cleanup > \"$CLEANUP\"; while [ ! -f \"$EXIT_GATE\" ]; do :; done; exit 0' TERM; printf '%s\\n' '{ready}'; while :; do /bin/sleep 1; done"
    );
    #[cfg(windows)]
    let script = format!(
        "[System.IO.File]::WriteAllText($env:MARKER,$PID.ToString()); [Console]::Out.WriteLine('{ready}'); [Console]::Out.Flush(); $null = [Console]::In.ReadToEnd(); [System.IO.File]::WriteAllText($env:CLEANUP,'cleanup'); while (-not [System.IO.File]::Exists($env:EXIT_GATE)) {{ [System.Threading.Thread]::Sleep(20) }}; exit 0"
    );
    let request = with_env(
        with_env(
            with_env(
                script_request(11, script),
                "MARKER",
                marker.to_string_lossy().into_owned(),
            ),
            "CLEANUP",
            cleanup.to_string_lossy().into_owned(),
        ),
        "EXIT_GATE",
        exit_gate.to_string_lossy().into_owned(),
    );
    let mut child = spawn_host();
    let mut stdin = child.stdin.take().expect("host stdin");
    let stdout = child.stdout.take().expect("host stdout");
    let (rx, reader) = output_reader(stdout);
    let deadline = Instant::now() + WAIT;
    let mut sequence = RequestSequence::new();

    send(&mut stdin, &mut sequence, &request);
    assert!(matches!(
        decode_reply(&recv_frame(&rx, deadline)),
        Ok(Reply::Owned { request_id: 11, .. })
    ));
    assert!(matches!(
        decode_event(&recv_frame(&rx, deadline)),
        Ok(Event::Ready { .. })
    ));
    let target = wait_for_pid(&marker);
    assert!(
        process_is_alive(target),
        "target must be live before parent EOF"
    );

    drop(stdin);
    wait_for_marker(&cleanup, "parent EOF cleanup");
    assert!(
        child
            .try_wait()
            .expect("poll host after cleanup witness")
            .is_none(),
        "host exited before target cleanup witness was observed"
    );
    assert!(
        process_is_alive(target),
        "target must still be live behind the exit gate after cleanup starts"
    );
    std::fs::write(&exit_gate, b"exit\n").expect("release target exit gate");
    let status = wait_bounded(&mut child);
    wait_until_gone(target);
    reader.join().expect("stdout reader");
    let frames: Vec<_> = rx
        .try_iter()
        .map(|frame| frame.map_err(io::Error::from).expect("read trailing frame"))
        .collect();
    assert_protocol_only(frames);
    let stderr = read_stderr(&mut child);
    let _ = std::fs::remove_dir_all(&markers);
    assert!(status.success(), "host status: {status}; stderr={stderr}");
    assert!(stderr.is_empty(), "normal parent EOF must be silent");
}

#[test]
fn broken_parent_output_fails_redacted_and_leaves_no_target() {
    const SECRET: &str = "binary-broken-output-secret";
    let markers = unique_marker_dir("binary-broken-output");
    let marker = markers.join("leader.pid");
    let ready_gate = markers.join("emit-ready");
    let ready = ready_frame("binary-broken-output");
    #[cfg(unix)]
    let script = format!(
        "printf '%s' \"$$\" > \"$MARKER\"; while [ ! -f \"$READY_GATE\" ]; do :; done; printf '%s\\n' '{ready}'; trap '' TERM; exec /bin/sleep 30"
    );
    #[cfg(windows)]
    let script = format!(
        "[System.IO.File]::WriteAllText($env:MARKER,$PID.ToString()); while (-not [System.IO.File]::Exists($env:READY_GATE)) {{ [System.Threading.Thread]::Sleep(20) }}; [Console]::Out.WriteLine('{ready}'); [Console]::Out.Flush(); [System.Threading.Thread]::Sleep(30000)"
    );
    let request = with_env(
        with_env(
            with_env(
                script_request(21, script),
                "MARKER",
                marker.to_string_lossy().into_owned(),
            ),
            "READY_GATE",
            ready_gate.to_string_lossy().into_owned(),
        ),
        "FIXTURE_SECRET",
        SECRET.to_owned(),
    );
    let mut child = spawn_host();
    let mut stdin = child.stdin.take().expect("host stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("host stdout"));
    let mut sequence = RequestSequence::new();

    send(&mut stdin, &mut sequence, &request);
    let mut owned = Vec::new();
    stdout
        .read_until(b'\n', &mut owned)
        .expect("read Owned before breaking parent output");
    assert!(matches!(
        decode_reply(&owned),
        Ok(Reply::Owned { request_id: 21, .. })
    ));
    let target = wait_for_pid(&marker);
    assert!(
        process_is_alive(target),
        "target must be live before parent output is broken"
    );

    drop(stdout);
    std::fs::write(&ready_gate, b"emit\n").expect("release Ready gate");
    let status = wait_bounded(&mut child);
    drop(stdin);
    wait_until_gone(target);
    let stderr = read_stderr(&mut child);
    let _ = std::fs::remove_dir_all(&markers);
    assert!(
        !status.success(),
        "broken host output unexpectedly succeeded"
    );
    assert!(
        stderr.contains("generation_failed") || stderr.contains("session_failed"),
        "static cleanup failure stage missing: {stderr}"
    );
    assert!(!stderr.contains(SECRET), "stderr leaked launch data");
}

#[test]
fn malformed_and_polluted_ready_fail_closed_without_echoing_child_bytes() {
    const SECRET: &str = "binary-ready-secret";
    for (request_id, polluted) in [(31, false), (32, true)] {
        let ready = ready_frame("binary-adversarial");
        #[cfg(unix)]
        let script = if polluted {
            format!("printf '%s\\n%s\\n' '{ready}' '{SECRET}-pollution'; exec /bin/sleep 30")
        } else {
            format!("printf '%s\\n' '{SECRET}-not-json'; exec /bin/sleep 30")
        };
        #[cfg(windows)]
        let script = if polluted {
            format!(
                "[Console]::Out.WriteLine('{ready}'); [Console]::Out.WriteLine('{SECRET}-pollution'); [Console]::Out.Flush(); [System.Threading.Thread]::Sleep(30000)"
            )
        } else {
            format!(
                "[Console]::Out.WriteLine('{SECRET}-not-json'); [Console]::Out.Flush(); [System.Threading.Thread]::Sleep(30000)"
            )
        };

        let mut child = spawn_host();
        let mut stdin = child.stdin.take().expect("host stdin");
        let stdout = child.stdout.take().expect("host stdout");
        let (rx, reader) = output_reader(stdout);
        let deadline = Instant::now() + WAIT;
        let mut sequence = RequestSequence::new();
        send(
            &mut stdin,
            &mut sequence,
            &script_request(request_id, script),
        );
        assert!(matches!(
            decode_reply(&recv_frame(&rx, deadline)),
            Ok(Reply::Owned {
                request_id: actual,
                ..
            }) if actual == request_id
        ));

        let status = wait_bounded(&mut child);
        drop(stdin);
        reader.join().expect("stdout reader");
        let frames: Vec<_> = rx
            .try_iter()
            .map(|frame| frame.map_err(io::Error::from).expect("read trailing frame"))
            .collect();
        let rendered = frames
            .iter()
            .flat_map(|frame| frame.iter().copied())
            .collect::<Vec<_>>();
        assert_protocol_only(frames);
        let stderr = read_stderr(&mut child);
        assert!(!status.success(), "invalid Ready unexpectedly succeeded");
        assert!(
            stderr.contains("session_failed"),
            "static generation failure missing: {stderr}"
        );
        assert!(
            !String::from_utf8_lossy(&rendered).contains(SECRET) && !stderr.contains(SECRET),
            "invalid child stdout leaked through host diagnostics"
        );
    }
}

fn descendant_request(
    request_id: u64,
    marker: &std::path::Path,
    gate: &std::path::Path,
) -> Request {
    let ready = ready_frame("binary-descendant");
    #[cfg(unix)]
    let script = format!(
        "/bin/sleep 30 & descendant=$!; printf '%s' \"$descendant\" > \"$MARKER\"; printf '%s\\n' '{ready}'; while [ ! -f \"$GATE\" ]; do :; done; exit 17"
    );
    #[cfg(windows)]
    let script = format!(
        "$start = [System.Diagnostics.ProcessStartInfo]::new(); $start.FileName = [System.IO.Path]::Combine($PSHOME,'powershell.exe'); $start.Arguments = '-NoLogo -NoProfile -NonInteractive -Command \"[System.Threading.Thread]::Sleep(30000)\"'; $start.UseShellExecute = $false; $child = [System.Diagnostics.Process]::Start($start); [System.IO.File]::WriteAllText($env:MARKER,$child.Id.ToString()); [Console]::Out.WriteLine('{ready}'); [Console]::Out.Flush(); while (-not [System.IO.File]::Exists($env:GATE)) {{ [System.Threading.Thread]::Sleep(20) }}; exit 17"
    );
    let request = script_request(request_id, script);
    let request = with_env(request, "MARKER", marker.to_string_lossy().into_owned());
    with_env(request, "GATE", gate.to_string_lossy().into_owned())
}

#[test]
fn leader_exit_with_live_descendant_cleans_tree_before_parent_eof() {
    let markers = unique_marker_dir("binary-descendant");
    let marker = markers.join("descendant.pid");
    let gate = markers.join("leader.exit");
    let mut child = spawn_host();
    let mut stdin = child.stdin.take().expect("host stdin");
    let stdout = child.stdout.take().expect("host stdout");
    let (rx, reader) = output_reader(stdout);
    let deadline = Instant::now() + WAIT;
    let mut sequence = RequestSequence::new();

    send(
        &mut stdin,
        &mut sequence,
        &descendant_request(41, &marker, &gate),
    );
    assert!(matches!(
        decode_reply(&recv_frame(&rx, deadline)),
        Ok(Reply::Owned { request_id: 41, .. })
    ));
    assert!(matches!(
        decode_event(&recv_frame(&rx, deadline)),
        Ok(Event::Ready {
            ref version, ..
        }) if version == "binary-descendant"
    ));
    let descendant = wait_for_pid(&marker);
    assert!(
        process_is_alive(descendant),
        "descendant must be live before leader exit"
    );

    std::fs::write(&gate, b"exit\n").expect("release leader exit gate");
    let exit = loop {
        let frame = recv_frame(&rx, deadline);
        if let Ok(Event::Exit { protocol, code }) = decode_event(&frame) {
            break (protocol, code);
        }
        assert!(decode_reply(&frame).is_ok(), "unexpected host output frame");
    };
    assert_eq!(exit, (PROTOCOL_VERSION, Some(17)));
    wait_until_gone(descendant);

    drop(stdin);
    let status = wait_bounded(&mut child);
    reader.join().expect("stdout reader");
    let stderr = read_stderr(&mut child);
    let _ = std::fs::remove_dir_all(&markers);
    assert!(status.success(), "host status: {status}; stderr={stderr}");
    assert!(stderr.is_empty(), "descendant cleanup must be silent");
}
