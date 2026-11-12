//! Startup scan runs after the listener is up. A skill already on disk under
//! `<data_dir>/skills` becomes visible on `GET /api/v1/skills/{id}` without
//! blocking the ready line.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/signature_command.rs"]
mod signature_command;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        #[allow(clippy::cast_possible_wrap)]
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32)
            && rustix::process::kill_process(pid, rustix::process::Signal::Term).is_ok()
        {
            let by = Instant::now() + Duration::from_secs(10);
            while self.0.try_wait().is_ok_and(|s| s.is_none()) && Instant::now() < by {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn serve_scans_the_user_directory_after_it_is_ready() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let skill = data.path().join("skills").join("weekly-report");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: weekly-report\nversion: 1.2.0\ndescription: 周报\nrequires: []\nevals: []\n---\nhello\n",
    )
    .unwrap();

    let (port, token, run) = serve(home.path(), data.path());
    let by = Instant::now() + Duration::from_secs(15);
    let mut last;
    loop {
        match get_skill(port, &token) {
            Ok((200, body)) => {
                let detail: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                assert_eq!(detail["skill_id"], "weekly-report", "{detail}");
                assert!(
                    detail["revisions"]
                        .as_array()
                        .is_some_and(|rows| rows.iter().any(|row| row["semver"] == "1.2.0")),
                    "{detail}"
                );
                break;
            }
            Ok((status, body)) => last = format!("{status} {body}"),
            Err(err) => last = err,
        }
        assert!(
            Instant::now() < by,
            "user skill was not visible after serve became ready: {last}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(run);
}

#[test]
fn rescan_route_rejects_a_path() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let home_skill = home
        .path()
        .join(".agent24")
        .join("skills")
        .join("home-only");
    std::fs::create_dir_all(&home_skill).unwrap();
    std::fs::write(
        home_skill.join("SKILL.md"),
        "---\nname: home-only\nversion: 1.0.0\ndescription: 周报\nrequires: []\nevals: []\n---\nHOMEONLYTOKEN\n",
    )
    .unwrap();
    let skill = data.path().join("skills").join("weekly-report");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: weekly-report\nversion: 1.2.0\ndescription: 周报\nrequires: []\nevals: []\n---\nhello\n",
    )
    .unwrap();
    let (port, token, run) = serve(home.path(), data.path());
    let (status, body) = post(port, &token, "/api/v1/skills/scans", "{}").expect("rescan");
    assert_eq!(status, 200, "{body}");
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(report["origin"], "user");
    assert!(
        report["skills"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["skill_id"] == "weekly-report")),
        "{report}"
    );
    assert!(!body.contains("HOMEONLYTOKEN"), "{body}");
    assert!(
        !report["skills"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["skill_id"] == "home-only")),
        "{report}"
    );
    let (status, body) = post(
        port,
        &token,
        "/api/v1/skills/scans",
        r#"{"path":"/etc/passwd"}"#,
    )
    .expect("path");
    assert_eq!(status, 400, "{body}");
    let rejected: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(rejected["error"]["code"], "invalid_request");
    drop(run);
}

fn serve(home: &Path, data: &Path) -> (u16, String, Running) {
    let mut cmd = signature_command::clear_command(env!("CARGO_BIN_EXE_agent24d"));
    cmd.env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("AGENT24_DATA_DIR", data);
    let mut child = cmd
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let run = Running(child);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let ready = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("no ready line within 30s");
    let ready: serde_json::Value = serde_json::from_str(ready.trim()).expect("the ready line");
    let port = u16::try_from(ready["port"].as_u64().unwrap()).unwrap();
    let token = ready["token"].as_str().unwrap().to_owned();
    (port, token, run)
}

fn get_skill(port: u16, token: &str) -> Result<(u16, String), String> {
    exchange(port, token, "GET", "/api/v1/skills/weekly-report", "")
}

fn post(port: u16, token: &str, path: &str, body: &str) -> Result<(u16, String), String> {
    exchange(port, token, "POST", path, body)
}

fn exchange(
    port: u16,
    token: &str,
    method: &str,
    path: &str,
    body: &str,
) -> Result<(u16, String), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|err| err.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|err| err.to_string())?;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\
         authorization: Bearer {token}\r\nconnection: close\r\ncontent-type: application/json\r\n\
         content-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|err| err.to_string())?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|err| err.to_string())?;
    let raw = String::from_utf8_lossy(&raw);
    let (head, resp) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_ref(), ""));
    let status = head
        .split("\r\n")
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|status| status.parse().ok())
        .unwrap_or(0);
    Ok((status, resp.to_owned()))
}
