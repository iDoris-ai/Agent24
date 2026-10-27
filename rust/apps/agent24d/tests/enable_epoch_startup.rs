//! K1-6a-G §2.1 F3, through the real binary: each start records what it sees
//! of a module with tools, and advances its enable epoch only when os.json
//! shows it enabled after it was last seen otherwise. The call lives in
//! `serve`, so a unit test of `observe_enable_epochs` does not cover it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

fn install(home: &Path) {
    let dir = home.join(".agent24/packages/noted");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: noted\nversion: \"0.1.0\"\nroute_namespace: /api/v1/noted\n\
         event_module: noted\ndata_dir: ~/.agent24/os/noted/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: \"false\"\n  args: []\n\
         tools:\n  - operation: list-notes\n    description: lists notes\n    \
         input_schema: {type: object, properties: {}}\n    risk: read\n    \
         output_privacy: local_only\n    timeout_ms: 5000\n    inline_wait_ms: 1000\n",
    )
    .unwrap();
}

/// Start the daemon, wait for its ready line — the observation is made
/// before it — and stop it.
fn start_and_stop(home: &Path) {
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = daemon.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let ready = rx.recv_timeout(Duration::from_secs(30));
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert!(
        ready.is_ok_and(|line| !line.is_empty()),
        "no ready line within 30s"
    );
}

/// `noted`'s row: `(epoch, observed_enabled)`, once an observation wrote one.
async fn seen(home: &Path) -> Option<(i64, bool)> {
    let url = format!("sqlite://{}", home.join(".agent24/agent24.db").display());
    let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
    let row = sqlx::query_as(
        "SELECT epoch, observed_enabled FROM module_enable_epochs \
         WHERE module = 'noted' AND observed_entry IS NOT NULL",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    pool.close().await;
    row
}

fn write_os_json(home: &Path, enabled: bool) {
    std::fs::write(
        home.join(".agent24/os.json"),
        format!("{{\"domainOs\": {{\"noted\": {{\"enabled\": {enabled}}}}}}}"),
    )
    .unwrap();
}

#[tokio::test]
async fn each_start_advances_the_epoch_only_for_a_module_enabled_again() {
    let home = tempfile::tempdir().unwrap();
    install(home.path());

    start_and_stop(home.path());
    let (first, enabled) = seen(home.path()).await.expect("the start observed noted");
    assert!(enabled);
    assert!(first > 0, "first seen enabled advances the epoch");

    start_and_stop(home.path());
    assert_eq!(
        seen(home.path()).await,
        Some((first, true)),
        "a restart alone"
    );

    write_os_json(home.path(), false);
    start_and_stop(home.path());
    assert_eq!(seen(home.path()).await, Some((first, false)));

    write_os_json(home.path(), true);
    start_and_stop(home.path());
    let (again, enabled) = seen(home.path()).await.unwrap();
    assert!(enabled);
    assert!(
        again > first,
        "enabled again by hand while the daemon was down"
    );
}
